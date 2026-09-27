use tonic::{Request, Response, Status};
use tufo_application::voice_channels::list_voice_channels;
use tufo_wire::{
    pb::{self, voice_channel_service_server::VoiceChannelService},
    proto,
};

use super::GrpcResultExt as _;
use crate::AppState;

pub(crate) struct VoiceChannelGrpcService {
    state: AppState,
}

impl VoiceChannelGrpcService {
    pub(crate) fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl VoiceChannelService for VoiceChannelGrpcService {
    async fn list_voice_channels(
        &self,
        request: Request<pb::ListVoiceChannelsRequest>,
    ) -> Result<Response<pb::ListVoiceChannelsResponse>, Status> {
        let request = proto::voice_channel::list_voice_channels_request(request.get_ref())?;
        let page = list_voice_channels::execute(&request, &self.state.discord).into_grpc()?;
        Ok(Response::new(
            proto::voice_channel::list_voice_channels_response(page),
        ))
    }
}
