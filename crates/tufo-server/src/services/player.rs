use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tufo_application::playback::{
    add_music_layer, add_to_queue, close_browser,
    control_music::{self, MusicCommand},
    control_sounds::{self, SoundsCommand},
    get_diagnostics, open_browser, play_collection, play_entry_sound, preview_song_loop,
    remove_from_queue, resume_saved_loops, select_output, stand_by, stop_music_layer, stop_sound,
    update_music_layer, update_player_settings, update_song_loop_preview, watch_player,
};
use tufo_infra::player::outputs::{AudioOutputs, DeviceOutputs};
use tufo_models::playback::player_state::PlayerState;
use tufo_wire::{
    pb::{self, player_service_server::PlayerService},
    proto,
};

use super::GrpcResultExt as _;
use crate::AppState;

pub(crate) struct PlayerGrpcService<O: AudioOutputs = DeviceOutputs> {
    state: AppState<O>,
    /// Turns true when the server starts draining, which ends every player stream.
    draining: watch::Receiver<bool>,
}

impl<O: AudioOutputs> PlayerGrpcService<O> {
    pub(crate) fn new(state: AppState<O>, draining: watch::Receiver<bool>) -> Self {
        Self { state, draining }
    }

    async fn control(&self, command: MusicCommand) -> Result<(), Status> {
        control_music::execute(command, self.state.player.as_ref())
            .await
            .into_grpc()
    }

    async fn control_sounds(&self, command: SoundsCommand) {
        control_sounds::execute(
            command,
            self.state.player.as_ref(),
            self.state.library.database(),
        )
        .await;
    }
}

#[tonic::async_trait]
impl<O: AudioOutputs> PlayerService for PlayerGrpcService<O> {
    type WatchPlayerStream = ReceiverStream<Result<pb::WatchPlayerResponse, Status>>;
    type PreviewSongLoopStream = ReceiverStream<Result<pb::PreviewSongLoopResponse, Status>>;

    async fn preview_song_loop(
        &self,
        request: Request<pb::PreviewSongLoopRequest>,
    ) -> Result<Response<Self::PreviewSongLoopStream>, Status> {
        let request = proto::song_loop::preview_song_loop_request(request.into_inner())?;
        let session =
            preview_song_loop::execute(request, &self.state.library, self.state.player.as_ref())
                .await
                .into_grpc()?;
        let (sender, receiver) = mpsc::channel(1);
        let player = std::sync::Arc::clone(&self.state.player);
        let mut draining = self.draining.clone();
        tokio::spawn(async move {
            let mut started = session.started;
            let mut ended = session.ended;
            loop {
                let response =
                    proto::song_loop::preview_song_loop_response(*started.borrow_and_update());
                if sender.send(Ok(response)).await.is_err() {
                    break;
                }
                tokio::select! {
                    () = sender.closed() => break,
                    _ = &mut ended => break,
                    _ = draining.wait_for(|draining| *draining) => break,
                    changed = started.changed() => if changed.is_err() {
                        break;
                    },
                }
            }
            preview_song_loop::stop(session.id, player.as_ref()).await;
        });
        Ok(Response::new(ReceiverStream::new(receiver)))
    }

    async fn update_song_loop_preview(
        &self,
        request: Request<pb::UpdateSongLoopPreviewRequest>,
    ) -> Result<Response<pb::UpdateSongLoopPreviewResponse>, Status> {
        let request = proto::song_loop::update_song_loop_preview_request(request.into_inner())?;
        update_song_loop_preview::execute(request, self.state.player.as_ref())
            .await
            .into_grpc()?;
        Ok(Response::new(pb::UpdateSongLoopPreviewResponse {}))
    }

    async fn watch_player(
        &self,
        _request: Request<pb::WatchPlayerRequest>,
    ) -> Result<Response<Self::WatchPlayerStream>, Status> {
        let states = watch_player::execute(self.state.player.as_ref());
        // One slot keeps backpressure: a slow reader holds the forwarder, which then sends the
        // latest state instead of queueing every intermediate one.
        let (sender, receiver) = mpsc::channel(1);
        tokio::spawn(forward_player_states(states, self.draining.clone(), sender));
        Ok(Response::new(ReceiverStream::new(receiver)))
    }

    async fn play_collection(
        &self,
        request: Request<pb::PlayCollectionRequest>,
    ) -> Result<Response<pb::PlayCollectionResponse>, Status> {
        let (collection_id, start) = proto::player::play_collection_request(request.get_ref())?;
        play_collection::execute(
            collection_id,
            start,
            &self.state.library,
            self.state.player.as_ref(),
        )
        .await
        .into_grpc()?;
        Ok(Response::new(pb::PlayCollectionResponse {}))
    }

    async fn pause_music(
        &self,
        _request: Request<pb::PauseMusicRequest>,
    ) -> Result<Response<pb::PauseMusicResponse>, Status> {
        self.control(MusicCommand::Pause).await?;
        Ok(Response::new(pb::PauseMusicResponse {}))
    }

    async fn add_music_layer(
        &self,
        request: Request<pb::AddMusicLayerRequest>,
    ) -> Result<Response<pb::AddMusicLayerResponse>, Status> {
        let entry_id = proto::player::add_music_layer_request(request.get_ref())?;
        add_music_layer::execute(entry_id, &self.state.library, self.state.player.as_ref())
            .await
            .into_grpc()?;
        Ok(Response::new(pb::AddMusicLayerResponse {}))
    }

    async fn stop_music_layer(
        &self,
        request: Request<pb::StopMusicLayerRequest>,
    ) -> Result<Response<pb::StopMusicLayerResponse>, Status> {
        let entry_id = proto::player::stop_music_layer_request(request.get_ref())?;
        stop_music_layer::execute(entry_id, self.state.player.as_ref()).await;
        Ok(Response::new(pb::StopMusicLayerResponse {}))
    }

    async fn update_music_layer(
        &self,
        request: Request<pb::UpdateMusicLayerRequest>,
    ) -> Result<Response<pb::UpdateMusicLayerResponse>, Status> {
        let (entry_id, update) = proto::player::update_music_layer_request(request.get_ref())?;
        update_music_layer::execute(entry_id, update, self.state.player.as_ref()).await;
        Ok(Response::new(pb::UpdateMusicLayerResponse {}))
    }

    async fn add_to_queue(
        &self,
        request: Request<pb::AddToQueueRequest>,
    ) -> Result<Response<pb::AddToQueueResponse>, Status> {
        let entry_id = proto::player::add_to_queue_request(request.get_ref())?;
        add_to_queue::execute(entry_id, &self.state.library, self.state.player.as_ref())
            .await
            .into_grpc()?;
        Ok(Response::new(pb::AddToQueueResponse {}))
    }

    async fn remove_from_queue(
        &self,
        request: Request<pb::RemoveFromQueueRequest>,
    ) -> Result<Response<pb::RemoveFromQueueResponse>, Status> {
        let entry_id = proto::player::remove_from_queue_request(request.get_ref())?;
        remove_from_queue::execute(entry_id, self.state.player.as_ref()).await;
        Ok(Response::new(pb::RemoveFromQueueResponse {}))
    }

    async fn resume_music(
        &self,
        _request: Request<pb::ResumeMusicRequest>,
    ) -> Result<Response<pb::ResumeMusicResponse>, Status> {
        self.control(MusicCommand::Resume).await?;
        Ok(Response::new(pb::ResumeMusicResponse {}))
    }

    async fn stop_music(
        &self,
        _request: Request<pb::StopMusicRequest>,
    ) -> Result<Response<pb::StopMusicResponse>, Status> {
        self.control(MusicCommand::Stop).await?;
        Ok(Response::new(pb::StopMusicResponse {}))
    }

    async fn skip_to_next_track(
        &self,
        _request: Request<pb::SkipToNextTrackRequest>,
    ) -> Result<Response<pb::SkipToNextTrackResponse>, Status> {
        self.control(MusicCommand::Next).await?;
        Ok(Response::new(pb::SkipToNextTrackResponse {}))
    }

    async fn skip_to_previous_track(
        &self,
        _request: Request<pb::SkipToPreviousTrackRequest>,
    ) -> Result<Response<pb::SkipToPreviousTrackResponse>, Status> {
        self.control(MusicCommand::Previous).await?;
        Ok(Response::new(pb::SkipToPreviousTrackResponse {}))
    }

    async fn seek_music(
        &self,
        request: Request<pb::SeekMusicRequest>,
    ) -> Result<Response<pb::SeekMusicResponse>, Status> {
        let position = proto::player::seek_music_request(request.into_inner())?;
        self.control(MusicCommand::Seek(position)).await?;
        Ok(Response::new(pb::SeekMusicResponse {}))
    }

    async fn update_player_settings(
        &self,
        request: Request<pb::UpdatePlayerSettingsRequest>,
    ) -> Result<Response<pb::UpdatePlayerSettingsResponse>, Status> {
        let update = proto::player::update_player_settings_request(request.into_inner())?;
        update_player_settings::execute(
            update,
            self.state.player.as_ref(),
            self.state.library.database(),
        )
        .await
        .into_grpc()?;
        Ok(Response::new(pb::UpdatePlayerSettingsResponse {}))
    }

    async fn play_sound(
        &self,
        request: Request<pb::PlaySoundRequest>,
    ) -> Result<Response<pb::PlaySoundResponse>, Status> {
        let entry_id = proto::player::play_sound_request(request.get_ref())?;
        play_entry_sound::execute(entry_id, &self.state.library, self.state.player.as_ref())
            .await
            .into_grpc()?;
        Ok(Response::new(pb::PlaySoundResponse {}))
    }

    async fn stop_sound(
        &self,
        request: Request<pb::StopSoundRequest>,
    ) -> Result<Response<pb::StopSoundResponse>, Status> {
        let entry_id = proto::player::stop_sound_request(request.get_ref())?;
        stop_sound::execute(
            entry_id,
            self.state.player.as_ref(),
            self.state.library.database(),
        )
        .await;
        Ok(Response::new(pb::StopSoundResponse {}))
    }

    async fn pause_sounds(
        &self,
        _request: Request<pb::PauseSoundsRequest>,
    ) -> Result<Response<pb::PauseSoundsResponse>, Status> {
        self.control_sounds(SoundsCommand::Pause).await;
        Ok(Response::new(pb::PauseSoundsResponse {}))
    }

    async fn resume_sounds(
        &self,
        _request: Request<pb::ResumeSoundsRequest>,
    ) -> Result<Response<pb::ResumeSoundsResponse>, Status> {
        self.control_sounds(SoundsCommand::Resume).await;
        Ok(Response::new(pb::ResumeSoundsResponse {}))
    }

    async fn resume_saved_loops(
        &self,
        _request: Request<pb::ResumeSavedLoopsRequest>,
    ) -> Result<Response<pb::ResumeSavedLoopsResponse>, Status> {
        resume_saved_loops::execute(&self.state.library, self.state.player.as_ref())
            .await
            .into_grpc()?;
        Ok(Response::new(pb::ResumeSavedLoopsResponse {}))
    }

    async fn select_output(
        &self,
        request: Request<pb::SelectOutputRequest>,
    ) -> Result<Response<pb::SelectOutputResponse>, Status> {
        let target = proto::player::select_output_request(request.into_inner())?;
        select_output::execute(target, self.state.player.as_ref())
            .await
            .into_grpc()?;
        Ok(Response::new(pb::SelectOutputResponse {}))
    }

    async fn recover_local_output(
        &self,
        _request: Request<pb::RecoverLocalOutputRequest>,
    ) -> Result<Response<pb::RecoverLocalOutputResponse>, Status> {
        tufo_application::playback::recover_local_output::execute(self.state.player.as_ref())
            .await
            .into_grpc()?;
        Ok(Response::new(pb::RecoverLocalOutputResponse {}))
    }

    async fn stand_by(
        &self,
        _request: Request<pb::StandByRequest>,
    ) -> Result<Response<pb::StandByResponse>, Status> {
        stand_by::execute(self.state.player.as_ref()).await;
        Ok(Response::new(pb::StandByResponse {}))
    }

    async fn open_browser(
        &self,
        _request: Request<pb::OpenBrowserRequest>,
    ) -> Result<Response<pb::OpenBrowserResponse>, Status> {
        open_browser::execute(self.state.player.as_ref())
            .await
            .into_grpc()?;
        Ok(Response::new(pb::OpenBrowserResponse {}))
    }

    async fn close_browser(
        &self,
        _request: Request<pb::CloseBrowserRequest>,
    ) -> Result<Response<pb::CloseBrowserResponse>, Status> {
        close_browser::execute(self.state.player.as_ref()).await;
        Ok(Response::new(pb::CloseBrowserResponse {}))
    }

    async fn get_player_diagnostics(
        &self,
        _request: Request<pb::GetPlayerDiagnosticsRequest>,
    ) -> Result<Response<pb::GetPlayerDiagnosticsResponse>, Status> {
        let diagnostics = get_diagnostics::execute(self.state.player.as_ref());
        Ok(Response::new(proto::player::diagnostics_response(
            &diagnostics,
        )))
    }
}

async fn forward_player_states(
    mut states: watch::Receiver<PlayerState>,
    mut draining: watch::Receiver<bool>,
    sender: mpsc::Sender<Result<pb::WatchPlayerResponse, Status>>,
) {
    loop {
        let message = proto::player::watch_player_response(&states.borrow_and_update());
        tokio::select! {
            sent = sender.send(Ok(message)) => {
                if sent.is_err() {
                    return;
                }
            }
            _ = draining.wait_for(|draining| *draining) => return,
        }
        tokio::select! {
            changed = states.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = draining.wait_for(|draining| *draining) => return,
            () = sender.closed() => return,
        }
    }
}
