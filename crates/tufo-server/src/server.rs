use std::{future::Future, time::Duration};

use anyhow::Context as _;
use tonic::{server::NamedService, transport::Server};
use tufo_infra::player::outputs::AudioOutputs;
use tufo_local_transport::LocalListener;
use tufo_wire::{
    FILE_DESCRIPTOR_SET,
    pb::{
        library_service_server::LibraryServiceServer, player_service_server::PlayerServiceServer,
        server_service_server::ServerServiceServer,
        voice_channel_service_server::VoiceChannelServiceServer,
    },
};

use crate::{
    AppState,
    services::{
        LibraryGrpcService, PlayerGrpcService, ServerControlGrpcService, VoiceChannelGrpcService,
    },
};

const MAX_CONCURRENT_REQUESTS_PER_CONNECTION: usize = 16;
const MAX_REQUEST_MESSAGE_SIZE: usize = 64 * 1024;
/// A large Kenku library can require thousands of explicit collection choices in one request.
const MAX_LIBRARY_REQUEST_MESSAGE_SIZE: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_MESSAGE_SIZE: usize = 1024 * 1024;
/// A library snapshot carries every entry; 64 MiB holds a few hundred thousand of them.
const MAX_LIBRARY_RESPONSE_MESSAGE_SIZE: usize = 64 * 1024 * 1024;
const APPLICATION_SERVICE_NAMES: [&str; 4] = [
    LibraryServiceServer::<LibraryGrpcService>::NAME,
    PlayerServiceServer::<PlayerGrpcService>::NAME,
    ServerServiceServer::<ServerControlGrpcService>::NAME,
    VoiceChannelServiceServer::<VoiceChannelGrpcService>::NAME,
];

/// Serves every service on `listener` until `shutdown` resolves or a client asks it to stop.
/// Draining ends playback streams at once and gives other requests at most `shutdown_grace_period`.
pub async fn serve<O: AudioOutputs>(
    listener: LocalListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
    shutdown_grace_period: Duration,
    state: AppState<O>,
) -> anyhow::Result<()> {
    let (incoming, _ownership) = listener.into_parts();
    let (health_reporter, health_server) = tonic_health::server::health_reporter();
    publish_health(&health_reporter, tonic_health::ServingStatus::Serving).await;
    let reflection_server = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
        .register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET)
        .build_v1()
        .context("building the gRPC reflection service")?;
    let (draining_sender, draining) = tokio::sync::watch::channel(false);
    let player_server =
        PlayerServiceServer::new(PlayerGrpcService::new(state.clone(), draining.clone()))
            .max_decoding_message_size(MAX_REQUEST_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_RESPONSE_MESSAGE_SIZE);
    let library_server =
        LibraryServiceServer::new(LibraryGrpcService::new(state.clone(), draining))
            .max_decoding_message_size(MAX_LIBRARY_REQUEST_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_LIBRARY_RESPONSE_MESSAGE_SIZE);
    let server_control_server =
        ServerServiceServer::new(ServerControlGrpcService::new(state.clone()))
            .max_decoding_message_size(MAX_REQUEST_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_RESPONSE_MESSAGE_SIZE);
    let voice_channel_server =
        VoiceChannelServiceServer::new(VoiceChannelGrpcService::new(state.clone()))
            .max_decoding_message_size(MAX_REQUEST_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_RESPONSE_MESSAGE_SIZE);

    let (drain_started_sender, drain_started_receiver) = tokio::sync::oneshot::channel();
    let stop_requests = state;
    let shutdown = async move {
        tokio::select! {
            () = shutdown => {}
            () = stop_requests.stop_requested() => {}
        }
        publish_health(&health_reporter, tonic_health::ServingStatus::NotServing).await;
        draining_sender.send_replace(true);
        if let Err(error) =
            tufo_application::library::import_kenku_library::interrupt(&stop_requests.library).await
        {
            tracing::error!(%error, "cannot interrupt the Kenku FM import at shutdown");
        }
        let _ = drain_started_sender.send(());
    };
    let grpc_server = Server::builder()
        .concurrency_limit_per_connection(MAX_CONCURRENT_REQUESTS_PER_CONNECTION)
        .add_service(health_server)
        .add_service(reflection_server)
        .add_service(library_server)
        .add_service(player_server)
        .add_service(server_control_server)
        .add_service(voice_channel_server)
        .serve_with_incoming_shutdown(incoming, shutdown);
    tokio::pin!(grpc_server);

    let result = tokio::select! {
        result = &mut grpc_server => result,
        _ = drain_started_receiver => {
            if let Ok(result) = tokio::time::timeout(shutdown_grace_period, &mut grpc_server).await {
                result
            } else {
                tracing::warn!(
                    ?shutdown_grace_period,
                    "gRPC connections exceeded the shutdown grace period"
                );
                Ok(())
            }
        }
    };
    result.context("serving gRPC on the local socket")
}

async fn publish_health(
    reporter: &tonic_health::server::HealthReporter,
    status: tonic_health::ServingStatus,
) {
    for service_name in APPLICATION_SERVICE_NAMES {
        reporter.set_service_status(service_name, status).await;
    }
    reporter.set_service_status("", status).await;
}
