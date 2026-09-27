use std::collections::BTreeSet;

use tokio_stream::StreamExt as _;
use tonic_health::{
    ServingStatus,
    pb::{HealthCheckRequest, health_client::HealthClient},
};
use tonic_reflection::pb::v1::{
    ServerReflectionRequest, server_reflection_client::ServerReflectionClient,
    server_reflection_request::MessageRequest, server_reflection_response::MessageResponse,
};

mod support;

use support::TestServer;

#[tokio::test]
async fn serves_health_on_the_private_socket_and_removes_it_on_shutdown() -> anyhow::Result<()> {
    let server = TestServer::start().await?;
    let socket_path = server.endpoint.path().to_path_buf();
    let mut health = HealthClient::new(server.channel().await?);

    let response = health
        .check(HealthCheckRequest {
            service: String::new(),
        })
        .await?
        .into_inner();
    drop(health);
    server.stop().await?;

    assert_eq!(response.status, ServingStatus::Serving as i32);
    assert!(!socket_path.exists());
    Ok(())
}

#[tokio::test]
async fn reflection_lists_exactly_the_registered_services() -> anyhow::Result<()> {
    let server = TestServer::start().await?;
    let mut reflection = ServerReflectionClient::new(server.channel().await?);

    let request = ServerReflectionRequest {
        host: String::new(),
        message_request: Some(MessageRequest::ListServices(String::new())),
    };
    let mut responses = reflection
        .server_reflection_info(tokio_stream::once(request))
        .await?
        .into_inner();
    let response = responses
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("reflection closed without a response"))??;
    drop(responses);
    drop(reflection);
    server.stop().await?;

    let Some(MessageResponse::ListServicesResponse(services)) = response.message_response else {
        anyhow::bail!("reflection did not list services");
    };
    let names = services
        .service
        .into_iter()
        .map(|service| service.name)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        BTreeSet::from([
            "grpc.health.v1.Health".to_owned(),
            "grpc.reflection.v1.ServerReflection".to_owned(),
            "tufo.v1.LibraryService".to_owned(),
            "tufo.v1.PlayerService".to_owned(),
            "tufo.v1.ServerService".to_owned(),
            "tufo.v1.VoiceChannelService".to_owned(),
        ])
    );
    Ok(())
}
