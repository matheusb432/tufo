use tonic::{Request, Response, Status};
use tufo_application::{
    discord::{disable_discord_connection, enable_discord_connection, set_discord_bot_token},
    server::get_server_status,
};
use tufo_wire::{
    pb::{self, server_service_server::ServerService},
    proto,
};

use super::GrpcResultExt as _;
use crate::AppState;

pub(crate) struct ServerControlGrpcService {
    state: AppState,
}

impl ServerControlGrpcService {
    pub(crate) fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl ServerService for ServerControlGrpcService {
    async fn get_server_status(
        &self,
        _request: Request<pb::GetServerStatusRequest>,
    ) -> Result<Response<pb::GetServerStatusResponse>, Status> {
        let process_id = std::num::NonZeroU32::new(std::process::id())
            .ok_or_else(|| Status::internal("the process has no identifier"))?;
        let status = get_server_status::execute(
            env!("CARGO_PKG_VERSION").to_owned(),
            process_id,
            self.state.started_at.elapsed(),
            &self.state.discord_credentials,
            &self.state.discord,
        )
        .await
        .into_grpc()?;
        Ok(Response::new(proto::server::server_status_message(status)))
    }

    async fn stop_server(
        &self,
        _request: Request<pb::StopServerRequest>,
    ) -> Result<Response<pb::StopServerResponse>, Status> {
        tracing::info!("a client asked the server to stop");
        self.state.stop_requested.notify_one();
        Ok(Response::new(pb::StopServerResponse {}))
    }

    async fn set_discord_bot_token(
        &self,
        request: Request<pb::SetDiscordBotTokenRequest>,
    ) -> Result<Response<pb::SetDiscordBotTokenResponse>, Status> {
        set_discord_bot_token::execute(
            request.into_inner().bot_token,
            &self.state.discord_credentials,
        )
        .await
        .into_grpc()?;
        Ok(Response::new(pb::SetDiscordBotTokenResponse {}))
    }

    async fn disable_discord_connection(
        &self,
        _request: Request<pb::DisableDiscordConnectionRequest>,
    ) -> Result<Response<pb::DisableDiscordConnectionResponse>, Status> {
        disable_discord_connection::execute(&self.state.discord, self.state.player.as_ref()).await;
        Ok(Response::new(pb::DisableDiscordConnectionResponse {}))
    }

    async fn enable_discord_connection(
        &self,
        _request: Request<pb::EnableDiscordConnectionRequest>,
    ) -> Result<Response<pb::EnableDiscordConnectionResponse>, Status> {
        enable_discord_connection::execute(&self.state.discord, self.state.player.as_ref()).await;
        Ok(Response::new(pb::EnableDiscordConnectionResponse {}))
    }
}
