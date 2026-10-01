//! Tufo's desktop GUI: a Slint window that drives the local tufo-server.

// Release builds on Windows open no console window behind the app; the server keeps its own log.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod accents;
mod autostart;
mod backgrounds;
#[cfg(feature = "benchmark")]
mod benchmark;
mod card_art;
mod controller;
mod drops;
mod files;
mod frame;
mod i18n;
#[cfg(target_os = "macos")]
mod macos_reopen;
mod presentation;
mod render;
mod search;
mod server_process;
mod settings;
mod sound_icons;
mod tray;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
mod wayland_hidden_window;
mod window_state;

#[allow(
    clippy::panic,
    clippy::todo,
    clippy::unwrap_used,
    reason = "Slint generates this module"
)]
mod ui {
    slint::include_modules!();
}

use std::{cell::OnceCell, rc::Rc, time::Duration};

use autostart::Launch;
use card_art::Shade;
use controller::Controller;
use presentation::PageKind;
use slint::{
    CloseRequestResponse, ComponentHandle as _, Timer, TimerMode,
    winit_030::{
        CustomApplicationHandler, EventResult, WinitWindowAccessor as _,
        winit::{
            event::WindowEvent,
            event_loop::ActiveEventLoop,
            window::{Window, WindowId},
        },
    },
};
use tray::{Tray, TrayCommand};
use tufo_client::library::Background;
use tufo_local_transport::{ActivationListener, GuiInstance, LocalEndpoint};
use tufo_models::library::{CollectionId, CollectionKind};

/// The GUI only waits on RPCs, so two workers are plenty.
const RUNTIME_WORKER_THREADS: usize = 2;
/// How often the player bar's clock and the tray's Play/Pause item catch up with the player.
const PROGRESS_TICK: Duration = Duration::from_millis(250);
/// How long a launch waits on the running GUI before opening a window of its own.
const GUI_INSTANCE_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// Names the window for the desktop, which matches it to the launcher `just install` writes.
const APP_ID: &str = "dev.tufo.Tufo";

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "benchmark")]
    if std::env::args().nth(1).as_deref() == Some("benchmark") {
        return benchmark::run();
    }
    run_desktop()
}

fn run_desktop() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let settings_file = settings::SettingsFile::from_environment();
    let language = settings_file
        .as_ref()
        .and_then(|file| file.load().ok())
        .unwrap_or_default()
        .language;
    i18n::initialize(language)?;
    let launch = Launch::from_args(std::env::args_os());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(RUNTIME_WORKER_THREADS)
        .enable_all()
        .build()?;
    let activations = match runtime.block_on(claim_gui_instance()) {
        Ok(GuiInstance::First(activations)) => Some(activations),
        Ok(GuiInstance::AlreadyRunning) => {
            tracing::info!("Tufo is already running and shows its window");
            return Ok(());
        }
        Err(error) => {
            tracing::warn!(%error, "cannot claim Tufo's window; a later launch opens another");
            None
        }
    };
    let exiting_controller = Rc::new(OnceCell::new());
    select_backend(Rc::clone(&exiting_controller))?;
    let window = ui::AppWindow::new()?;
    frame::install(&window);
    i18n::select_window_language(language)?;
    let window_state_file = window_state::WindowStateFile::from_environment();
    window_state::install(&window, runtime.handle(), window_state_file);
    // The window picks this up when it first shows; setting it needs the platform the window
    // started.
    slint::set_xdg_app_id(APP_ID)?;
    let controller = Controller::new(runtime.handle().clone(), window.as_weak(), settings_file);
    let _ = exiting_controller.set(controller.clone());
    bind(&window, &controller);
    backgrounds::install(&window);
    accents::install(&window);
    render::background_groups(&window);
    render::sound_icons(&window);
    bind_art(&window);
    drops::install(controller.clone());
    let requests = WindowRequests {
        window: window.as_weak(),
        controller: controller.clone(),
    };
    let tray = show_tray(&requests);
    let hidden_in_tray = launch == Launch::AtLogin && tray.is_some();
    window.window().on_close_requested({
        let controller = controller.clone();
        let hides_to_tray = tray.is_some();
        move || {
            if !hides_to_tray {
                controller.quit();
            }
            CloseRequestResponse::HideWindow
        }
    });
    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, PROGRESS_TICK, {
        let controller = controller.clone();
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                controller.tick(&window);
            }
            if let Some(tray) = &tray {
                tray.show_playback(controller.tray_playback());
            }
        }
    });
    #[cfg(target_os = "macos")]
    macos_reopen::show_window_on_reopen(requests.clone());
    if let Some(activations) = activations {
        runtime.spawn(requests.follow_activations(activations));
    }
    if launch == Launch::AtLogin {
        controller.resume_saved_loops_once_connected();
    }
    runtime.spawn(controller.maintain_connection());
    if !hidden_in_tray {
        window.show()?;
        window_state::apply_size_limits(&window);
    }
    #[cfg(target_os = "linux")]
    if hidden_in_tray {
        slint::spawn_local(wayland_hidden_window::close_native_window(window.as_weak()))?;
    }
    // Hiding the window doesn't end the loop; quitting does, once the server has stopped.
    slint::run_event_loop_until_quit()?;
    Ok(())
}

/// Selects winit on the window's native platform: Wayland in a Wayland session.
fn select_backend(
    exiting_controller: Rc<OnceCell<Controller>>,
) -> Result<(), slint::PlatformError> {
    let backend =
        slint::BackendSelector::new().with_winit_custom_application_handler(ApplicationEvents {
            drops: drops::DropHandler::default(),
            controller: exiting_controller,
        });
    #[cfg(windows)]
    let backend = backend.with_winit_window_attributes_hook(frame::with_shadow);
    backend.select()
}

/// The tray icon, or `None` without a tray, where closing the window quits.
fn show_tray(requests: &WindowRequests) -> Option<Tray> {
    let requests = requests.clone();
    Tray::show(move |command| requests.send(command))
        .inspect_err(
            |error| tracing::warn!(%error, "no tray icon, so closing the window quits Tufo"),
        )
        .ok()
}

/// The winit events handled here: file drops, and an exit the window didn't ask for.
struct ApplicationEvents {
    drops: drops::DropHandler,
    /// Set once the window has a controller.
    controller: Rc<OnceCell<Controller>>,
}

impl CustomApplicationHandler for ApplicationEvents {
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        winit_window: Option<&Window>,
        slint_window: Option<&slint::Window>,
        event: &WindowEvent,
    ) -> EventResult {
        self.drops
            .window_event(event_loop, window_id, winit_window, slint_window, event)
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) -> EventResult {
        self.drops.about_to_wait(event_loop)
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) -> EventResult {
        if let Some(controller) = self.controller.get() {
            controller.stand_by_on_exit();
        }
        EventResult::Propagate
    }
}

async fn claim_gui_instance() -> std::io::Result<GuiInstance> {
    let endpoint = LocalEndpoint::gui_from_environment()?;
    GuiInstance::claim(&endpoint, GUI_INSTANCE_PROBE_TIMEOUT).await
}

/// Carries the tray's commands and later launches' requests to the UI thread.
#[derive(Clone)]
struct WindowRequests {
    window: slint::Weak<ui::AppWindow>,
    controller: Controller,
}

impl WindowRequests {
    fn send(&self, command: TrayCommand) {
        let requests = self.clone();
        // This fails only once the event loop has quit, when nothing is left to show or stop.
        let _ = slint::invoke_from_event_loop(move || requests.run(command));
    }

    fn run(&self, command: TrayCommand) {
        match command {
            TrayCommand::ShowWindow => {
                if let Some(window) = self.window.upgrade() {
                    show_window(&window);
                }
            }
            TrayCommand::ShowMiniPlayer => {
                if let Some(window) = self.window.upgrade() {
                    show_window(&window);
                    window
                        .global::<ui::WindowFrame>()
                        .invoke_set_mini_player(true);
                }
            }
            TrayCommand::TogglePlayPause => self.controller.toggle_play_pause(),
            TrayCommand::ToggleSoundsPaused => self.controller.toggle_sounds_paused(),
            TrayCommand::Quit => self.controller.quit(),
        }
    }

    /// Shows the window whenever Tufo is launched again, until the runtime shuts down.
    async fn follow_activations(self, mut activations: ActivationListener) {
        loop {
            if let Err(error) = activations.next_activation().await {
                tracing::warn!(%error, "stopped hearing later launches of Tufo");
                return;
            }
            self.send(TrayCommand::ShowWindow);
        }
    }
}

/// Shows the window, hidden to the tray or minimized, and raises it.
fn show_window(window: &ui::AppWindow) {
    if let Err(error) = window.show() {
        tracing::warn!(%error, "cannot show the window");
        return;
    }
    window_state::apply_size_limits(window);
    window.window().with_winit_window(|winit_window| {
        winit_window.set_minimized(false);
        winit_window.focus_window();
    });
}

fn kind(kind: ui::CollectionKind) -> CollectionKind {
    match kind {
        ui::CollectionKind::Playlist => CollectionKind::Playlist,
        ui::CollectionKind::Soundboard => CollectionKind::Soundboard,
    }
}

fn bind_art(window: &ui::AppWindow) {
    let art = window.global::<ui::Art>();
    art.on_layout_moving(backgrounds::set_layout_moving);
    art.on_card_background(background_art(window, Shade::TitleFoot));
    art.on_painting(background_art(window, Shade::None));
    art.on_backdrop({
        let window = window.as_weak();
        move |width, height, light, canopy, shade, _revision| {
            let scale = window
                .upgrade()
                .map_or(1.0, |window| window.window().scale_factor());
            backgrounds::backdrop::image(
                physical_pixels(width, scale),
                physical_pixels(height, scale),
                [light, canopy, shade],
            )
        }
    });
    art.on_picture_missing(|background, _pictures_revision| {
        presentation::background_from_fields(background.preset, &background.image_path)
            .is_some_and(|background| backgrounds::picture_missing(&background))
    });
}

/// Draws the window's `Art` callbacks' backgrounds with `shade`, at the window's scale.
fn background_art(
    window: &ui::AppWindow,
    shade: Shade,
) -> impl Fn(slint::SharedString, ui::CollectionBackground, f32, f32, f32, i32) -> slint::Image + 'static
{
    let window = window.as_weak();
    move |id, background, width, height, radius, _pictures_revision| {
        let scale = window
            .upgrade()
            .map_or(1.0, |window| window.window().scale_factor());
        let background =
            presentation::background_from_fields(background.preset, &background.image_path)
                .unwrap_or(Background::Gradient);
        backgrounds::card(
            uuid::Uuid::parse_str(&id).ok().map(CollectionId::from_uuid),
            &background,
            shade,
            physical_pixels(width, scale),
            physical_pixels(height, scale),
            physical_pixels(radius, scale),
        )
    }
}

/// A logical length in whole physical pixels at the window's `scale`, at least one.
fn physical_pixels(length: f32, scale: f32) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is rounded and clamped to a small positive pixel count first"
    )]
    let pixels = (length * scale).round().clamp(1.0, f32::from(u16::MAX)) as u32;
    pixels
}

fn page(page: ui::Page) -> PageKind {
    match page {
        ui::Page::Home => PageKind::Home,
        ui::Page::Playlists => PageKind::Playlists,
        ui::Page::Playlist => PageKind::Playlist,
        ui::Page::Soundboards => PageKind::Soundboards,
        ui::Page::Soundboard => PageKind::Soundboard,
    }
}

/// Routes each window action to the controller.
#[allow(
    clippy::too_many_lines,
    reason = "one registration per action reads best as a list"
)]
fn bind(window: &ui::AppWindow, controller: &Controller) {
    let actions = window.global::<ui::Actions>();
    // `$handle` names the controller in `$body`, so each handler owns a clone of it.
    macro_rules! on {
        ($handle:ident => $register:ident, |$($argument:ident),*| $body:expr) => {{
            let $handle = $handle.clone();
            actions.$register(move |$($argument),*| $body);
        }};
    }
    on!(controller => on_open_page, |target, id| controller.open_page(page(target), &id));
    on!(controller => on_filter_changed, |text| controller.filter(&text));
    on!(controller => on_palette_query_changed, |query| controller.search_palette(&query));
    on!(controller => on_palette_activate, |index| {
        if let Ok(index) = usize::try_from(index) {
            controller.activate_palette(index);
        }
    });
    on!(controller => on_create_collection, |collection_kind, title| {
        controller.create_collection(kind(collection_kind), &title);
    });
    on!(controller => on_follow_folder, |collection_kind| controller.follow_folder(kind(collection_kind)));
    on!(controller => on_rename_collection, |id, title| controller.rename_collection(&id, &title));
    on!(controller => on_start_new_collection, | | controller.start_new_collection());
    on!(controller => on_pick_background, |target, choice| {
        let background = presentation::background_from_fields(choice.preset, &choice.image_path);
        if let (Some(target), Some(background)) = (controller::background_target(target), background) {
            controller.pick_background(target, background);
        }
    });
    on!(controller => on_choose_background_file, |target| {
        if let Some(target) = controller::background_target(target) {
            controller.choose_background_file(target);
        }
    });
    on!(controller => on_delete_collection, |id| controller.delete_collection(&id));
    on!(controller => on_set_order, |key, descending| controller.set_order(key, descending));
    on!(controller => on_add_files, |id| controller.add_files(&id));
    on!(controller => on_play_collection, |id| controller.play_collection(&id));
    on!(controller => on_play_track, |id| controller.play_track(&id));
    on!(controller => on_add_music_layer, |id| controller.add_music_layer(&id));
    on!(controller => on_add_to_queue, |id| controller.add_to_queue(&id));
    on!(controller => on_remove_from_queue, |id| controller.remove_from_queue(&id));
    on!(controller => on_stop_music_layer, |id| controller.stop_music_layer(&id));
    on!(controller => on_set_music_layer_volume, |id, volume| controller.set_music_layer_volume(&id, volume));
    on!(controller => on_set_music_layer_looping, |id, looping| controller.set_music_layer_looping(&id, looping));
    on!(controller => on_rename_entry, |id, title| controller.rename_entry(&id, &title));
    on!(controller => on_delete_entry, |id| controller.delete_entry(&id));
    on!(controller => on_toggle_sound, |id| controller.toggle_sound(&id));
    on!(controller => on_set_sound_looping, |id, looping| controller.set_sound_looping(&id, looping));
    on!(controller => on_set_sound_volume, |id, volume| controller.set_sound_volume(&id, volume));
    on!(controller => on_update_sound, |id, title, fade_in, fade_out, icon| {
        controller.update_sound(&id, &title, fade_in, fade_out, icon);
    });
    on!(controller => on_stop_sound, |id| controller.stop_sound(&id));
    on!(controller => on_toggle_sounds_paused, | | controller.toggle_sounds_paused());
    on!(controller => on_toggle_play_pause, | | controller.toggle_play_pause());
    on!(controller => on_next_track, | | controller.next_track());
    on!(controller => on_previous_track, | | controller.previous_track());
    on!(controller => on_seek, |fraction| controller.seek(fraction));
    on!(controller => on_set_volume, |volume| controller.set_volume(volume));
    on!(controller => on_toggle_mute, | | controller.toggle_mute());
    on!(controller => on_toggle_shuffle, | | controller.toggle_shuffle());
    on!(controller => on_cycle_repeat, | | controller.cycle_repeat());
    on!(controller => on_select_output, |index| controller.select_output(index));
    on!(controller => on_refresh_outputs, | | controller.refresh_outputs());
    on!(controller => on_set_crossfade, |crossfade_ms| controller.set_crossfade(crossfade_ms));
    on!(controller => on_set_keep_playing_after_quit, |keep_playing| {
        controller.set_keep_playing_after_quit(keep_playing);
    });
    on!(controller => on_set_start_at_login, |start_at_login| {
        controller.set_start_at_login(start_at_login);
    });
    on!(controller => on_save_discord_token, |token| controller.save_discord_token(&token));
    on!(controller => on_set_discord_connection_disabled, |disabled| {
        controller.set_discord_connection_disabled(disabled);
    });
    on!(controller => on_import_kenku_library, | | controller.import_kenku_library());
}
