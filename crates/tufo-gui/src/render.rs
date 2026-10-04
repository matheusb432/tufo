//! Writes view models into the window's globals on the UI thread.

use std::time::Duration;

use slint::{ComponentHandle as _, Model as _, ModelRc, SharedString, Timer, TimerMode, VecModel};
use tufo_client::{
    desktop::TourVisibility,
    library::{Background, ImportReport, KenkuImportStatus},
};
use tufo_models::{kenku_import::KenkuImportState, library::SoundIcon};

use crate::{
    backgrounds,
    i18n::{tr, tr_args, tr_count},
    loop_editor::{LoopEditorFrame, PreviewKind},
    presentation::{
        self, CardView, CollectionPageView, CurrentOutputView, DrawerView, LibraryPageView,
        NavItemView, OutputRowView, PageKind, PlayerBarView, SongRowView, SongsPageView,
    },
    search::PaletteResult,
    settings::{DesktopSettings, QuitBehavior},
    sound_icons,
    ui::{
        AppWindow, Art, BackgroundGroup, BackgroundLabel, BackgroundPresetTile, Backgrounds,
        CollectionBackground, CollectionCard, CurrentOutput, KenkuConflictRow, KenkuImportView,
        LibraryView, LoopBadge, LoopEditorView, LoopPreview, MusicLayer, NavItem, Navigation,
        OutputRow, OutputRowKind, Page, PlayerView, PlaylistChoice, Search, SearchResult, Session,
        SongFolder, SongLoopImportRow, SongLoopImportView, SongRow, SoundChip, SoundIcons,
        SoundTile, TrackRow, VoiceBadge,
    },
};

const TOAST_DURATION: Duration = Duration::from_secs(4);
const ERROR_TOAST_DURATION: Duration = Duration::from_secs(8);

thread_local! {
    static TOAST_TIMER: Timer = Timer::default();
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

pub(crate) fn open_kenku_import(window: &slint::Weak<AppWindow>) {
    let _ =
        window.upgrade_in_event_loop(|window| window.global::<KenkuImportView>().set_open(true));
}

pub(crate) fn starting_kenku_import(window: &slint::Weak<AppWindow>) {
    let _ = window.upgrade_in_event_loop(|window| {
        let view = window.global::<KenkuImportView>();
        view.set_open(true);
        view.set_busy(true);
        view.set_choosing(false);
        view.set_can_retry(false);
        view.set_error("".into());
        view.set_summary("".into());
        view.set_percent(0);
        view.set_stage(tr("Reading Kenku FM’s library…").into());
    });
}

pub(crate) fn kenku_import_start_error(window: &slint::Weak<AppWindow>, error: String) {
    let _ = window.upgrade_in_event_loop(move |window| {
        let view = window.global::<KenkuImportView>();
        view.set_busy(false);
        view.set_error(error.into());
    });
}

pub(crate) fn kenku_import(
    window: &slint::Weak<AppWindow>,
    status: KenkuImportStatus,
    choices: Vec<bool>,
) {
    let stage = match status.state {
        KenkuImportState::Reading => tr("Reading Kenku FM’s library…"),
        KenkuImportState::Importing if status.progress.percent() < 30 => tr("Checking folders…"),
        KenkuImportState::Importing if status.progress.percent() < 85 => tr("Reading audio files…"),
        KenkuImportState::Importing => tr("Saving your import…"),
        _ => "",
    };
    let error = status
        .error_reason
        .as_deref()
        .map(presentation::kenku_error_text)
        .unwrap_or_default();
    let summary =
        if status.state == KenkuImportState::Succeeded || status.report.collections_imported > 0 {
            presentation::kenku_report_text(&status.report)
        } else {
            String::new()
        };
    let _ = window.upgrade_in_event_loop(move |window| {
        let view = window.global::<KenkuImportView>();
        view.set_available(true);
        view.set_busy(status.state.is_running());
        view.set_choosing(status.state == KenkuImportState::AwaitingChoices);
        view.set_can_retry(
            matches!(
                status.state,
                KenkuImportState::Failed | KenkuImportState::Interrupted
            ) || (status.state == KenkuImportState::Succeeded
                && !status.report.unreadable_files.is_empty()),
        );
        view.set_percent(i32::try_from(status.progress.percent()).unwrap_or(100));
        view.set_stage(stage.into());
        view.set_summary(summary.into());
        view.set_error(error.into());
        view.set_conflicts(model(
            status
                .conflicts
                .iter()
                .enumerate()
                .map(|(index, conflict)| KenkuConflictRow {
                    incoming: conflict.incoming_title.as_str().into(),
                    existing: conflict.existing_title.as_str().into(),
                    kind: match conflict.kind {
                        tufo_models::library::CollectionKind::Playlist => tr("Playlist"),
                        tufo_models::library::CollectionKind::Soundboard => tr("Soundboard"),
                    }
                    .into(),
                    overwrite: choices.get(index).copied().unwrap_or(false),
                })
                .collect(),
        ));
        view.set_unreadable(model(
            status
                .report
                .unreadable_files
                .iter()
                .map(|path| path.as_ref().to_string_lossy().into_owned().into())
                .collect(),
        ));
        view.set_unsupported(model(
            status
                .report
                .unsupported_sources
                .iter()
                .map(|address| address.as_str().into())
                .collect(),
        ));
    });
}

fn card(card: &CardView) -> CollectionCard {
    CollectionCard {
        id: card.id.to_string().into(),
        title: card.title.as_str().into(),
        count_label: card.count_label.as_str().into(),
        background: collection_background(&card.background),
        playing: card.playing,
        missing: card.missing,
    }
}

fn nav_item(item: &NavItemView) -> NavItem {
    NavItem {
        id: item.id.to_string().into(),
        title: item.title.as_str().into(),
        playing: item.playing,
        missing: item.missing,
    }
}

fn song_row(song: SongRowView) -> SongRow {
    SongRow {
        id: song.id.to_string().into(),
        title: song.title.into(),
        duration: song.duration.into(),
        date_added: song.date_added.into(),
        source: song.source.into(),
        missing: song.missing,
        removable: song.removable,
        loop_badge: loop_badge(song.loop_badge),
    }
}

fn loop_badge(badge: presentation::LoopBadgeView) -> LoopBadge {
    match badge {
        presentation::LoopBadgeView::None => LoopBadge::None,
        presentation::LoopBadgeView::Saved => LoopBadge::Saved,
        presentation::LoopBadgeView::NeedsReview => LoopBadge::NeedsReview,
    }
}

fn collection_background(background: &Background) -> CollectionBackground {
    let (preset, image_path) = presentation::background_fields(background);
    CollectionBackground {
        preset,
        image_path: image_path.into(),
    }
}

/// Lists the preset paintings in the background gallery's groups.
pub(crate) fn background_groups(window: &AppWindow) {
    let groups = presentation::PRESET_GROUPS
        .iter()
        .map(|(name, presets)| BackgroundGroup {
            name: SharedString::from(tr(name)),
            presets: model(
                presets
                    .iter()
                    .map(|preset| BackgroundPresetTile {
                        preset: i32::try_from(preset.index()).unwrap_or(-1),
                        label: background_label(&Background::Preset(*preset)),
                    })
                    .collect(),
            ),
        })
        .collect();
    window.global::<Backgrounds>().set_groups(model(groups));
}

pub(crate) fn sound_icons(window: &AppWindow) {
    let icons = window.global::<SoundIcons>();
    icons.set_images(model(sound_icons::images()));
    icons.set_labels(model(
        SoundIcon::ALL
            .iter()
            .map(|&icon| SharedString::from(sound_icons::label(icon)))
            .collect(),
    ));
}

/// Shows the background the Add dialog creates its collection with.
pub(crate) fn background_draft(window: &slint::Weak<AppWindow>, draft: &Background) {
    let (background, label) = (collection_background(draft), background_label(draft));
    let _ = window.upgrade_in_event_loop(move |window| {
        let backgrounds = window.global::<Backgrounds>();
        backgrounds.set_draft(background);
        backgrounds.set_draft_label(label);
    });
}

fn background_label(background: &Background) -> BackgroundLabel {
    let label = presentation::background_label(background);
    BackgroundLabel {
        name: label.name.into(),
        credit: label.credit.into(),
    }
}

/// Tries the pictures that failed to decode again after the library changed, drawing again
/// the cards that showed their gradient instead.
pub(crate) fn retry_missing_backgrounds(window: &slint::Weak<AppWindow>) {
    let _ = window.upgrade_in_event_loop(|window| {
        if backgrounds::retry_missing_pictures() {
            let art = window.global::<Art>();
            art.set_pictures_revision(art.get_pictures_revision().wrapping_add(1));
        }
    });
}

pub(crate) fn library(
    window: &slint::Weak<AppWindow>,
    view: LibraryPageView,
    navigation: presentation::Navigation,
) {
    let _ = window.upgrade_in_event_loop(move |window| {
        let library = window.global::<LibraryView>();
        library.set_loaded(true);
        library.set_playlists(model(view.playlists.iter().map(card).collect()));
        library.set_soundboards(model(view.soundboards.iter().map(card).collect()));
        show_rows(
            &library.get_nav_playlists(),
            view.nav_playlists.iter().map(nav_item).collect(),
            |items| library.set_nav_playlists(items),
        );
        show_rows(
            &library.get_nav_soundboards(),
            view.nav_soundboards.iter().map(nav_item).collect(),
            |items| library.set_nav_soundboards(items),
        );
        library.set_playlist_count(i32::try_from(view.playlist_count).unwrap_or(i32::MAX));
        library.set_soundboard_count(i32::try_from(view.soundboard_count).unwrap_or(i32::MAX));
        show_rows(
            &library.get_playlist_choices(),
            view.playlist_choices
                .into_iter()
                .map(|choice| PlaylistChoice {
                    id: choice.id.to_string().into(),
                    title: choice.title.into(),
                    count_label: choice.count_label.into(),
                    holds_song: choice.holds_song,
                })
                .collect(),
            |choices| library.set_playlist_choices(choices),
        );
        if let Some(songs) = view.songs {
            songs_page(&library, songs);
        }
        let page = match navigation.page {
            PageKind::Home => Page::Home,
            PageKind::Songs => Page::Songs,
            PageKind::Playlists => Page::Playlists,
            PageKind::Playlist => Page::Playlist,
            PageKind::Soundboards => Page::Soundboards,
            PageKind::Soundboard => Page::Soundboard,
        };
        let pages = window.global::<Navigation>();
        pages.set_collection_id(
            navigation
                .collection_id
                .map(|id| id.to_string())
                .unwrap_or_default()
                .into(),
        );
        // A collection that disappeared sends its page back to the grid.
        let page = match (page, &view.collection) {
            (Page::Playlist, None) => Page::Playlists,
            (Page::Soundboard, None) => Page::Soundboards,
            (page, _) => page,
        };
        pages.set_page(page);
        if let Some(collection) = view.collection {
            collection_page(&library, collection);
        }
    });
}

fn songs_page(library: &LibraryView<'_>, songs: SongsPageView) {
    library.set_songs_summary(songs.summary.into());
    library.set_songs_overflow(songs.overflow.into());
    show_rows(
        &library.get_song_folders(),
        songs
            .folders
            .into_iter()
            .map(|folder| SongFolder {
                id: folder.id.to_string().into(),
                path: folder.path.into(),
                count_label: folder.count_label.into(),
            })
            .collect(),
        |folders| library.set_song_folders(folders),
    );
    show_rows(
        &library.get_songs(),
        songs.rows.into_iter().map(song_row).collect(),
        |rows| library.set_songs(rows),
    );
}

fn collection_page(library: &LibraryView<'_>, collection: CollectionPageView) {
    library.set_collection_title(collection.title.into());
    library.set_collection_summary(collection.summary.into());
    library.set_collection_background(collection_background(&collection.background));
    library.set_collection_background_label(background_label(&collection.background));
    library.set_collection_linked(collection.linked_folder.is_some());
    library.set_collection_folder(collection.linked_folder.unwrap_or_default().into());
    library.set_collection_missing(collection.missing);
    library.set_order_key(collection.order_key);
    library.set_order_descending(collection.order_descending);
    library.set_tracks(model(
        collection
            .tracks
            .into_iter()
            .map(|track| TrackRow {
                id: track.id.to_string().into(),
                song_id: track.song_id.to_string().into(),
                title: track.title.into(),
                duration: track.duration.into(),
                date_added: track.date_added.into(),
                source: track.source.into(),
                missing: track.missing,
                current: track.current,
                playing: track.playing,
                queue_number: track
                    .queue_number
                    .and_then(|number| i32::try_from(number).ok())
                    .unwrap_or(0),
                loop_badge: loop_badge(track.loop_badge),
            })
            .collect(),
    ));
    let tiles = collection
        .sounds
        .into_iter()
        .map(|sound| SoundTile {
            id: sound.id.to_string().into(),
            title: sound.title.into(),
            icon: i32::try_from(sound.icon.index()).unwrap_or(0),
            looping: sound.looping,
            volume: sound.volume,
            fade_in_ms: i32::try_from(sound.fade_in_ms).unwrap_or(i32::MAX),
            fade_out_ms: i32::try_from(sound.fade_out_ms).unwrap_or(i32::MAX),
            playing: sound.playing,
            progress: sound.progress,
            missing: sound.missing,
        })
        .collect();
    show_rows(&library.get_sounds(), tiles, |tiles| {
        library.set_sounds(tiles);
    });
    show_rows(
        &library.get_song_search_results(),
        collection
            .song_search_results
            .into_iter()
            .map(song_row)
            .collect(),
        |rows| library.set_song_search_results(rows),
    );
}

pub(crate) fn player(
    window: &slint::Weak<AppWindow>,
    bar: PlayerBarView,
    drawer: DrawerView,
    crossfade: Duration,
) {
    let _ = window.upgrade_in_event_loop(move |window| {
        player_progress(&window, &bar);
        let view = window.global::<PlayerView>();
        view.set_has_music(bar.has_music);
        view.set_has_playback(bar.has_playback);
        view.set_entry_id(
            bar.entry_id
                .map(|id| id.to_string())
                .unwrap_or_default()
                .into(),
        );
        view.set_can_add_layer(bar.can_add_layer);
        view.set_layer_count(bar.layer_count);
        let layers = bar
            .music_layers
            .iter()
            .map(|layer| MusicLayer {
                id: layer.id.to_string().into(),
                title: layer.title.as_str().into(),
                volume: layer.volume,
                looping: layer.looping,
            })
            .collect();
        show_rows(&view.get_music_layers(), layers, |layers| {
            view.set_music_layers(layers);
        });
        view.set_title(bar.title.into());
        view.set_collection_id(
            bar.collection_id
                .map(|id| id.to_string())
                .unwrap_or_default()
                .into(),
        );
        view.set_collection_title(bar.collection_title.into());
        view.set_collection_background(collection_background(&bar.collection_background));
        view.set_playing(bar.playing);
        view.set_volume(bar.volume);
        view.set_effects_volume(bar.effects_volume);
        view.set_muted(bar.muted);
        view.set_shuffle(bar.shuffle);
        view.set_repeat(bar.repeat);
        view.set_songs_queued(bar.songs_queued);
        view.set_sounds_paused(bar.sounds_paused);
        let session = window.global::<Session>();
        session.set_outputs(model(drawer.rows.into_iter().map(output_row).collect()));
        session.set_current_output(current_output(drawer.current_output));
        session.set_voice_status(drawer.voice_status.into());
        session.set_voice_badge(voice_badge(drawer.voice_badge));
        session.set_discord_configured(drawer.discord_configured);
        session.set_discord_disabled(drawer.discord_disabled);
        session.set_crossfade_ms(i32::try_from(crossfade.as_millis()).unwrap_or(i32::MAX));
    });
}

/// Updates only what moves with time: the clock, the seek bar, the sound chips, and the rings
/// around the open soundboard's playing sounds.
pub(crate) fn player_progress(window: &AppWindow, bar: &PlayerBarView) {
    let view = window.global::<PlayerView>();
    view.set_progress(bar.progress);
    view.set_elapsed(bar.elapsed.as_str().into());
    view.set_remaining(bar.remaining.as_str().into());
    let chips = bar
        .sounds
        .iter()
        .map(|sound| SoundChip {
            id: sound.id.to_string().into(),
            title: sound.title.as_str().into(),
            progress: sound.progress,
            looping: sound.looping,
        })
        .collect::<Vec<_>>();
    show_rows(&view.get_sounds(), chips, |chips| view.set_sounds(chips));
    let tiles = window.global::<LibraryView>().get_sounds();
    for (index, mut tile) in tiles.iter().enumerate() {
        let Some(sound) = bar
            .sounds
            .iter()
            .find(|sound| tile.playing && tile.id == sound.id.to_string().as_str())
        else {
            continue;
        };
        if (tile.progress - sound.progress).abs() > f32::EPSILON {
            tile.progress = sound.progress;
            tiles.set_row_data(index, tile);
        }
    }
}

/// Updates rows in place: a new model rebuilds them, dropping hovers, presses, and drags.
fn show_rows<T: Clone + PartialEq + 'static>(
    shown: &ModelRc<T>,
    rows: Vec<T>,
    replace: impl FnOnce(ModelRc<T>),
) {
    match shown.as_any().downcast_ref::<VecModel<T>>() {
        Some(shown) if shown.row_count() == rows.len() => {
            for (index, row) in rows.into_iter().enumerate() {
                if shown.row_data(index).as_ref() != Some(&row) {
                    shown.set_row_data(index, row);
                }
            }
        }
        Some(shown) => shown.set_vec(rows),
        None => replace(model(rows)),
    }
}

fn output_row_kind(kind: presentation::OutputRowKind) -> OutputRowKind {
    match kind {
        presentation::OutputRowKind::Local => OutputRowKind::Local,
        presentation::OutputRowKind::Guild => OutputRowKind::Guild,
        presentation::OutputRowKind::Channel => OutputRowKind::Channel,
    }
}

fn output_row(row: OutputRowView) -> OutputRow {
    OutputRow {
        kind: output_row_kind(row.kind),
        label: row.label.into(),
        initial: row.initial.into(),
        selected: row.selected,
        index: row
            .index
            .and_then(|index| i32::try_from(index).ok())
            .unwrap_or(-1),
    }
}

fn current_output(output: CurrentOutputView) -> CurrentOutput {
    CurrentOutput {
        kind: output_row_kind(output.kind),
        label: output.label.into(),
        guild_name: output.guild_name.into(),
        guild_initial: output.guild_initial.into(),
    }
}

fn voice_badge(badge: presentation::VoiceBadge) -> VoiceBadge {
    match badge {
        presentation::VoiceBadge::None => VoiceBadge::None,
        presentation::VoiceBadge::Joining => VoiceBadge::Joining,
        presentation::VoiceBadge::Dropped => VoiceBadge::Dropped,
    }
}

pub(crate) fn palette(window: &slint::Weak<AppWindow>, results: &[PaletteResult]) {
    let results = results
        .iter()
        .map(|result| SearchResult {
            section: result.section.into(),
            title: result.title.as_str().into(),
            subtitle: result.subtitle.as_str().into(),
        })
        .collect::<Vec<_>>();
    let _ = window.upgrade_in_event_loop(move |window| {
        let search = window.global::<Search>();
        let last = i32::try_from(results.len()).unwrap_or(i32::MAX) - 1;
        search.set_selected(search.get_selected().clamp(0, last.max(0)));
        search.set_results(model(results));
    });
}

pub(crate) fn clear_song_search(window: &slint::Weak<AppWindow>) {
    let _ = window.upgrade_in_event_loop(|window| {
        window
            .global::<Search>()
            .set_song_search_text(SharedString::new());
    });
}

pub(crate) fn clear_filter(window: &slint::Weak<AppWindow>) {
    let _ = window.upgrade_in_event_loop(|window| {
        let search = window.global::<Search>();
        search.set_filter_open(false);
        search.set_filter_text(SharedString::new());
    });
}

pub(crate) fn connection(window: &slint::Weak<AppWindow>, connected: bool, text: String) {
    let _ = window.upgrade_in_event_loop(move |window| {
        let session = window.global::<Session>();
        session.set_connected(connected);
        session.set_connection_text(text.into());
    });
}

pub(crate) fn desktop_settings(window: &slint::Weak<AppWindow>, settings: DesktopSettings) {
    let _ = window.upgrade_in_event_loop(move |window| {
        window
            .global::<Session>()
            .set_keep_playing_after_quit(settings.quit_behavior == QuitBehavior::KeepPlaying);
        let guides = window.global::<crate::ui::GuidedTours>();
        let visibility = match settings.tours {
            TourVisibility::AlwaysShow => crate::ui::TourVisibility::AlwaysShow,
            TourVisibility::ShowUnseen => crate::ui::TourVisibility::ShowUnseen,
            TourVisibility::AlwaysHide => crate::ui::TourVisibility::AlwaysHide,
        };
        guides.set_visibility(visibility);
        if visibility == crate::ui::TourVisibility::AlwaysHide {
            guides.invoke_close();
        }
    });
}

pub(crate) fn start_at_login(window: &slint::Weak<AppWindow>, enabled: bool) {
    let _ = window.upgrade_in_event_loop(move |window| {
        window.global::<Session>().set_start_at_login(enabled);
    });
}

pub(crate) fn drop_target(window: &slint::Weak<AppWindow>, hovering: bool, label: String) {
    let _ = window.upgrade_in_event_loop(move |window| {
        let session = window.global::<Session>();
        session.set_drop_hover(hovering);
        session.set_drop_label(label.into());
    });
}

/// `None` closes the editor.
pub(crate) fn loop_editor(window: &slint::Weak<AppWindow>, frame: Option<LoopEditorFrame>) {
    let _ = window.upgrade_in_event_loop(move |window| {
        let view = window.global::<LoopEditorView>();
        let Some(frame) = frame else {
            view.set_open(false);
            return;
        };
        view.set_song_title(frame.song_title.into());
        view.set_loading(frame.loading);
        view.set_notice(frame.notice.into());
        view.set_error(frame.error.into());
        view.set_has_saved_loop(frame.has_saved_loop);
        view.set_can_save(frame.can_save);
        view.set_playback_start_text(frame.playback_start_text.into());
        view.set_loop_start_text(frame.loop_start_text.into());
        view.set_loop_end_text(frame.loop_end_text.into());
        view.set_crossfade_text(frame.crossfade_text.into());
        view.set_playhead_text(frame.playhead_text.into());
        view.set_playback_start_x(frame.playback_start_x);
        view.set_loop_start_x(frame.loop_start_x);
        view.set_loop_end_x(frame.loop_end_x);
        view.set_crossfade_start_x(frame.crossfade_start_x);
        view.set_playhead_x(frame.playhead_x);
        view.set_view_start(frame.view_start);
        view.set_view_visible(frame.view_visible);
        view.set_waveform_revision(frame.waveform_revision);
        view.set_preview(match frame.preview {
            PreviewKind::Stopped => LoopPreview::Stopped,
            PreviewKind::FromPlayhead => LoopPreview::FromPlayhead,
            PreviewKind::Seam => LoopPreview::Seam,
        });
        view.set_open(true);
    });
}

/// `None` closes the dialog.
pub(crate) fn song_loop_import(
    window: &slint::Weak<AppWindow>,
    import: impl FnOnce() -> Option<presentation::SongLoopImportView> + Send + 'static,
) {
    let _ = window.upgrade_in_event_loop(move |window| {
        let view = window.global::<SongLoopImportView>();
        let Some(import) = import() else {
            view.set_open(false);
            return;
        };
        view.set_busy(import.busy);
        view.set_summary(import.summary.into());
        view.set_rows(model(
            import
                .rows
                .into_iter()
                .map(|row| SongLoopImportRow {
                    path: row.path.into(),
                    outcome: row.outcome.into(),
                    failed: row.failed,
                    conflict: row.conflict,
                })
                .collect(),
        ));
        view.set_conflicts(i32::try_from(import.conflicts).unwrap_or(i32::MAX));
        view.set_replace(import.replace);
        view.set_can_import(import.can_import);
        view.set_open(true);
    });
}

/// Shows a message for a few seconds; errors stay longer.
pub(crate) fn toast(window: &slint::Weak<AppWindow>, text: String, is_error: bool) {
    let window = window.clone();
    let _ = window.clone().upgrade_in_event_loop(move |handle| {
        let session = handle.global::<Session>();
        session.set_toast(text.into());
        session.set_toast_is_error(is_error);
        let duration = if is_error {
            ERROR_TOAST_DURATION
        } else {
            TOAST_DURATION
        };
        TOAST_TIMER.with(|timer| {
            timer.start(TimerMode::SingleShot, duration, move || {
                clear_toast(&window);
            });
        });
    });
}

fn clear_toast(window: &slint::Weak<AppWindow>) {
    if let Some(window) = window.upgrade() {
        window.global::<Session>().set_toast(SharedString::new());
    }
}

/// What an import or addition changed, with complete pluralized sentences.
pub(crate) fn report_text(report: &ImportReport) -> String {
    let mut sentences = vec![match (report.collections_created, report.files_added) {
        (0, 0) => tr("Nothing new to add.").to_owned(),
        (0, files) => tr_count("Added {n} file.", "Added {n} files.", files),
        (collections, files) => tr_args(
            "Added {0} with {1}.",
            &[
                &tr_count("{n} collection", "{n} collections", collections),
                &tr_count("{n} file", "{n} files", files),
            ],
        ),
    }];
    if !report.unreadable_files.is_empty() {
        sentences.push(tr_count(
            "{n} file could not be read.",
            "{n} files could not be read.",
            report.unreadable_files.len(),
        ));
    }
    if !report.ignored_paths.is_empty() {
        sentences.push(tr_count(
            "Skipped {n} item that is not audio or a folder.",
            "Skipped {n} items that are not audio or folders.",
            report.ignored_paths.len(),
        ));
    }
    sentences.join(" ")
}
