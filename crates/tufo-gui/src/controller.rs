//! Connects the window to tufo-server: follows the library and the player, and runs the window's
//! actions on the tokio runtime, never on the UI thread.

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant, SystemTime},
};

mod browser;
mod kenku_import;
mod song_loops;

pub(crate) use browser::{PaneGeometry, PanePointer};
use slint::ComponentHandle as _;
use tufo_client::{
    ClientError, Code, LocalEndpoint, TufoClient,
    desktop::{DesktopSettingsUpdate, TourVisibility},
    library::{
        AddFilesToCollection, AddSongsToPlaylist, Background, CollectionSource, CreateCollection,
        ImportPaths, ImportReport, LibrarySnapshot, UpdateCollection, UpdateCollectionEntry,
    },
    player::UpdatePlayerSettings,
};
use tufo_models::{
    discord::VoiceChannel,
    library::{
        AudioFileId, BackgroundPresetChooser, CollectionEntryId, CollectionId, CollectionKind,
        EntryOrder, EntryOrderKey, FadeDuration, LibraryFolderId, SoundIcon, Title, Volume,
    },
    path::{ImageFilePath, LocalPath},
    playback::{
        AUDIO_FILE_EXTENSIONS, OutputTarget,
        player_state::{CrossfadeDuration, PlayerState},
    },
};

use crate::{
    autostart,
    i18n::{tr, tr_args},
    media_controls::MediaState,
    presentation::{self, Navigation, PageKind, PageQueries},
    render,
    search::{self, PaletteResult, PaletteTarget},
    server_process,
    settings::{DesktopSettings, QuitBehavior, SettingsFile},
    tray::{TrayMusic, TrayPlayback, TraySounds},
    ui::{self, AppWindow},
};

const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// A server the window started gets this long to bind its endpoint before another start.
const SERVER_START_GRACE: Duration = Duration::from_secs(10);
/// How often the window tries a server it started, which binds its endpoint within milliseconds.
const SERVER_START_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// The server's bot logs in after the server starts, so an early listing may be unavailable.
const VOICE_CHANNEL_LIST_ATTEMPTS: usize = 80;
const VOICE_CHANNEL_LIST_RETRY_DELAY: Duration = Duration::from_millis(250);
const IMAGE_EXTENSIONS: [&str; 3] = ["jpeg", "jpg", "png"];

/// The server rejects a seek to a track's end, so the seek bar's end means this long before it,
/// and the track ends as it would on its own.
const SEEK_END_MARGIN: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub(crate) struct Controller {
    runtime: tokio::runtime::Handle,
    window: slint::Weak<AppWindow>,
    settings_file: Option<SettingsFile>,
    shared: Arc<Mutex<Shared>>,
}

#[derive(Default)]
struct Shared {
    client: Option<TufoClient>,
    snapshot: Arc<LibrarySnapshot>,
    player: PlayerState,
    voice_channels: Vec<VoiceChannel>,
    output_targets: Vec<OutputTarget>,
    navigation: Navigation,
    filter: String,
    /// The search for songs to add to the open playlist.
    song_search: String,
    /// The song the Add to playlist dialog adds.
    adding_song: Option<AudioFileId>,
    palette_results: Vec<PaletteResult>,
    /// The newest volume each slider asked for, not yet sent.
    pending_volumes: Vec<(VolumeTarget, Volume)>,
    sending_volumes: bool,
    server_started_at: Option<Instant>,
    settings: DesktopSettings,
    /// The background the Add dialog creates its collection with.
    background_draft: Background,
    resume_saved_loops_on_connect: bool,
    quitting: bool,
    kenku: kenku_import::KenkuImportSession,
    song_loops: song_loops::SongLoopSession,
    browser: browser::BrowserSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VolumeTarget {
    Player,
    Effects,
    Browser,
    Sound(CollectionEntryId),
}

/// Which collection's background a picker changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackgroundTarget {
    /// The collection the Add dialog creates.
    NewCollection,
    /// The open collection, which the Edit dialog edits.
    OpenCollection,
}

impl Controller {
    pub(crate) fn new(
        runtime: tokio::runtime::Handle,
        window: slint::Weak<AppWindow>,
        settings_file: Option<SettingsFile>,
    ) -> Self {
        Self {
            runtime,
            window,
            settings_file,
            shared: Arc::default(),
        }
    }

    /// Keeps a session with the server open until the runtime shuts down with the window,
    /// starting the server when its endpoint isn't live.
    pub(crate) async fn maintain_connection(self) {
        self.load_settings();
        loop {
            let connected = LocalEndpoint::from_environment()
                .map_err(tufo_client::ConnectError::from)
                .and_then(|endpoint| TufoClient::connect_lazy(&endpoint));
            match connected {
                Ok(client) => match client.watch_player().await {
                    Ok(states) => self.follow_server(client, states).await,
                    Err(error) => {
                        tracing::warn!(%error, "server is unreachable");
                        self.start_server();
                    }
                },
                // The server creates its runtime directory when it first starts.
                Err(tufo_client::ConnectError::LocalEndpoint(error))
                    if error.kind() == std::io::ErrorKind::NotFound =>
                {
                    self.start_server();
                }
                Err(error) => {
                    tracing::warn!(%error, "cannot reach server");
                    render::connection(
                        &self.window,
                        false,
                        tr("Cannot reach tufo-server. Check the server log.").to_owned(),
                    );
                }
            }
            let delay = if self.server_starting() {
                SERVER_START_POLL_INTERVAL
            } else {
                RECONNECT_DELAY
            };
            tokio::time::sleep(delay).await;
        }
    }

    pub(crate) fn resume_saved_loops_once_connected(&self) {
        self.lock().resume_saved_loops_on_connect = true;
    }

    /// Whether a server the window started may still be binding its endpoint.
    fn server_starting(&self) -> bool {
        self.lock()
            .server_started_at
            .is_some_and(|started| started.elapsed() <= SERVER_START_GRACE)
    }

    /// Reads the desktop settings, keeping the defaults when the file can't be read.
    fn load_settings(&self) {
        if let Some(file) = &self.settings_file {
            match file.load() {
                Ok(settings) => self.lock().settings = settings,
                Err(error) => {
                    tracing::warn!(%error, "cannot load desktop settings");
                    self.toast_error(
                        tr("Tufo could not read config.toml. Fix or remove it and restart Tufo.")
                            .to_owned(),
                    );
                }
            }
        }
        render::desktop_settings(&self.window, self.lock().settings);
        render::start_at_login(&self.window, autostart::is_enabled());
    }

    /// Starts the server unless one started moments ago and is still binding its endpoint.
    fn start_server(&self) {
        if self.server_starting() {
            render::connection(&self.window, false, tr("Starting tufo-server…").to_owned());
            return;
        }
        self.lock().server_started_at = Some(Instant::now());
        match server_process::spawn_server() {
            Ok(()) => {
                tracing::info!("started tufo-server");
                render::connection(&self.window, false, tr("Starting tufo-server…").to_owned());
            }
            Err(error) => {
                tracing::warn!(%error, "cannot start server");
                render::connection(
                    &self.window,
                    false,
                    tr("Tufo could not start its server. Check that tufo-server is installed.")
                        .to_owned(),
                );
            }
        }
    }

    async fn follow_server(&self, client: TufoClient, mut states: tufo_client::PlayerStates) {
        self.lock().client = Some(client.clone());
        tracing::info!("connected to tufo-server");
        if std::mem::take(&mut self.lock().resume_saved_loops_on_connect) {
            self.run(|client| async move { client.resume_saved_loops().await });
        }
        render::connection(
            &self.window,
            true,
            tr("Connected to tufo-server").to_owned(),
        );
        let library = self
            .runtime
            .spawn(self.clone().follow_library(client.clone()));
        let imports = self
            .runtime
            .spawn(self.clone().follow_kenku_import(client.clone()));
        let voice_channels = self.runtime.spawn(self.clone().load_voice_channels(client));
        while let Some(result) = states.next().await {
            match result {
                Ok(state) => {
                    self.lock().player = state;
                    self.render_library();
                    self.render_player();
                    self.sync_browser();
                }
                Err(error) => {
                    self.toast_error(presentation::error_text(&error));
                    break;
                }
            }
        }
        library.abort();
        imports.abort();
        voice_channels.abort();
        {
            let mut shared = self.lock();
            shared.client = None;
            shared.player = PlayerState::default();
        }
        self.render_player();
        self.sync_browser();
        render::connection(
            &self.window,
            false,
            tr("Lost tufo-server; reconnecting…").to_owned(),
        );
    }

    async fn follow_library(self, client: TufoClient) {
        let mut snapshots = match client.watch_library().await {
            Ok(snapshots) => snapshots,
            Err(error) => {
                self.toast_error(presentation::error_text(&error));
                return;
            }
        };
        while let Some(result) = snapshots.next().await {
            match result {
                Ok(mut snapshot) => {
                    presentation::localize_snapshot(&mut snapshot);
                    self.lock().snapshot = Arc::new(snapshot);
                    render::retry_missing_backgrounds(&self.window);
                    self.render_library();
                    // The player names the playing collection and shows its background.
                    self.render_player();
                    self.refresh_palette();
                }
                Err(error) => {
                    self.toast_error(presentation::error_text(&error));
                    return;
                }
            }
        }
    }

    /// Loads the voice channels, retrying while the server's bot is still logging in.
    async fn load_voice_channels(self, client: TufoClient) {
        for _ in 0..VOICE_CHANNEL_LIST_ATTEMPTS {
            match client.list_voice_channels().await {
                Ok(channels) => {
                    self.lock().voice_channels = channels;
                    self.render_player();
                    return;
                }
                // Without Discord only local output exists, which the drawer always offers.
                Err(ClientError::Status(status)) if status.code() == Code::FailedPrecondition => {
                    return;
                }
                Err(ClientError::Status(status)) if status.code() == Code::Unavailable => {
                    tokio::time::sleep(VOICE_CHANNEL_LIST_RETRY_DELAY).await;
                }
                Err(error) => {
                    self.toast_error(presentation::error_text(&error));
                    return;
                }
            }
        }
        self.toast_error(tr("Discord voice channels are still loading; press refresh.").to_owned());
    }

    // Rendering.

    pub(crate) fn render_library(&self) {
        let (view, navigation) = {
            let shared = self.lock();
            let queries = PageQueries {
                filter: &shared.filter,
                song_search: &shared.song_search,
                adding_song: shared.adding_song,
            };
            let view = presentation::library_page(
                &shared.snapshot,
                &shared.player,
                shared.navigation,
                queries,
                SystemTime::now(),
            );
            (view, shared.navigation)
        };
        render::library(&self.window, view, navigation);
    }

    pub(crate) fn render_player(&self) {
        let (bar, drawer, crossfade) = {
            let mut shared = self.lock();
            let bar = presentation::player_bar(&shared.snapshot, &shared.player, SystemTime::now());
            let (drawer, targets) = presentation::drawer(&shared.player, &shared.voice_channels);
            shared.output_targets = targets;
            (bar, drawer, shared.player.crossfade.into_inner())
        };
        render::player(&self.window, bar, drawer, crossfade);
    }

    /// Moves the player bar's clock and the sound chips' progress. Runs on the UI thread.
    pub(crate) fn tick(&self, window: &AppWindow) {
        let bar = {
            let shared = self.lock();
            let active = shared.player.music_tracks().any(|music| !music.paused)
                || !shared.player.sounds.is_empty();
            if !active {
                return;
            }
            presentation::player_bar(&shared.snapshot, &shared.player, SystemTime::now())
        };
        render::player_progress(window, &bar);
    }

    pub(crate) fn tray_playback(&self) -> TrayPlayback {
        let shared = self.lock();
        let music = match shared.player.music_tracks().next() {
            None => TrayMusic::Stopped,
            Some(music) if music.paused => TrayMusic::Paused,
            Some(_) => TrayMusic::Playing,
        };
        let sounds = match (shared.player.sounds.is_empty(), shared.player.sounds_paused) {
            (true, _) => TraySounds::Silent,
            (false, true) => TraySounds::Paused,
            (false, false) => TraySounds::Playing,
        };
        TrayPlayback { music, sounds }
    }

    pub(crate) fn media_state(&self) -> MediaState {
        let shared = self.lock();
        MediaState::from_player(&shared.snapshot, &shared.player, SystemTime::now())
    }

    fn refresh_palette(&self) {
        let query = self.window.upgrade().map(|window| {
            window
                .global::<crate::ui::Search>()
                .get_palette_query()
                .to_string()
        });
        if let Some(query) = query {
            self.search_palette(&query);
        }
    }

    // Navigation and search.

    pub(crate) fn open_page(&self, page: PageKind, collection_id: &str) {
        {
            let mut shared = self.lock();
            shared.navigation = Navigation {
                page,
                collection_id: parse_uuid(collection_id).map(CollectionId::from_uuid),
            };
            shared.filter.clear();
            shared.song_search.clear();
        }
        render::clear_filter(&self.window);
        render::clear_song_search(&self.window);
        self.render_library();
        self.sync_browser_view();
    }

    pub(crate) fn filter(&self, text: &str) {
        text.clone_into(&mut self.lock().filter);
        self.render_library();
    }

    pub(crate) fn search_songs(&self, query: &str) {
        query.clone_into(&mut self.lock().song_search);
        self.render_library();
    }

    pub(crate) fn search_palette(&self, query: &str) {
        let results = search::search(&self.lock().snapshot, query);
        render::palette(&self.window, &results);
        self.lock().palette_results = results;
    }

    pub(crate) fn activate_palette(&self, index: usize) {
        let Some(result) = self.lock().palette_results.get(index).cloned() else {
            return;
        };
        match result.target {
            PaletteTarget::Track {
                collection_id,
                entry_id,
            } => self.run(move |client| async move {
                client.play_collection(collection_id, Some(entry_id)).await
            }),
            PaletteTarget::Sound { entry_id } => {
                self.run(move |client| async move { client.play_sound(entry_id).await });
            }
            PaletteTarget::Collection { kind, id } => {
                let page = match kind {
                    CollectionKind::Playlist => PageKind::Playlist,
                    CollectionKind::Soundboard => PageKind::Soundboard,
                };
                self.open_page(page, &id.to_string());
            }
        }
    }

    // Collections.

    /// Starts the Add dialog's draft on a preset the library shows least.
    pub(crate) fn start_new_collection(&self) {
        let draft = {
            let mut shared = self.lock();
            let shown = presentation::shown_presets(&shared.snapshot);
            let preset = BackgroundPresetChooser::new(shown, fastrand::Rng::new()).choose();
            shared.background_draft = Background::Preset(preset);
            shared.background_draft.clone()
        };
        render::background_draft(&self.window, &draft);
    }

    pub(crate) fn create_collection(&self, kind: CollectionKind, title: &str) {
        let Ok(title) = Title::try_new(title.to_owned()) else {
            self.toast_error(tr("A name needs at least one character.").to_owned());
            return;
        };
        let background = self.lock().background_draft.clone();
        self.run(move |client| async move {
            client
                .create_collection(&CreateCollection {
                    kind,
                    title,
                    background,
                })
                .await
                .map(drop)
        });
    }

    pub(crate) fn follow_folder(&self, kind: CollectionKind) {
        let controller = self.clone();
        self.runtime.spawn(async move {
            let folders = rfd::AsyncFileDialog::new()
                .set_title(tr("Choose folders for Tufo to follow"))
                .pick_folders()
                .await
                .unwrap_or_default();
            let paths = folders
                .iter()
                .filter_map(|folder| LocalPath::try_new(folder.path().to_path_buf()).ok())
                .collect::<Vec<_>>();
            if !paths.is_empty() {
                controller.import(kind, paths);
            }
        });
    }

    fn import(&self, kind: CollectionKind, paths: Vec<LocalPath>) {
        self.run_report(move |client| async move {
            client.import_paths(&ImportPaths { kind, paths }).await
        });
    }

    pub(crate) fn rename_collection(&self, id: &str, title: &str) {
        let (Some(id), Ok(title)) = (collection_id(id), Title::try_new(title.to_owned())) else {
            return;
        };
        if presentation::find_collection(&self.lock().snapshot, id)
            .is_some_and(|collection| collection.title == title)
        {
            return;
        }
        self.update_collection(
            id,
            UpdateCollection {
                title: Some(title),
                ..UpdateCollection::default()
            },
        );
    }

    /// Shows `background` behind the target: the Add dialog's draft, or the open collection.
    pub(crate) fn pick_background(&self, target: BackgroundTarget, background: Background) {
        match target {
            BackgroundTarget::NewCollection => {
                self.lock().background_draft = background.clone();
                render::background_draft(&self.window, &background);
            }
            BackgroundTarget::OpenCollection => {
                let Some(id) = self.lock().navigation.collection_id else {
                    return;
                };
                self.update_collection(
                    id,
                    UpdateCollection {
                        background: Some(background),
                        ..UpdateCollection::default()
                    },
                );
            }
        }
    }

    pub(crate) fn choose_background_file(&self, target: BackgroundTarget) {
        let controller = self.clone();
        self.runtime.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(tr("Choose a background image"))
                .add_filter(tr("Images"), &IMAGE_EXTENSIONS)
                .pick_file()
                .await
            else {
                return;
            };
            controller.pick_background_file(target, file.path().to_path_buf());
        });
    }

    fn pick_background_file(&self, target: BackgroundTarget, path: PathBuf) {
        match ImageFilePath::try_new(path) {
            Ok(image) => self.pick_background(target, Background::Image(image)),
            Err(_) => self.toast_error(tr("That image's path isn't usable.").to_owned()),
        }
    }

    pub(crate) fn set_order(&self, key: i32, descending: bool) {
        let Some(id) = self.lock().navigation.collection_id else {
            return;
        };
        let key = match key {
            1 => EntryOrderKey::Duration,
            2 => EntryOrderKey::DateAdded,
            _ => EntryOrderKey::Title,
        };
        self.update_collection(
            id,
            UpdateCollection {
                order: Some(EntryOrder { key, descending }),
                ..UpdateCollection::default()
            },
        );
    }

    pub(crate) fn set_collection_pinned(&self, id: &str, pinned: bool) {
        let Some(id) = collection_id(id) else {
            return;
        };
        if presentation::find_collection(&self.lock().snapshot, id)
            .is_none_or(|collection| collection.pinned == pinned)
        {
            return;
        }
        self.update_collection(
            id,
            UpdateCollection {
                pinned: Some(pinned),
                ..UpdateCollection::default()
            },
        );
    }

    fn update_collection(&self, id: CollectionId, update: UpdateCollection) {
        self.run(move |client| async move { client.update_collection(id, &update).await });
    }

    pub(crate) fn delete_collection(&self, id: &str) {
        let Some(id) = collection_id(id) else {
            return;
        };
        let back = match self.lock().navigation.page {
            PageKind::Soundboard => PageKind::Soundboards,
            _ => PageKind::Playlists,
        };
        self.open_page(back, "");
        self.run(move |client| async move { client.delete_collection(id).await });
    }

    pub(crate) fn add_files(&self, id: &str) {
        let Some(id) = collection_id(id) else {
            return;
        };
        let controller = self.clone();
        self.runtime.spawn(async move {
            let files = rfd::AsyncFileDialog::new()
                .set_title(tr("Choose audio files"))
                .add_filter(tr("Audio"), &AUDIO_FILE_EXTENSIONS)
                .pick_files()
                .await
                .unwrap_or_default();
            let paths = files
                .iter()
                .filter_map(|file| LocalPath::try_new(file.path().to_path_buf()).ok())
                .collect::<Vec<_>>();
            if !paths.is_empty() {
                controller.add_paths(id, paths);
            }
        });
    }

    fn add_paths(&self, collection_id: CollectionId, paths: Vec<LocalPath>) {
        self.run_report(move |client| async move {
            client
                .add_files_to_collection(&AddFilesToCollection {
                    collection_id,
                    paths,
                })
                .await
        });
    }

    // Songs.

    /// Lists the hand-picked playlists for the Add to playlist dialog, marking those that
    /// already list the song; an empty id stops listing them.
    pub(crate) fn set_adding_song(&self, song_id: &str) {
        self.lock().adding_song = audio_file_id(song_id);
        self.render_library();
    }

    pub(crate) fn add_song_to_playlist(&self, playlist_id: &str, song_id: &str) {
        let (Some(playlist_id), Some(song_id)) =
            (collection_id(playlist_id), audio_file_id(song_id))
        else {
            return;
        };
        let title = presentation::find_collection(&self.lock().snapshot, playlist_id)
            .map(|playlist| playlist.title.as_str().to_owned())
            .unwrap_or_default();
        let Some(client) = self.connected_client() else {
            return;
        };
        let controller = self.clone();
        self.runtime.spawn(async move {
            let request = AddSongsToPlaylist {
                playlist_id,
                audio_file_ids: vec![song_id],
            };
            match client.add_songs_to_playlist(&request).await {
                Ok(0) => controller.toast(tr_args("{0} already has this song.", &[&title])),
                Ok(_) => controller.toast(tr_args("Added to {0}.", &[&title])),
                Err(error) => controller.toast_error(presentation::error_text(&error)),
            }
        });
    }

    /// Creates a playlist on a preset the library shows least, then adds the song to it.
    pub(crate) fn add_song_to_new_playlist(&self, title: &str, song_id: &str) {
        let Some(song_id) = audio_file_id(song_id) else {
            return;
        };
        let Ok(title) = Title::try_new(title.to_owned()) else {
            self.toast_error(tr("A name needs at least one character.").to_owned());
            return;
        };
        let Some(client) = self.connected_client() else {
            return;
        };
        let shown = presentation::shown_presets(&self.lock().snapshot);
        let preset = BackgroundPresetChooser::new(shown, fastrand::Rng::new()).choose();
        let controller = self.clone();
        self.runtime.spawn(async move {
            let created = async {
                let playlist_id = client
                    .create_collection(&CreateCollection {
                        kind: CollectionKind::Playlist,
                        title: title.clone(),
                        background: Background::Preset(preset),
                    })
                    .await?;
                let request = AddSongsToPlaylist {
                    playlist_id,
                    audio_file_ids: vec![song_id],
                };
                client.add_songs_to_playlist(&request).await
            };
            match created.await {
                Ok(_) => controller.toast(tr_args("Added to {0}.", &[&title])),
                Err(error) => controller.toast_error(presentation::error_text(&error)),
            }
        });
    }

    pub(crate) fn remove_song(&self, song_id: &str) {
        if let Some(id) = audio_file_id(song_id) {
            self.run(move |client| async move { client.delete_song(id).await });
        }
    }

    pub(crate) fn unfollow_folder(&self, folder_id: &str) {
        if let Some(id) = parse_uuid(folder_id).map(LibraryFolderId::from_uuid) {
            self.run(move |client| async move { client.delete_library_folder(id).await });
        }
    }

    // Drops.

    pub(crate) fn show_drop_target(&self, hovering: bool) {
        if self.background_drop_target().is_some() {
            render::drop_target(
                &self.window,
                hovering,
                tr("Drop an image to show it behind the collection").to_owned(),
            );
            return;
        }
        let navigation = self.lock().navigation;
        let label = match navigation.page {
            PageKind::Songs | PageKind::Playlists => {
                tr("Drop folders of music to follow them").to_owned()
            }
            PageKind::Soundboards => tr("Drop folders of sounds to follow them").to_owned(),
            PageKind::Playlist | PageKind::Soundboard if self.open_collection_is_linked() => {
                tr("This collection follows its folder; add files there").to_owned()
            }
            PageKind::Playlist | PageKind::Soundboard => tr("Drop the files here…").to_owned(),
            PageKind::Home | PageKind::Browser => {
                tr("Open Playlists or Soundboards to drop folders").to_owned()
            }
        };
        render::drop_target(&self.window, hovering, label);
    }

    fn open_collection_is_linked(&self) -> bool {
        let shared = self.lock();
        shared
            .navigation
            .collection_id
            .and_then(|id| presentation::find_collection(&shared.snapshot, id))
            .is_some_and(|collection| matches!(collection.source, CollectionSource::Linked { .. }))
    }

    /// The picker an image dropped now goes to. Reads the window, so it runs on the UI thread.
    fn background_drop_target(&self) -> Option<BackgroundTarget> {
        background_target(self.window.upgrade()?.invoke_background_drop_target())
    }

    /// Imports dropped folders on a grid, adds dropped files to the open manual collection, or,
    /// while a dialog shows the background picker, takes a dropped image as the background.
    pub(crate) fn drop_paths(&self, paths: Vec<PathBuf>) {
        if let Some(target) = self.background_drop_target() {
            match paths
                .into_iter()
                .find(|path| has_extension(path, &IMAGE_EXTENSIONS))
            {
                Some(image) => self.pick_background_file(target, image),
                None => self.toast_error(tr("Drop a JPEG or PNG image to use it.").to_owned()),
            }
            return;
        }
        let paths = paths
            .into_iter()
            .filter_map(|path| LocalPath::try_new(path).ok())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return;
        }
        let navigation = self.lock().navigation;
        match (navigation.page, navigation.collection_id) {
            (PageKind::Songs | PageKind::Playlists, _) => {
                self.import(CollectionKind::Playlist, paths);
            }
            (PageKind::Soundboards, _) => self.import(CollectionKind::Soundboard, paths),
            (PageKind::Playlist | PageKind::Soundboard, Some(id)) => {
                if self.open_collection_is_linked() {
                    self.toast_error(
                        tr("This collection follows its folder. Copy the files into that folder instead.").to_owned(),
                    );
                } else {
                    self.add_paths(id, paths);
                }
            }
            _ => self.toast(tr("Open Playlists or Soundboards to drop folders there.").to_owned()),
        }
    }

    // Entries.

    pub(crate) fn play_collection(&self, id: &str) {
        let Some(id) = collection_id(id) else {
            return;
        };
        let active = {
            let shared = self.lock();
            shared
                .player
                .music_tracks()
                .any(|music| music.collection_id == id)
        };
        if active {
            self.toggle_play_pause();
        } else {
            self.run(move |client| async move { client.play_collection(id, None).await });
        }
    }

    pub(crate) fn play_track(&self, entry_id: &str) {
        let Some(entry_id) = entry_id_of(entry_id) else {
            return;
        };
        let (playing, collection) = {
            let shared = self.lock();
            let collection = presentation::find_entry(&shared.snapshot, entry_id)
                .map(|(collection, _)| collection.id);
            let playing = shared
                .player
                .music_tracks()
                .any(|music| music.entry.entry_id == entry_id);
            (playing, collection)
        };
        match (playing, collection) {
            (true, _) => self.toggle_play_pause(),
            (false, Some(collection)) => self.run(move |client| async move {
                client.play_collection(collection, Some(entry_id)).await
            }),
            (false, None) => {}
        }
    }

    pub(crate) fn add_music_layer(&self, id: &str) {
        let Some(id) = entry_id_of(id) else {
            return;
        };
        self.run(move |client| async move { client.add_music_layer(id).await });
    }

    pub(crate) fn add_to_queue(&self, id: &str) {
        let Some(id) = entry_id_of(id) else {
            return;
        };
        self.run(move |client| async move { client.add_to_queue(id).await });
    }

    pub(crate) fn remove_from_queue(&self, id: &str) {
        let Some(id) = entry_id_of(id) else {
            return;
        };
        self.run(move |client| async move { client.remove_from_queue(id).await });
    }

    pub(crate) fn stop_music_layer(&self, id: &str) {
        let Some(id) = entry_id_of(id) else {
            return;
        };
        self.run(move |client| async move { client.stop_music_layer(id).await });
    }

    pub(crate) fn set_music_layer_volume(&self, id: &str, volume: f32) {
        let (Some(id), Ok(volume)) = (entry_id_of(id), Volume::try_new(volume)) else {
            return;
        };
        let update = tufo_client::player::UpdateMusicLayer {
            volume: Some(volume),
            looping: None,
        };
        self.run(move |client| async move { client.update_music_layer(id, update).await });
    }

    pub(crate) fn set_music_layer_looping(&self, id: &str, looping: bool) {
        let Some(id) = entry_id_of(id) else {
            return;
        };
        let update = tufo_client::player::UpdateMusicLayer {
            volume: None,
            looping: Some(looping),
        };
        self.run(move |client| async move { client.update_music_layer(id, update).await });
    }

    pub(crate) fn rename_entry(&self, id: &str, title: &str) {
        let (Some(id), Ok(title)) = (entry_id_of(id), Title::try_new(title.to_owned())) else {
            return;
        };
        if presentation::find_entry(&self.lock().snapshot, id)
            .is_some_and(|(_, entry)| entry.title == title)
        {
            return;
        }
        self.update_entry(
            id,
            UpdateCollectionEntry {
                title: Some(title),
                ..UpdateCollectionEntry::default()
            },
        );
    }

    pub(crate) fn delete_entry(&self, id: &str) {
        if let Some(id) = entry_id_of(id) {
            self.run(move |client| async move { client.delete_collection_entry(id).await });
        }
    }

    pub(crate) fn toggle_sound(&self, id: &str) {
        let Some(id) = entry_id_of(id) else {
            return;
        };
        let playing = self
            .lock()
            .player
            .sounds
            .iter()
            .any(|sound| sound.entry_id == id);
        if playing {
            self.run(move |client| async move { client.stop_sound(id).await });
        } else {
            self.run(move |client| async move { client.play_sound(id).await });
        }
    }

    pub(crate) fn stop_sound(&self, id: &str) {
        if let Some(id) = entry_id_of(id) {
            self.run(move |client| async move { client.stop_sound(id).await });
        }
    }

    fn change_sound(
        &self,
        id: &str,
        title: Option<Title>,
        icon: Option<SoundIcon>,
        change: impl FnOnce(&mut tufo_models::library::SoundSettings),
    ) {
        let Some(id) = entry_id_of(id) else {
            return;
        };
        if let Some(update) = self.sound_update(id, title, icon, change) {
            self.update_entry(id, update);
        }
    }

    /// Changes one sound setting, keeping the others as the library has them.
    fn sound_update(
        &self,
        id: CollectionEntryId,
        title: Option<Title>,
        icon: Option<SoundIcon>,
        change: impl FnOnce(&mut tufo_models::library::SoundSettings),
    ) -> Option<UpdateCollectionEntry> {
        let (mut sound, title) = {
            let shared = self.lock();
            let (_, entry) = presentation::find_entry(&shared.snapshot, id)?;
            (entry.sound, title.filter(|title| *title != entry.title))
        };
        change(&mut sound);
        Some(UpdateCollectionEntry {
            title,
            sound: Some(sound),
            icon,
            ..UpdateCollectionEntry::default()
        })
    }

    pub(crate) fn set_sound_looping(&self, id: &str, looping: bool) {
        self.change_sound(id, None, None, |sound| sound.looping = looping);
    }

    pub(crate) fn set_sound_volume(&self, id: &str, volume: f32) {
        if let Some(id) = entry_id_of(id) {
            self.queue_volume(VolumeTarget::Sound(id), volume);
        }
    }

    pub(crate) fn update_sound(
        &self,
        id: &str,
        title: &str,
        fade_in_ms: i32,
        fade_out_ms: i32,
        icon_index: i32,
    ) {
        let fade = |milliseconds: i32| {
            FadeDuration::try_new(Duration::from_millis(
                u64::try_from(milliseconds).unwrap_or(0),
            ))
            .ok()
        };
        let (Some(fade_in), Some(fade_out)) = (fade(fade_in_ms), fade(fade_out_ms)) else {
            self.toast_error(tr("Fades must be between 0 and 60,000 ms.").to_owned());
            return;
        };
        let title = Title::try_new(title.to_owned()).ok();
        let icon = usize::try_from(icon_index)
            .ok()
            .and_then(|index| SoundIcon::ALL.get(index).copied());
        self.change_sound(id, title, icon, |sound| {
            sound.fade_in = fade_in;
            sound.fade_out = fade_out;
        });
    }

    fn update_entry(&self, id: CollectionEntryId, update: UpdateCollectionEntry) {
        self.run(move |client| async move { client.update_collection_entry(id, &update).await });
    }

    pub(crate) fn toggle_sounds_paused(&self) {
        let paused = {
            let shared = self.lock();
            if shared.player.sounds.is_empty() {
                return;
            }
            shared.player.sounds_paused
        };
        if paused {
            self.run(|client| async move { client.resume_sounds().await });
        } else {
            self.run(|client| async move { client.pause_sounds().await });
        }
    }

    // Transport.

    pub(crate) fn toggle_play_pause(&self) {
        let paused = {
            let shared = self.lock();
            shared
                .player
                .music_tracks()
                .next()
                .map(|music| music.paused)
        };
        if let Some(paused) = paused {
            self.set_music_paused(!paused);
        }
    }

    pub(crate) fn set_music_paused(&self, paused: bool) {
        if self.lock().player.music_tracks().next().is_none() {
            return;
        }
        if paused {
            self.run(|client| async move { client.pause_music().await });
        } else {
            self.run(|client| async move { client.resume_music().await });
        }
    }

    pub(crate) fn stop_music(&self) {
        self.run(|client| async move { client.stop_music().await });
    }

    pub(crate) fn seek_to(&self, position: Duration) {
        let duration = self
            .lock()
            .player
            .music
            .as_ref()
            .and_then(|music| music.duration);
        let Some(duration) = duration else {
            return;
        };
        if position < duration {
            self.run(move |client| async move { client.seek_music(position).await });
        }
    }

    pub(crate) fn seek_by(&self, forward: bool, offset: Duration) {
        let now = {
            let shared = self.lock();
            shared.player.music.as_ref().map(|music| {
                (
                    presentation::music_position(music, shared.player.modes, SystemTime::now()),
                    presentation::music_end(music),
                )
            })
        };
        let Some((position, Some(end))) = now else {
            return;
        };
        let position = if forward {
            position.saturating_add(offset)
        } else {
            position.saturating_sub(offset)
        };
        if position >= end {
            self.next_track();
        } else {
            self.seek_to(position);
        }
    }

    pub(crate) fn next_track(&self) {
        self.run(|client| async move { client.skip_to_next_track().await });
    }

    pub(crate) fn previous_track(&self) {
        self.run(|client| async move { client.skip_to_previous_track().await });
    }

    pub(crate) fn seek(&self, fraction: f32) {
        let bounds = self
            .lock()
            .player
            .music
            .as_ref()
            .and_then(|music| Some((music.duration?, presentation::music_end(music)?)));
        if let Some((duration, end)) = bounds {
            let position = duration
                .mul_f32(fraction.clamp(0.0, 1.0))
                .min(end.saturating_sub(SEEK_END_MARGIN));
            self.seek_to(position);
        }
    }

    /// Sends the newest slider volume, coalescing a fast drag into a few requests.
    pub(crate) fn set_volume(&self, volume: f32) {
        self.queue_volume(VolumeTarget::Player, volume);
    }

    pub(crate) fn set_effects_volume(&self, volume: f32) {
        self.queue_volume(VolumeTarget::Effects, volume);
    }

    /// Keeps one request in flight and only each slider's newest volume waiting.
    fn queue_volume(&self, target: VolumeTarget, volume: f32) {
        let Ok(volume) = Volume::try_new(volume.clamp(0.0, 1.0)) else {
            return;
        };
        let start_sender = {
            let mut shared = self.lock();
            match shared
                .pending_volumes
                .iter_mut()
                .find(|(pending, _)| *pending == target)
            {
                Some(pending) => pending.1 = volume,
                None => shared.pending_volumes.push((target, volume)),
            }
            !std::mem::replace(&mut shared.sending_volumes, true)
        };
        if start_sender {
            self.runtime.spawn(self.clone().send_pending_volumes());
        }
    }

    async fn send_pending_volumes(self) {
        while let Some((target, volume, client)) = self.take_pending_volume() {
            let Some(client) = client else {
                continue;
            };
            if let Err(error) = self.send_volume(&client, target, volume).await {
                self.toast_error(presentation::error_text(&error));
            }
        }
    }

    async fn send_volume(
        &self,
        client: &TufoClient,
        target: VolumeTarget,
        volume: Volume,
    ) -> Result<(), ClientError> {
        match target {
            VolumeTarget::Player => {
                let update = UpdatePlayerSettings {
                    volume: Some(volume),
                    muted: Some(false),
                    ..UpdatePlayerSettings::default()
                };
                client.update_player_settings(&update).await
            }
            VolumeTarget::Effects => {
                client
                    .update_player_settings(&UpdatePlayerSettings {
                        effects_volume: Some(volume),
                        ..UpdatePlayerSettings::default()
                    })
                    .await
            }
            VolumeTarget::Browser => {
                client
                    .update_player_settings(&UpdatePlayerSettings {
                        browser_volume: Some(volume),
                        browser_muted: Some(false),
                        ..UpdatePlayerSettings::default()
                    })
                    .await
            }
            VolumeTarget::Sound(id) => {
                match self.sound_update(id, None, None, |sound| sound.volume = volume) {
                    Some(update) => client.update_collection_entry(id, &update).await,
                    None => Ok(()),
                }
            }
        }
    }

    /// The oldest slider's newest unsent volume, or `None` after marking the sender finished.
    fn take_pending_volume(&self) -> Option<(VolumeTarget, Volume, Option<TufoClient>)> {
        let mut shared = self.lock();
        if shared.pending_volumes.is_empty() {
            shared.sending_volumes = false;
            return None;
        }
        let (target, volume) = shared.pending_volumes.remove(0);
        Some((target, volume, shared.client.clone()))
    }

    pub(crate) fn toggle_mute(&self) {
        let muted = !self.lock().player.muted;
        self.update_settings(UpdatePlayerSettings {
            muted: Some(muted),
            ..UpdatePlayerSettings::default()
        });
    }

    pub(crate) fn toggle_shuffle(&self) {
        let shuffle = !self.lock().player.modes.shuffle;
        self.update_settings(UpdatePlayerSettings {
            shuffle: Some(shuffle),
            ..UpdatePlayerSettings::default()
        });
    }

    pub(crate) fn cycle_repeat(&self) {
        let repeat = {
            let state = self.lock();
            presentation::next_repeat_mode(
                state.player.modes.repeat,
                !state.player.queued_entry_ids.is_empty(),
            )
        };
        self.update_settings(UpdatePlayerSettings {
            repeat: Some(repeat),
            ..UpdatePlayerSettings::default()
        });
    }

    fn update_settings(&self, update: UpdatePlayerSettings) {
        self.run(move |client| async move { client.update_player_settings(&update).await });
    }

    // Outputs, settings, and quitting.

    pub(crate) fn select_output(&self, index: i32) {
        let target = usize::try_from(index)
            .ok()
            .and_then(|index| self.lock().output_targets.get(index).copied());
        if let Some(target) = target {
            self.run(move |client| async move { client.select_output(target).await });
        }
    }

    pub(crate) fn refresh_outputs(&self) {
        if let Some(client) = self.client() {
            self.runtime.spawn(self.clone().load_voice_channels(client));
        }
    }

    /// Saves the desktop settings, then the crossfade when it changed, and a new bot token
    /// followed by a server restart so the bot logs in with it; the connection loop starts the
    /// server again.
    pub(crate) fn set_crossfade(&self, crossfade_ms: i32) {
        let crossfade = CrossfadeDuration::try_new(Duration::from_millis(
            u64::try_from(crossfade_ms).unwrap_or(0),
        ))
        .ok()
        .filter(|crossfade| *crossfade != self.lock().player.crossfade);
        if crossfade.is_none() {
            return;
        }
        self.run(move |client| async move {
            client
                .update_player_settings(&UpdatePlayerSettings {
                    crossfade,
                    ..UpdatePlayerSettings::default()
                })
                .await
        });
    }

    pub(crate) fn set_keep_playing_after_quit(&self, keep_playing_after_quit: bool) {
        let quit_behavior = if keep_playing_after_quit {
            QuitBehavior::KeepPlaying
        } else {
            QuitBehavior::StandBy
        };
        self.save_desktop_settings(DesktopSettingsUpdate {
            quit_behavior: Some(quit_behavior),
            ..DesktopSettingsUpdate::default()
        });
    }

    pub(crate) fn set_tour_visibility(&self, visibility: ui::TourVisibility) {
        let tours = match visibility {
            ui::TourVisibility::AlwaysShow => TourVisibility::AlwaysShow,
            ui::TourVisibility::ShowUnseen => TourVisibility::ShowUnseen,
            ui::TourVisibility::AlwaysHide => TourVisibility::AlwaysHide,
        };
        self.save_desktop_settings(DesktopSettingsUpdate {
            tours: Some(tours),
            ..DesktopSettingsUpdate::default()
        });
    }

    pub(crate) fn set_discord_connection_disabled(&self, disabled: bool) {
        if disabled {
            self.run(|client| async move { client.disable_discord_connection().await });
        } else {
            self.run(|client| async move { client.enable_discord_connection().await });
        }
    }

    /// Restarts the server, so its bot logs in with the new token.
    pub(crate) fn save_discord_token(&self, token: &str) {
        let token = token.trim().to_owned();
        if token.is_empty() {
            return;
        }
        self.run(move |client| async move {
            client.set_discord_bot_token(token).await?;
            client.stop_server().await
        });
    }

    pub(crate) fn set_start_at_login(&self, start_at_login: bool) {
        if start_at_login == autostart::is_enabled() {
            return;
        }
        match autostart::set_enabled(start_at_login) {
            Ok(()) => render::start_at_login(&self.window, start_at_login),
            Err(error) => {
                tracing::warn!(%error, "cannot change starting at login");
                self.toast_error(
                    tr("Tufo could not change starting at login. Check your system settings.")
                        .to_owned(),
                );
            }
        }
    }

    fn save_desktop_settings(&self, update: DesktopSettingsUpdate) {
        let Some(file) = self.settings_file.clone() else {
            self.toast_error(
                tr("This system names no config directory for Tufo's settings.").to_owned(),
            );
            return;
        };
        let controller = self.clone();
        self.runtime
            .spawn_blocking(move || match file.update(update) {
                Ok(settings) => {
                    controller.lock().settings = settings;
                    render::desktop_settings(&controller.window, settings);
                }
                Err(error) => {
                    tracing::warn!(%error, "cannot save desktop settings");
                    controller.toast_error(
                        tr("Tufo could not save config.toml. Check its contents and permissions.")
                            .to_owned(),
                    );
                }
            });
    }

    /// Quits Tufo, leaving the server running and, unless the audio should keep playing,
    /// standing it by first.
    pub(crate) fn quit(&self) {
        let stand_by_client = self.start_quitting();
        let window = self.window.clone();
        self.runtime.spawn(async move {
            if let Some(client) = stand_by_client {
                let _ = client.stand_by().await;
            }
            let _ = window.upgrade_in_event_loop(|_| {
                let _ = slint::quit_event_loop();
            });
        });
    }

    /// Stands the server by as [`Self::quit`] does when the event loop ends without it: macOS's
    /// Quit menu item and logging out end the loop directly. Blocks the UI thread.
    pub(crate) fn stand_by_on_exit(&self) {
        if let Some(client) = self.start_quitting() {
            let _ = self.runtime.block_on(client.stand_by());
        }
    }

    /// The client to stand the server by with, the first time Tufo quits.
    fn start_quitting(&self) -> Option<TufoClient> {
        let mut shared = self.lock();
        if std::mem::replace(&mut shared.quitting, true)
            || shared.settings.quit_behavior != QuitBehavior::StandBy
        {
            return None;
        }
        shared.client.clone()
    }

    // Plumbing.

    /// Runs one request, and shows its failure.
    fn run<Command, Request>(&self, command: Command)
    where
        Command: FnOnce(TufoClient) -> Request + Send + 'static,
        Request: Future<Output = Result<(), ClientError>> + Send,
    {
        let Some(client) = self.connected_client() else {
            return;
        };
        let controller = self.clone();
        self.runtime.spawn(async move {
            if let Err(error) = command(client).await {
                controller.toast_error(presentation::error_text(&error));
            }
        });
    }

    /// Runs an import or addition and shows what it changed.
    fn run_report<Command, Request>(&self, command: Command)
    where
        Command: FnOnce(TufoClient) -> Request + Send + 'static,
        Request: Future<Output = Result<ImportReport, ClientError>> + Send,
    {
        let Some(client) = self.connected_client() else {
            return;
        };
        let controller = self.clone();
        self.toast(tr("Reading the audio files…").to_owned());
        self.runtime.spawn(async move {
            match command(client).await {
                Ok(report) => controller.toast(render::report_text(&report)),
                Err(error) => controller.toast_error(presentation::error_text(&error)),
            }
        });
    }

    fn toast(&self, text: String) {
        render::toast(&self.window, text, false);
    }

    fn toast_error(&self, text: String) {
        render::toast(&self.window, text, true);
    }

    fn client(&self) -> Option<TufoClient> {
        self.lock().client.clone()
    }

    /// Reports a missing connection as a toast.
    fn connected_client(&self) -> Option<TufoClient> {
        let client = self.client();
        if client.is_none() {
            self.toast_error(server_disconnected_text());
        }
        client
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn parse_uuid(text: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(text).ok()
}

fn collection_id(text: &str) -> Option<CollectionId> {
    parse_uuid(text).map(CollectionId::from_uuid)
}

/// The picker the window's target names, or `None` when no dialog shows one.
pub(crate) fn background_target(target: ui::BackgroundTarget) -> Option<BackgroundTarget> {
    match target {
        ui::BackgroundTarget::None => None,
        ui::BackgroundTarget::NewCollection => Some(BackgroundTarget::NewCollection),
        ui::BackgroundTarget::OpenCollection => Some(BackgroundTarget::OpenCollection),
    }
}

/// Whether the path ends in one of `extensions`, ignoring case.
fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

fn entry_id_of(text: &str) -> Option<CollectionEntryId> {
    parse_uuid(text).map(CollectionEntryId::from_uuid)
}

fn audio_file_id(text: &str) -> Option<AudioFileId> {
    parse_uuid(text).map(AudioFileId::from_uuid)
}

fn server_disconnected_text() -> String {
    tr("tufo-server is not connected.").to_owned()
}
