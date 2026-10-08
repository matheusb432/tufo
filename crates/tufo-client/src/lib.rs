//! Typed client for the local Tufo server, speaking gRPC over its user-private local endpoint.
//!
//! Callers exchange domain values only; Prost messages and the transport stay inside this crate.

use std::time::Duration;

pub mod browser;
pub mod desktop;
pub mod import_resources;
pub mod process;
pub mod song_loop_update;

pub use tonic::Code;
use tonic::{Request, Streaming, transport::Channel};
pub use tufo_local_transport::{ConnectError, LocalEndpoint};
use tufo_models::{
    discord::VoiceChannel,
    kenku_import::KenkuImportId,
    library::{AudioFileId, CollectionEntryId, CollectionId, LibraryFolderId},
    playback::{OutputTarget, diagnostics::PlayerDiagnostics, player_state::PlayerState},
};
/// The operation and status contracts callers exchange with the server.
pub use tufo_wire::{error, library, player, server, song_loops};
use tufo_wire::{
    library::{
        AddFilesToCollection, AddSongsToPlaylist, CreateCollection, ImportKenkuLibrary,
        ImportPaths, ImportReport, KenkuImportStatus, LibrarySnapshot, UpdateCollection,
        UpdateCollectionEntry,
    },
    pb::{
        self, library_service_client::LibraryServiceClient,
        player_service_client::PlayerServiceClient, server_service_client::ServerServiceClient,
        voice_channel_service_client::VoiceChannelServiceClient,
    },
    player::{UpdateMusicLayer, UpdatePlayerSettings},
    proto::{self, InvalidMessage},
    song_loops::{
        ImportSongLoops, PreviewSongLoop, PreviewStarted, SongLoopImportResult, SongWaveform,
        UpdateSongLoop,
    },
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Client-side cap on waiting for any response to start; streams may then run indefinitely.
const RESPONSE_START_TIMEOUT: Duration = Duration::from_secs(30);
const QUICK_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Covers reading, probing, and prefilling a large file.
const PLAY_AUDIO_TIMEOUT: Duration = Duration::from_secs(10);
/// Covers joining a Discord voice channel, which the server bounds at 15 s.
const SELECT_OUTPUT_TIMEOUT: Duration = Duration::from_secs(20);
/// Covers scanning and fingerprinting a large dropped folder; the server reads about 50 files a
/// second.
const IMPORT_TIMEOUT: Duration = Duration::from_mins(10);
/// Covers the browser's grace period to save its tabs, then killing what remains.
const BROWSER_CLOSE_TIMEOUT: Duration = Duration::from_secs(15);
/// Covers decoding a long song in full.
const SONG_ANALYSIS_TIMEOUT: Duration = Duration::from_mins(2);
/// A library snapshot carries every entry, so it may be large.
const LIBRARY_SNAPSHOT_SIZE_MAX: usize = 64 * 1024 * 1024;
/// Bounds how many pages `list_voice_channels` follows.
const VOICE_CHANNEL_PAGES_MAX: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Status(#[from] tonic::Status),
    #[error(transparent)]
    InvalidResponse(#[from] InvalidMessage),
    #[error("the server returned more than {VOICE_CHANNEL_PAGES_MAX} pages of voice channels")]
    TooManyPages,
    #[error("the server ended the preview before it started")]
    PreviewNotStarted,
    #[error(
        "the running server does not support partial library updates; replace matching Tufo binaries and run `tufo server restart`"
    )]
    LibraryPatchesUnsupported,
}

#[derive(Debug, Clone)]
pub struct TufoClient {
    health: tonic_health::pb::health_client::HealthClient<Channel>,
    library: LibraryServiceClient<Channel>,
    player: PlayerServiceClient<Channel>,
    server: ServerServiceClient<Channel>,
    voice_channels: VoiceChannelServiceClient<Channel>,
}

impl TufoClient {
    /// Checks the standard whole-server gRPC health entry without changing server state.
    pub async fn server_is_serving(&self) -> Result<bool, ClientError> {
        let response = self
            .health
            .clone()
            .check(with_timeout(
                tonic_health::pb::HealthCheckRequest {
                    service: String::new(),
                },
                QUICK_REQUEST_TIMEOUT,
            ))
            .await?
            .into_inner();
        Ok(response.status == tonic_health::ServingStatus::Serving as i32)
    }

    pub async fn server_status(&self) -> Result<server::ServerStatus, ClientError> {
        let response = self
            .server
            .clone()
            .get_server_status(with_timeout(
                pb::GetServerStatusRequest {},
                QUICK_REQUEST_TIMEOUT,
            ))
            .await?;
        Ok(proto::server::server_status(response.into_inner())?)
    }

    /// Connects on first use and reconnects after the server restarts.
    pub fn connect_lazy(endpoint: &LocalEndpoint) -> Result<Self, ConnectError> {
        let channel = endpoint.connect_lazy(CONNECT_TIMEOUT, RESPONSE_START_TIMEOUT)?;
        Ok(Self {
            health: tonic_health::pb::health_client::HealthClient::new(channel.clone()),
            library: LibraryServiceClient::new(channel.clone())
                .max_decoding_message_size(LIBRARY_SNAPSHOT_SIZE_MAX),
            player: PlayerServiceClient::new(channel.clone()),
            server: ServerServiceClient::new(channel.clone()),
            voice_channels: VoiceChannelServiceClient::new(channel),
        })
    }

    /// Lists every voice channel the bot can see, following pages.
    pub async fn list_voice_channels(&self) -> Result<Vec<VoiceChannel>, ClientError> {
        let mut channels = Vec::new();
        let mut page_token = String::new();
        for _ in 0..VOICE_CHANNEL_PAGES_MAX {
            let request = with_timeout(
                proto::voice_channel::list_voice_channels_message(page_token),
                QUICK_REQUEST_TIMEOUT,
            );
            let response = self
                .voice_channels
                .clone()
                .list_voice_channels(request)
                .await?
                .into_inner();
            let (page, next_page_token) = proto::voice_channel::voice_channel_page(response)?;
            channels.extend(page);
            match next_page_token {
                Some(token) => page_token = token,
                None => return Ok(channels),
            }
        }
        Err(ClientError::TooManyPages)
    }
}

/// Library methods: each change is visible in the library stream once its call returns.
impl TufoClient {
    /// Streams the library, then the library again after each change, until the server shuts
    /// down.
    pub async fn watch_library(&self) -> Result<LibrarySnapshots, ClientError> {
        let stream = self
            .library
            .clone()
            .watch_library(pb::WatchLibraryRequest {})
            .await?
            .into_inner();
        Ok(LibrarySnapshots { stream })
    }

    pub async fn import_paths(&self, request: &ImportPaths) -> Result<ImportReport, ClientError> {
        let request = with_timeout(
            proto::library::import_paths_message(request),
            IMPORT_TIMEOUT,
        );
        let response = self.library.clone().import_paths(request).await?;
        Ok(proto::library::import_report(response.into_inner().report)?)
    }

    pub async fn add_files_to_collection(
        &self,
        request: &AddFilesToCollection,
    ) -> Result<ImportReport, ClientError> {
        let request = with_timeout(
            proto::library::add_files_to_collection_message(request),
            IMPORT_TIMEOUT,
        );
        let response = self
            .library
            .clone()
            .add_files_to_collection(request)
            .await?;
        Ok(proto::library::import_report(response.into_inner().report)?)
    }

    pub async fn create_collection(
        &self,
        request: &CreateCollection,
    ) -> Result<CollectionId, ClientError> {
        let request = with_timeout(
            proto::library::create_collection_message(request),
            QUICK_REQUEST_TIMEOUT,
        );
        let response = self.library.clone().create_collection(request).await?;
        Ok(proto::library::created_collection_id(response.get_ref())?)
    }

    /// Rejects nonempty nested patches before mutation when the server lacks patch support.
    pub async fn update_collection(
        &self,
        id: CollectionId,
        update: &UpdateCollection,
    ) -> Result<(), ClientError> {
        if update
            .order_patch
            .is_some_and(|patch| patch != library::EntryOrderPatch::default())
        {
            self.require_library_patches().await?;
        }
        let request = with_timeout(
            proto::library::update_collection_message(id, update),
            QUICK_REQUEST_TIMEOUT,
        );
        self.library.clone().update_collection(request).await?;
        Ok(())
    }

    pub async fn delete_collection(&self, id: CollectionId) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::library::delete_collection_message(id),
            QUICK_REQUEST_TIMEOUT,
        );
        self.library.clone().delete_collection(request).await?;
        Ok(())
    }

    /// Rejects nonempty nested patches before mutation when the server lacks patch support.
    pub async fn update_collection_entry(
        &self,
        id: CollectionEntryId,
        update: &UpdateCollectionEntry,
    ) -> Result<(), ClientError> {
        if update
            .sound_patch
            .is_some_and(|patch| patch != library::SoundSettingsPatch::default())
        {
            self.require_library_patches().await?;
        }
        let request = with_timeout(
            proto::library::update_collection_entry_message(id, update),
            QUICK_REQUEST_TIMEOUT,
        );
        self.library
            .clone()
            .update_collection_entry(request)
            .await?;
        Ok(())
    }

    async fn require_library_patches(&self) -> Result<(), ClientError> {
        if !self.server_status().await?.supports_library_patches {
            return Err(ClientError::LibraryPatchesUnsupported);
        }
        Ok(())
    }

    pub async fn delete_collection_entry(&self, id: CollectionEntryId) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::library::delete_collection_entry_message(id),
            QUICK_REQUEST_TIMEOUT,
        );
        self.library
            .clone()
            .delete_collection_entry(request)
            .await?;
        Ok(())
    }

    /// How many songs the playlist didn't list before.
    pub async fn add_songs_to_playlist(
        &self,
        request: &AddSongsToPlaylist,
    ) -> Result<usize, ClientError> {
        let request = with_timeout(
            proto::library::add_songs_to_playlist_message(request),
            QUICK_REQUEST_TIMEOUT,
        );
        let response = self.library.clone().add_songs_to_playlist(request).await?;
        Ok(proto::library::songs_added(response.into_inner()))
    }

    pub async fn delete_song(&self, id: AudioFileId) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::library::delete_song_message(id),
            QUICK_REQUEST_TIMEOUT,
        );
        self.library.clone().delete_song(request).await?;
        Ok(())
    }

    pub async fn delete_library_folder(&self, id: LibraryFolderId) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::library::delete_library_folder_message(id),
            QUICK_REQUEST_TIMEOUT,
        );
        self.library.clone().delete_library_folder(request).await?;
        Ok(())
    }

    pub async fn rescan_library(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::RescanLibraryRequest {}, IMPORT_TIMEOUT);
        self.library.clone().rescan_library(request).await?;
        Ok(())
    }

    /// Starts or resumes a saved import; its stream carries progress and the eventual result.
    pub async fn import_kenku_library(
        &self,
        request: &ImportKenkuLibrary,
    ) -> Result<KenkuImportId, ClientError> {
        let request = with_timeout(
            proto::library::import_kenku_library_message(request),
            QUICK_REQUEST_TIMEOUT,
        );
        let response = self.library.clone().import_kenku_library(request).await?;
        Ok(proto::library::kenku_import_id(
            &response.into_inner().import_id,
        )?)
    }

    pub async fn get_song_waveform(&self, id: AudioFileId) -> Result<SongWaveform, ClientError> {
        let request = with_timeout(
            proto::song_loop::get_song_waveform_message(id),
            SONG_ANALYSIS_TIMEOUT,
        );
        let response = self.library.clone().get_song_waveform(request).await?;
        Ok(proto::song_loop::song_waveform(response.into_inner())?)
    }

    pub async fn update_song_loop(&self, update: &UpdateSongLoop) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::song_loop::update_song_loop_message(update),
            SONG_ANALYSIS_TIMEOUT,
        );
        self.library.clone().update_song_loop(request).await?;
        Ok(())
    }

    pub async fn delete_song_loop(&self, id: AudioFileId) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::song_loop::delete_song_loop_message(id),
            QUICK_REQUEST_TIMEOUT,
        );
        self.library.clone().delete_song_loop(request).await?;
        Ok(())
    }

    pub async fn export_song_loops(&self) -> Result<String, ClientError> {
        let request = with_timeout(pb::ExportSongLoopsRequest {}, QUICK_REQUEST_TIMEOUT);
        let response = self.library.clone().export_song_loops(request).await?;
        Ok(response.into_inner().document)
    }

    pub async fn import_song_loops(
        &self,
        request: &ImportSongLoops,
    ) -> Result<Vec<SongLoopImportResult>, ClientError> {
        let request = with_timeout(
            proto::song_loop::import_song_loops_message(request),
            IMPORT_TIMEOUT,
        );
        let response = self.library.clone().import_song_loops(request).await?;
        Ok(proto::song_loop::song_loop_import_results(
            response.into_inner(),
        )?)
    }

    pub async fn watch_kenku_import(&self) -> Result<KenkuImportUpdates, ClientError> {
        let stream = self
            .library
            .clone()
            .watch_kenku_import(pb::WatchKenkuImportRequest {})
            .await?
            .into_inner();
        Ok(KenkuImportUpdates { stream })
    }
}

/// The latest saved import and coalesced progress. An initial None means no imports yet.
pub struct KenkuImportUpdates {
    stream: Streaming<pb::WatchKenkuImportResponse>,
}

impl KenkuImportUpdates {
    pub async fn next(&mut self) -> Option<Result<Option<KenkuImportStatus>, ClientError>> {
        match self.stream.message().await {
            Ok(Some(response)) => Some(
                response
                    .status
                    .map(proto::library::kenku_import_status)
                    .transpose()
                    .map_err(Into::into),
            ),
            Ok(None) => None,
            Err(error) => Some(Err(error.into())),
        }
    }
}

/// Library snapshots as the server publishes them.
pub struct LibrarySnapshots {
    stream: Streaming<pb::WatchLibraryResponse>,
}

impl LibrarySnapshots {
    /// Waits for the next snapshot; `None` once the server ends the stream.
    pub async fn next(&mut self) -> Option<Result<LibrarySnapshot, ClientError>> {
        match self.stream.message().await {
            Ok(Some(response)) => {
                Some(proto::library::library_snapshot(response).map_err(ClientError::from))
            }
            Ok(None) => None,
            Err(status) => Some(Err(ClientError::Status(status))),
        }
    }
}

/// Player methods: each change is visible in the player stream once its call returns.
impl TufoClient {
    /// Streams the player state, then each change, until the server shuts down.
    pub async fn watch_player(&self) -> Result<PlayerStates, ClientError> {
        let stream = self
            .player
            .clone()
            .watch_player(pb::WatchPlayerRequest {})
            .await?
            .into_inner();
        Ok(PlayerStates { stream })
    }

    pub async fn play_collection(
        &self,
        collection_id: CollectionId,
        start: Option<CollectionEntryId>,
    ) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::player::play_collection_message(collection_id, start),
            PLAY_AUDIO_TIMEOUT,
        );
        self.player.clone().play_collection(request).await?;
        Ok(())
    }

    pub async fn pause_music(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::PauseMusicRequest {}, QUICK_REQUEST_TIMEOUT);
        self.player.clone().pause_music(request).await?;
        Ok(())
    }

    pub async fn add_music_layer(&self, entry_id: CollectionEntryId) -> Result<(), ClientError> {
        let request = with_timeout(
            pb::AddMusicLayerRequest {
                collection_entry_id: entry_id.to_string(),
            },
            PLAY_AUDIO_TIMEOUT,
        );
        self.player.clone().add_music_layer(request).await?;
        Ok(())
    }

    /// Plays the song now when no music plays.
    pub async fn add_to_queue(&self, entry_id: CollectionEntryId) -> Result<(), ClientError> {
        let request = with_timeout(
            pb::AddToQueueRequest {
                collection_entry_id: entry_id.to_string(),
            },
            PLAY_AUDIO_TIMEOUT,
        );
        self.player.clone().add_to_queue(request).await?;
        Ok(())
    }

    pub async fn remove_from_queue(&self, entry_id: CollectionEntryId) -> Result<(), ClientError> {
        let request = with_timeout(
            pb::RemoveFromQueueRequest {
                collection_entry_id: entry_id.to_string(),
            },
            QUICK_REQUEST_TIMEOUT,
        );
        self.player.clone().remove_from_queue(request).await?;
        Ok(())
    }

    pub async fn stop_music_layer(&self, entry_id: CollectionEntryId) -> Result<(), ClientError> {
        let request = with_timeout(
            pb::StopMusicLayerRequest {
                collection_entry_id: entry_id.to_string(),
            },
            QUICK_REQUEST_TIMEOUT,
        );
        self.player.clone().stop_music_layer(request).await?;
        Ok(())
    }

    pub async fn update_music_layer(
        &self,
        entry_id: CollectionEntryId,
        update: UpdateMusicLayer,
    ) -> Result<(), ClientError> {
        let request = with_timeout(
            pb::UpdateMusicLayerRequest {
                collection_entry_id: entry_id.to_string(),
                volume: update.volume.map(tufo_models::library::Volume::into_inner),
                looping: update.looping,
            },
            QUICK_REQUEST_TIMEOUT,
        );
        self.player.clone().update_music_layer(request).await?;
        Ok(())
    }

    pub async fn resume_music(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::ResumeMusicRequest {}, PLAY_AUDIO_TIMEOUT);
        self.player.clone().resume_music(request).await?;
        Ok(())
    }

    pub async fn stop_music(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::StopMusicRequest {}, QUICK_REQUEST_TIMEOUT);
        self.player.clone().stop_music(request).await?;
        Ok(())
    }

    pub async fn skip_to_next_track(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::SkipToNextTrackRequest {}, PLAY_AUDIO_TIMEOUT);
        self.player.clone().skip_to_next_track(request).await?;
        Ok(())
    }

    pub async fn skip_to_previous_track(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::SkipToPreviousTrackRequest {}, PLAY_AUDIO_TIMEOUT);
        self.player.clone().skip_to_previous_track(request).await?;
        Ok(())
    }

    pub async fn seek_music(&self, position: Duration) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::player::seek_music_message(position),
            PLAY_AUDIO_TIMEOUT,
        );
        self.player.clone().seek_music(request).await?;
        Ok(())
    }

    pub async fn update_player_settings(
        &self,
        update: &UpdatePlayerSettings,
    ) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::player::update_player_settings_message(update),
            QUICK_REQUEST_TIMEOUT,
        );
        self.player.clone().update_player_settings(request).await?;
        Ok(())
    }

    pub async fn play_sound(&self, entry_id: CollectionEntryId) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::player::play_sound_message(entry_id),
            PLAY_AUDIO_TIMEOUT,
        );
        self.player.clone().play_sound(request).await?;
        Ok(())
    }

    pub async fn stop_sound(&self, entry_id: CollectionEntryId) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::player::stop_sound_message(entry_id),
            QUICK_REQUEST_TIMEOUT,
        );
        self.player.clone().stop_sound(request).await?;
        Ok(())
    }

    pub async fn pause_sounds(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::PauseSoundsRequest {}, QUICK_REQUEST_TIMEOUT);
        self.player.clone().pause_sounds(request).await?;
        Ok(())
    }

    pub async fn resume_sounds(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::ResumeSoundsRequest {}, QUICK_REQUEST_TIMEOUT);
        self.player.clone().resume_sounds(request).await?;
        Ok(())
    }

    pub async fn resume_saved_loops(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::ResumeSavedLoopsRequest {}, PLAY_AUDIO_TIMEOUT);
        self.player.clone().resume_saved_loops(request).await?;
        Ok(())
    }

    pub async fn select_output(&self, target: OutputTarget) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::player::select_output_message(target),
            SELECT_OUTPUT_TIMEOUT,
        );
        self.player.clone().select_output(request).await?;
        Ok(())
    }

    /// Fades out all audio, then closes the audio device and leaves any voice channel until
    /// something plays again.
    pub async fn stand_by(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::StandByRequest {}, QUICK_REQUEST_TIMEOUT);
        self.player.clone().stand_by(request).await?;
        Ok(())
    }

    /// Starts the browser unless it runs; the player state reports when it accepts views.
    pub async fn open_browser(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::OpenBrowserRequest {}, QUICK_REQUEST_TIMEOUT);
        self.player.clone().open_browser(request).await?;
        Ok(())
    }

    /// Returns once every process the browser started is gone.
    pub async fn close_browser(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::CloseBrowserRequest {}, BROWSER_CLOSE_TIMEOUT);
        self.player.clone().close_browser(request).await?;
        Ok(())
    }

    /// The preview plays until the returned value drops.
    pub async fn preview_song_loop(
        &self,
        request: &PreviewSongLoop,
    ) -> Result<SongLoopPreview, ClientError> {
        let request = with_timeout(
            proto::song_loop::preview_song_loop_message(request),
            SONG_ANALYSIS_TIMEOUT,
        );
        let mut stream = self
            .player
            .clone()
            .preview_song_loop(request)
            .await?
            .into_inner();
        let started = stream
            .message()
            .await?
            .ok_or(ClientError::PreviewNotStarted)?;
        Ok(SongLoopPreview {
            started: proto::song_loop::preview_started(started)?,
            stream,
        })
    }

    pub async fn player_diagnostics(&self) -> Result<PlayerDiagnostics, ClientError> {
        let request = with_timeout(pb::GetPlayerDiagnosticsRequest {}, QUICK_REQUEST_TIMEOUT);
        let response = self.player.clone().get_player_diagnostics(request).await?;
        Ok(proto::player::player_diagnostics(response.into_inner())?)
    }

    /// Asks the server to drain and exit. It stops all audio.
    pub async fn stop_server(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::StopServerRequest {}, QUICK_REQUEST_TIMEOUT);
        self.server.clone().stop_server(request).await?;
        Ok(())
    }

    /// Saves the Discord bot token; it applies when the server next starts.
    pub async fn set_discord_bot_token(&self, bot_token: String) -> Result<(), ClientError> {
        let request = with_timeout(
            pb::SetDiscordBotTokenRequest { bot_token },
            QUICK_REQUEST_TIMEOUT,
        );
        self.server.clone().set_discord_bot_token(request).await?;
        Ok(())
    }

    /// Moves playback off any voice channel and logs the bot out until the connection is
    /// enabled again.
    pub async fn disable_discord_connection(&self) -> Result<(), ClientError> {
        // Moving playback off a voice channel reopens the local device.
        let request = with_timeout(
            pb::DisableDiscordConnectionRequest {},
            SELECT_OUTPUT_TIMEOUT,
        );
        self.server
            .clone()
            .disable_discord_connection(request)
            .await?;
        Ok(())
    }

    pub async fn enable_discord_connection(&self) -> Result<(), ClientError> {
        let request = with_timeout(pb::EnableDiscordConnectionRequest {}, QUICK_REQUEST_TIMEOUT);
        self.server
            .clone()
            .enable_discord_connection(request)
            .await?;
        Ok(())
    }
}

/// A preview playing on the server's local device.
pub struct SongLoopPreview {
    pub started: PreviewStarted,
    stream: Streaming<pb::PreviewSongLoopResponse>,
}

impl SongLoopPreview {
    /// Resolves when the server ends the preview, as when a newer one replaces it.
    pub async fn ended(&mut self) -> Result<(), ClientError> {
        while self.stream.message().await?.is_some() {}
        Ok(())
    }
}

/// Player states as the server publishes them.
pub struct PlayerStates {
    stream: Streaming<pb::WatchPlayerResponse>,
}

impl PlayerStates {
    /// Waits for the next state; `None` once the server ends the stream.
    pub async fn next(&mut self) -> Option<Result<PlayerState, ClientError>> {
        match self.stream.message().await {
            Ok(Some(response)) => {
                Some(proto::player::player_state(response).map_err(ClientError::from))
            }
            Ok(None) => None,
            Err(status) => Some(Err(ClientError::Status(status))),
        }
    }
}

fn with_timeout<T>(message: T, timeout: Duration) -> Request<T> {
    let mut request = Request::new(message);
    request.set_timeout(timeout);
    request
}
