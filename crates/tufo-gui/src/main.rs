//! Tufo's desktop GUI: a Slint window that drives the local tufo-server.

// Release builds on Windows open no console window behind the app; the server keeps its own log.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod accents;
mod autostart;
mod backgrounds;
#[cfg(feature = "benchmark")]
mod benchmark;
mod browser_input;
mod card_art;
mod controller;
mod drops;
mod frame;
mod guided_tours;
mod i18n;
mod loop_editor;
#[cfg(target_os = "macos")]
mod macos_reopen;
mod media_controls;
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

    #[cfg(test)]
    mod tests;
}

use std::{cell::OnceCell, rc::Rc, time::Duration};

use autostart::Launch;
use card_art::Shade;
use controller::{Controller, PaneGeometry, PanePointer};
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
use tufo_client::{
    browser::{KeyModifiers, MouseButton},
    library::Background,
};
use tufo_local_transport::{ActivationListener, GuiInstance, LocalEndpoint};
use tufo_models::library::{CollectionId, CollectionKind};

/// The GUI only waits on RPCs, so two workers are plenty.
const RUNTIME_WORKER_THREADS: usize = 2;
/// How often the player clock, tray, and desktop media session catch up with the player.
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
    guided_tours::install(&window);
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
    let media_updates = media_controls::install(requests.clone())
        .inspect_err(|error| tracing::warn!(%error, "cannot start desktop media controls"))
        .ok();
    let hidden_in_tray = launch == Launch::AtLogin && tray.is_some();
    controller.set_window_visible(!hidden_in_tray);
    bind_close_request(&window, controller.clone(), tray.is_some());
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
            if let Some(updates) = &media_updates {
                updates.publish(controller.media_state());
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

fn bind_close_request(window: &ui::AppWindow, controller: Controller, hides_to_tray: bool) {
    let weak = window.as_weak();
    window.window().on_close_requested(move || {
        if let Some(window) = weak.upgrade() {
            window.global::<ui::GuidedTours>().invoke_close();
        }
        if !hides_to_tray {
            controller.quit();
        }
        controller.set_window_visible(false);
        CloseRequestResponse::HideWindow
    });
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

/// The winit events handled here: file drops, a hidden or shown window, and an exit the window
/// didn't ask for.
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
        if let Some(controller) = self.controller.get() {
            match event {
                WindowEvent::Occluded(occluded) => controller.set_window_visible(!occluded),
                // Narrowing an f64 scale such as 1.25 to Slint's f32 loses nothing that matters.
                #[expect(clippy::cast_possible_truncation, reason = "scales are small")]
                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                    controller.set_scale_factor(*scale_factor as f32);
                }
                _ => {}
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(winit_window) = winit_window {
            frame::window_event(winit_window, event);
        }
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
                    self.controller.set_window_visible(true);
                }
            }
            TrayCommand::ShowMiniPlayer => {
                if let Some(window) = self.window.upgrade() {
                    show_window(&window);
                    self.controller.set_window_visible(true);
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
        move |width, height, corner_radius, recess_depth, light, canopy, shade, _revision| {
            let scale = window
                .upgrade()
                .map_or(1.0, |window| window.window().scale_factor());
            backgrounds::backdrop::image(
                physical_pixels(width, scale),
                physical_pixels(height, scale),
                physical_pixels_or_none(corner_radius, scale),
                physical_pixels_or_none(recess_depth, scale),
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

/// A logical length in whole physical pixels at the window's `scale`, or zero for no length.
fn physical_pixels_or_none(length: f32, scale: f32) -> u32 {
    if length > 0.0 {
        physical_pixels(length, scale)
    } else {
        0
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

fn loop_marker(marker: ui::LoopMarker) -> loop_editor::Marker {
    match marker {
        ui::LoopMarker::PlaybackStart => loop_editor::Marker::PlaybackStart,
        ui::LoopMarker::LoopStart => loop_editor::Marker::LoopStart,
        ui::LoopMarker::LoopEnd => loop_editor::Marker::LoopEnd,
    }
}

fn preview_kind(preview: ui::LoopPreview) -> Option<loop_editor::PreviewKind> {
    match preview {
        ui::LoopPreview::Stopped => None,
        ui::LoopPreview::FromPlayhead => Some(loop_editor::PreviewKind::FromPlayhead),
        ui::LoopPreview::Seam => Some(loop_editor::PreviewKind::Seam),
    }
}

fn loop_field_texts(fields: ui::LoopFieldTexts) -> loop_editor::LoopFieldTexts {
    loop_editor::LoopFieldTexts {
        playback_start: fields.playback_start.into(),
        loop_start: fields.loop_start.into(),
        loop_end: fields.loop_end.into(),
        crossfade: fields.crossfade.into(),
    }
}

fn pane_pointer(kind: ui::BrowserPointerKind, button: ui::BrowserButton) -> Option<PanePointer> {
    let button = match button {
        ui::BrowserButton::Left => Some(MouseButton::Left),
        ui::BrowserButton::Middle => Some(MouseButton::Middle),
        ui::BrowserButton::Right => Some(MouseButton::Right),
        ui::BrowserButton::None => None,
    };
    match kind {
        ui::BrowserPointerKind::Move => Some(PanePointer::Moved),
        ui::BrowserPointerKind::Leave => Some(PanePointer::Left),
        ui::BrowserPointerKind::Down => button.map(PanePointer::Pressed),
        ui::BrowserPointerKind::Up => button.map(PanePointer::Released),
    }
}

fn pane_modifiers(modifiers: &ui::BrowserModifiers) -> KeyModifiers {
    KeyModifiers {
        shift: modifiers.shift,
        control: modifiers.control,
        alt: modifiers.alt,
        meta: modifiers.meta,
    }
}

fn page(page: ui::Page) -> PageKind {
    match page {
        ui::Page::Home => PageKind::Home,
        ui::Page::Songs => PageKind::Songs,
        ui::Page::Playlists => PageKind::Playlists,
        ui::Page::Playlist => PageKind::Playlist,
        ui::Page::Soundboards => PageKind::Soundboards,
        ui::Page::Soundboard => PageKind::Soundboard,
        ui::Page::Browser => PageKind::Browser,
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
    on!(controller => on_song_search_changed, |query| controller.search_songs(&query));
    on!(controller => on_adding_song_changed, |song_id| controller.set_adding_song(&song_id));
    on!(controller => on_add_song_to_playlist, |playlist_id, song_id| {
        controller.add_song_to_playlist(&playlist_id, &song_id);
    });
    on!(controller => on_add_song_to_new_playlist, |title, song_id| {
        controller.add_song_to_new_playlist(&title, &song_id);
    });
    on!(controller => on_remove_song, |song_id| controller.remove_song(&song_id));
    on!(controller => on_unfollow_folder, |folder_id| controller.unfollow_folder(&folder_id));
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
    on!(controller => on_set_effects_volume, |volume| controller.set_effects_volume(volume));
    on!(controller => on_toggle_mute, | | controller.toggle_mute());
    on!(controller => on_toggle_shuffle, | | controller.toggle_shuffle());
    on!(controller => on_cycle_repeat, | | controller.cycle_repeat());
    on!(controller => on_select_output, |index| controller.select_output(index));
    on!(controller => on_refresh_outputs, | | controller.refresh_outputs());
    on!(controller => on_set_crossfade, |crossfade_ms| controller.set_crossfade(crossfade_ms));
    on!(controller => on_set_keep_playing_after_quit, |keep_playing| {
        controller.set_keep_playing_after_quit(keep_playing);
    });
    on!(controller => on_set_tour_visibility, |visibility| {
        controller.set_tour_visibility(visibility);
    });
    on!(controller => on_set_start_at_login, |start_at_login| {
        controller.set_start_at_login(start_at_login);
    });
    on!(controller => on_save_discord_token, |token| controller.save_discord_token(&token));
    on!(controller => on_set_discord_connection_disabled, |disabled| {
        controller.set_discord_connection_disabled(disabled);
    });
    on!(controller => on_import_kenku_library, | | controller.import_kenku_library());
    on!(controller => on_start_kenku_import, | | controller.start_kenku_import());
    on!(controller => on_retry_kenku_import, | | controller.retry_kenku_import());
    on!(controller => on_choose_kenku_conflict, | index, overwrite | controller.choose_kenku_conflict(index, overwrite));
    on!(controller => on_confirm_kenku_import, | | controller.confirm_kenku_import());
    on!(controller => on_edit_song_loop, |song_id| controller.edit_song_loop(&song_id));
    on!(controller => on_close_loop_editor, | | controller.close_loop_editor());
    on!(controller => on_loop_marker_dragged, |marker, fraction| {
        controller.drag_loop_marker(loop_marker(marker), fraction);
    });
    on!(controller => on_loop_marker_typed, |marker, text| {
        controller.type_loop_marker(loop_marker(marker), &text);
    });
    on!(controller => on_loop_crossfade_typed, |text| controller.type_loop_crossfade(&text));
    on!(controller => on_loop_playhead_set, |fraction| controller.set_loop_playhead(fraction));
    on!(controller => on_loop_playhead_step, |forward, fine| controller.step_loop_playhead(forward, fine));
    on!(controller => on_loop_marker_at_playhead, |marker| {
        controller.place_loop_marker_at_playhead(loop_marker(marker));
    });
    on!(controller => on_loop_show_marker, |marker| controller.show_loop_marker(loop_marker(marker)));
    on!(controller => on_loop_zoom, |zoom_in| controller.zoom_loop_view(zoom_in));
    on!(controller => on_loop_scroll, |fraction| controller.scroll_loop_view(fraction));
    on!(controller => on_toggle_loop_preview, |preview| match preview_kind(preview) {
        Some(kind) => controller.toggle_loop_preview(kind),
        None => controller.stop_loop_preview(),
    });
    on!(controller => on_loop_editor_tick, | | controller.tick_loop_editor());
    on!(controller => on_save_song_loop, |fields| controller.save_song_loop(&loop_field_texts(fields)));
    on!(controller => on_delete_song_loop, | | controller.delete_song_loop());
    on!(controller => on_export_song_loops, | | controller.export_song_loops());
    on!(controller => on_import_song_loops, | | controller.import_song_loops());
    on!(controller => on_set_song_loop_import_replace, |replace| controller.set_song_loop_import_replace(replace));
    on!(controller => on_confirm_song_loop_import, | | controller.confirm_song_loop_import());
    on!(controller => on_close_song_loop_import, | | controller.close_song_loop_import());
    on!(controller => on_open_browser, | | controller.open_browser());
    on!(controller => on_close_browser, | | controller.close_browser());
    on!(controller => on_browser_navigate, |typed| controller.browser_navigate(&typed));
    on!(controller => on_browser_new_tab, | | controller.browser_new_tab());
    on!(controller => on_browser_select_tab, |id| controller.browser_select_tab(id));
    on!(controller => on_browser_close_tab, |id| controller.browser_close_tab(id));
    on!(controller => on_browser_back, | | controller.browser_back());
    on!(controller => on_browser_forward, | | controller.browser_forward());
    on!(controller => on_browser_reload, | | controller.browser_reload());
    on!(controller => on_browser_stop, | | controller.browser_stop());
    let weak = window.as_weak();
    on!(controller => on_browser_view_resized, |width, height| {
        let scale_factor = weak.upgrade().map_or(1.0, |window| window.window().scale_factor());
        controller.browser_pane_resized(PaneGeometry {
            width,
            height,
            scale_factor,
        });
    });
    on!(controller => on_browser_pointer, |kind, button, x, y, modifiers| {
        if let Some(pointer) = pane_pointer(kind, button) {
            controller.browser_pointer(pointer, x, y, pane_modifiers(&modifiers));
        }
    });
    on!(controller => on_browser_scroll, |x, y, delta_x, delta_y, modifiers| {
        controller.browser_scroll((x, y), (delta_x, delta_y), pane_modifiers(&modifiers));
    });
    on!(controller => on_browser_key, |text, pressed, repeat, modifiers| {
        controller.browser_key(&text, pressed, repeat, pane_modifiers(&modifiers));
    });
    on!(controller => on_browser_focus, |focused| controller.browser_focus(focused));
    on!(controller => on_browser_paste, |text| controller.browser_paste(&text));
    on!(controller => on_set_browser_volume, |volume| controller.set_browser_volume(volume));
    on!(controller => on_toggle_browser_mute, | | controller.toggle_browser_mute());
    let loop_editor = window.global::<ui::LoopEditorView>();
    loop_editor.on_waveform({
        let controller = controller.clone();
        let window = window.as_weak();
        move |width, height, color, _revision| {
            let scale = window
                .upgrade()
                .map_or(1.0, |window| window.window().scale_factor());
            controller.loop_waveform_image(
                (
                    physical_pixels(width, scale),
                    physical_pixels(height, scale),
                ),
                color,
            )
        }
    });
}
