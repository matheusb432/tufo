//! A legacy status response must stop nested updates before a library RPC is sent.

use std::time::Duration;

use tonic::{Request, Response, Status, transport::Server};
use tufo_client::{
    ClientError, Code, TufoClient,
    library::{EntryOrderPatch, SoundSettingsPatch, UpdateCollection, UpdateCollectionEntry},
};
use tufo_local_transport::{LocalEndpoint, LocalListener};
use tufo_models::library::{CollectionEntryId, CollectionId, EntryOrder, SoundSettings, Title};
use tufo_wire::pb::{
    self,
    server_service_server::{ServerService, ServerServiceServer},
};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

struct StatusOnly;

#[tonic::async_trait]
impl ServerService for StatusOnly {
    async fn get_server_status(
        &self,
        _request: Request<pb::GetServerStatusRequest>,
    ) -> Result<Response<pb::GetServerStatusResponse>, Status> {
        Ok(Response::new(pb::GetServerStatusResponse {
            version: "legacy-test".to_owned(),
            process_id: std::process::id(),
            uptime: Some(
                Duration::ZERO
                    .try_into()
                    .map_err(|_| Status::internal("invalid fixture uptime"))?,
            ),
            discord_bot_token: pb::DiscordBotTokenStatus::Missing.into(),
            discord_connection: pb::DiscordConnectionStatus::NotConfigured.into(),
            ..pb::GetServerStatusResponse::default()
        }))
    }

    async fn stop_server(
        &self,
        _request: Request<pb::StopServerRequest>,
    ) -> Result<Response<pb::StopServerResponse>, Status> {
        Err(Status::unimplemented("status-only fixture"))
    }

    async fn set_discord_bot_token(
        &self,
        _request: Request<pb::SetDiscordBotTokenRequest>,
    ) -> Result<Response<pb::SetDiscordBotTokenResponse>, Status> {
        Err(Status::unimplemented("status-only fixture"))
    }

    async fn disable_discord_connection(
        &self,
        _request: Request<pb::DisableDiscordConnectionRequest>,
    ) -> Result<Response<pb::DisableDiscordConnectionResponse>, Status> {
        Err(Status::unimplemented("status-only fixture"))
    }

    async fn enable_discord_connection(
        &self,
        _request: Request<pb::EnableDiscordConnectionRequest>,
    ) -> Result<Response<pb::EnableDiscordConnectionResponse>, Status> {
        Err(Status::unimplemented("status-only fixture"))
    }
}

struct StatusOnlyServer {
    endpoint: LocalEndpoint,
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    _directory: tempfile::TempDir,
}

impl StatusOnlyServer {
    async fn start() -> anyhow::Result<Self> {
        let directory = tempfile::tempdir()?;
        let endpoint = LocalEndpoint::from_root(directory.path().join("runtime"))?;
        let listener = LocalListener::bind(&endpoint, TEST_TIMEOUT).await?;
        #[cfg(unix)]
        let (incoming, ownership) = listener.into_parts();
        #[cfg(windows)]
        let (incoming, ()) = listener.into_parts();
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            #[cfg(unix)]
            let _ownership = ownership;
            Server::builder()
                .add_service(ServerServiceServer::new(StatusOnly))
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = stopped.await;
                })
                .await
        });
        Ok(Self {
            endpoint,
            shutdown,
            server,
            _directory: directory,
        })
    }

    async fn stop(self) -> anyhow::Result<()> {
        let _ = self.shutdown.send(());
        tokio::time::timeout(TEST_TIMEOUT, self.server).await???;
        Ok(())
    }
}

#[tokio::test]
async fn legacy_capabilities_reject_nested_updates_before_attempting_library_rpcs()
-> anyhow::Result<()> {
    let server = StatusOnlyServer::start().await?;
    let client = TufoClient::connect_lazy(&server.endpoint)?;
    let collection_id = CollectionId::from_uuid("00000000-0000-0000-0000-000000000001".parse()?);
    let entry_id = CollectionEntryId::from_uuid("00000000-0000-0000-0000-000000000002".parse()?);
    assert!(!client.server_status().await?.supports_library_patches);

    let order_update = UpdateCollection {
        title: Some(Title::try_new("Do not change this title".to_owned())?),
        order_patch: Some(EntryOrderPatch {
            descending: Some(false),
            ..EntryOrderPatch::default()
        }),
        ..UpdateCollection::default()
    };
    let order_result = tokio::time::timeout(
        TEST_TIMEOUT,
        client.update_collection(collection_id, &order_update),
    )
    .await?;
    assert!(
        matches!(&order_result, Err(ClientError::LibraryPatchesUnsupported)),
        "{order_result:?}"
    );

    let sound_update = UpdateCollectionEntry {
        title: Some(Title::try_new("Do not change this title".to_owned())?),
        sound_patch: Some(SoundSettingsPatch {
            looping: Some(false),
            ..SoundSettingsPatch::default()
        }),
        ..UpdateCollectionEntry::default()
    };
    let sound_result = tokio::time::timeout(
        TEST_TIMEOUT,
        client.update_collection_entry(entry_id, &sound_update),
    )
    .await?;
    assert!(
        matches!(&sound_result, Err(ClientError::LibraryPatchesUnsupported)),
        "{sound_result:?}"
    );

    let full_order = UpdateCollection {
        order: Some(EntryOrder::default()),
        ..UpdateCollection::default()
    };
    let full_order_result = client.update_collection(collection_id, &full_order).await;
    assert!(
        matches!(&full_order_result, Err(ClientError::Status(status))
        if status.code() == Code::Unimplemented),
        "{full_order_result:?}"
    );
    let full_sound = UpdateCollectionEntry {
        sound: Some(SoundSettings::default()),
        ..UpdateCollectionEntry::default()
    };
    let full_sound_result = client.update_collection_entry(entry_id, &full_sound).await;
    assert!(
        matches!(&full_sound_result, Err(ClientError::Status(status))
        if status.code() == Code::Unimplemented),
        "{full_sound_result:?}"
    );
    drop(client);
    server.stop().await
}
