use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tufo_application::library::{
    add_files_to_collection, add_songs_to_playlist, create_collection, delete_collection,
    delete_collection_entry, delete_library_folder, delete_song, import_kenku_library,
    import_paths, sync_library, update_collection, update_collection_entry,
};
use tufo_infra::kenku::DiskKenkuReader;
use tufo_wire::{
    library::LibrarySnapshot,
    pb::{self, library_service_server::LibraryService},
    proto,
};

use super::GrpcResultExt as _;
use crate::AppState;

pub(crate) struct LibraryGrpcService {
    state: AppState,
    /// Turns true when the server starts draining, which ends every library stream.
    draining: watch::Receiver<bool>,
}

impl LibraryGrpcService {
    pub(crate) fn new(state: AppState, draining: watch::Receiver<bool>) -> Self {
        Self { state, draining }
    }
}

#[tonic::async_trait]
impl LibraryService for LibraryGrpcService {
    type WatchLibraryStream = ReceiverStream<Result<pb::WatchLibraryResponse, Status>>;

    async fn watch_library(
        &self,
        _request: Request<pb::WatchLibraryRequest>,
    ) -> Result<Response<Self::WatchLibraryStream>, Status> {
        let snapshots = self.state.library.watch();
        // One slot keeps backpressure: a slow reader holds the forwarder, which then sends the
        // latest snapshot instead of queueing every intermediate one.
        let (sender, receiver) = mpsc::channel(1);
        tokio::spawn(forward_snapshots(snapshots, self.draining.clone(), sender));
        Ok(Response::new(ReceiverStream::new(receiver)))
    }

    async fn import_paths(
        &self,
        request: Request<pb::ImportPathsRequest>,
    ) -> Result<Response<pb::ImportPathsResponse>, Status> {
        let request = proto::library::import_paths_request(request.into_inner())?;
        let report = import_paths::execute(request, &self.state.library)
            .await
            .into_grpc()?;
        Ok(Response::new(pb::ImportPathsResponse {
            report: Some(proto::library::import_report_message(&report)),
        }))
    }

    async fn add_files_to_collection(
        &self,
        request: Request<pb::AddFilesToCollectionRequest>,
    ) -> Result<Response<pb::AddFilesToCollectionResponse>, Status> {
        let request = proto::library::add_files_to_collection_request(request.into_inner())?;
        let report = add_files_to_collection::execute(request, &self.state.library)
            .await
            .into_grpc()?;
        Ok(Response::new(pb::AddFilesToCollectionResponse {
            report: Some(proto::library::import_report_message(&report)),
        }))
    }

    async fn create_collection(
        &self,
        request: Request<pb::CreateCollectionRequest>,
    ) -> Result<Response<pb::CreateCollectionResponse>, Status> {
        let request = proto::library::create_collection_request(request.into_inner())?;
        let id = create_collection::execute(request, &self.state.library)
            .await
            .into_grpc()?;
        Ok(Response::new(proto::library::create_collection_response(
            id,
        )))
    }

    async fn update_collection(
        &self,
        request: Request<pb::UpdateCollectionRequest>,
    ) -> Result<Response<pb::UpdateCollectionResponse>, Status> {
        let (id, update) = proto::library::update_collection_request(request.into_inner())?;
        update_collection::execute(id, update, &self.state.library)
            .await
            .into_grpc()?;
        Ok(Response::new(pb::UpdateCollectionResponse {}))
    }

    async fn delete_collection(
        &self,
        request: Request<pb::DeleteCollectionRequest>,
    ) -> Result<Response<pb::DeleteCollectionResponse>, Status> {
        let id = proto::library::delete_collection_request(request.get_ref())?;
        delete_collection::execute(id, &self.state.library)
            .await
            .into_grpc()?;
        Ok(Response::new(pb::DeleteCollectionResponse {}))
    }

    async fn update_collection_entry(
        &self,
        request: Request<pb::UpdateCollectionEntryRequest>,
    ) -> Result<Response<pb::UpdateCollectionEntryResponse>, Status> {
        let (id, update) = proto::library::update_collection_entry_request(request.into_inner())?;
        update_collection_entry::execute(
            id,
            update,
            &self.state.library,
            self.state.player.as_ref(),
        )
        .await
        .into_grpc()?;
        Ok(Response::new(pb::UpdateCollectionEntryResponse {}))
    }

    async fn delete_collection_entry(
        &self,
        request: Request<pb::DeleteCollectionEntryRequest>,
    ) -> Result<Response<pb::DeleteCollectionEntryResponse>, Status> {
        let id = proto::library::delete_collection_entry_request(request.get_ref())?;
        delete_collection_entry::execute(id, &self.state.library)
            .await
            .into_grpc()?;
        Ok(Response::new(pb::DeleteCollectionEntryResponse {}))
    }

    async fn add_songs_to_playlist(
        &self,
        request: Request<pb::AddSongsToPlaylistRequest>,
    ) -> Result<Response<pb::AddSongsToPlaylistResponse>, Status> {
        let request = proto::library::add_songs_to_playlist_request(request.into_inner())?;
        let songs_added = add_songs_to_playlist::execute(request, &self.state.library)
            .await
            .into_grpc()?;
        Ok(Response::new(
            proto::library::add_songs_to_playlist_response(songs_added),
        ))
    }

    async fn delete_song(
        &self,
        request: Request<pb::DeleteSongRequest>,
    ) -> Result<Response<pb::DeleteSongResponse>, Status> {
        let id = proto::library::delete_song_request(request.get_ref())?;
        delete_song::execute(id, &self.state.library)
            .await
            .into_grpc()?;
        Ok(Response::new(pb::DeleteSongResponse {}))
    }

    async fn delete_library_folder(
        &self,
        request: Request<pb::DeleteLibraryFolderRequest>,
    ) -> Result<Response<pb::DeleteLibraryFolderResponse>, Status> {
        let id = proto::library::delete_library_folder_request(request.get_ref())?;
        delete_library_folder::execute(id, &self.state.library)
            .await
            .into_grpc()?;
        Ok(Response::new(pb::DeleteLibraryFolderResponse {}))
    }

    async fn rescan_library(
        &self,
        _request: Request<pb::RescanLibraryRequest>,
    ) -> Result<Response<pb::RescanLibraryResponse>, Status> {
        sync_library::execute(&self.state.library)
            .await
            .into_grpc()?;
        Ok(Response::new(pb::RescanLibraryResponse {}))
    }

    async fn import_kenku_library(
        &self,
        request: Request<pb::ImportKenkuLibraryRequest>,
    ) -> Result<Response<pb::ImportKenkuLibraryResponse>, Status> {
        let request = proto::library::import_kenku_library_request(request.into_inner())?;
        let report = import_kenku_library::execute(
            request,
            &self.state.library,
            self.state.player.as_ref(),
            &DiskKenkuReader,
            &self.state.discord_credentials,
        )
        .await
        .into_grpc()?;
        Ok(Response::new(pb::ImportKenkuLibraryResponse {
            report: Some(proto::library::kenku_import_report_message(&report)),
        }))
    }
}

async fn forward_snapshots(
    mut snapshots: watch::Receiver<Arc<LibrarySnapshot>>,
    mut draining: watch::Receiver<bool>,
    sender: mpsc::Sender<Result<pb::WatchLibraryResponse, Status>>,
) {
    loop {
        let snapshot = Arc::clone(&snapshots.borrow_and_update());
        let message = proto::library::watch_library_response(&snapshot);
        tokio::select! {
            sent = sender.send(Ok(message)) => {
                if sent.is_err() {
                    return;
                }
            }
            _ = draining.wait_for(|draining| *draining) => return,
        }
        tokio::select! {
            changed = snapshots.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = draining.wait_for(|draining| *draining) => return,
            () = sender.closed() => return,
        }
    }
}
