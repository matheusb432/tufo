mod library;
mod player;
mod server_control;
mod snapshot_stream;
mod voice_channel;

pub(crate) use library::LibraryGrpcService;
pub(crate) use player::PlayerGrpcService;
pub(crate) use server_control::ServerControlGrpcService;
use tonic::Status;
use tufo_application::error::{ApplicationError, ErrorClass};
pub(crate) use voice_channel::VoiceChannelGrpcService;

/// Logs a failed request once, with its causes, and returns its caller-safe status.
fn failure_status(error: &impl ApplicationError) -> Status {
    let metadata = error.metadata();
    let mut causes = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        causes.push_str(": ");
        causes.push_str(&cause.to_string());
        source = cause.source();
    }
    tracing::warn!(reason = metadata.reason, error = causes, "request failed");
    let code = match metadata.class {
        ErrorClass::InvalidArgument => tonic::Code::InvalidArgument,
        ErrorClass::NotFound => tonic::Code::NotFound,
        ErrorClass::FailedPrecondition => tonic::Code::FailedPrecondition,
        ErrorClass::Unavailable => tonic::Code::Unavailable,
        ErrorClass::Internal => tonic::Code::Internal,
    };
    tufo_wire::error::status(code, error.to_string(), metadata.reason)
}

trait GrpcResultExt<T> {
    fn into_grpc(self) -> Result<T, Status>;
}

impl<T, E: ApplicationError> GrpcResultExt<T> for Result<T, E> {
    fn into_grpc(self) -> Result<T, Status> {
        self.map_err(|error| failure_status(&error))
    }
}

#[cfg(test)]
mod tests {
    use tufo_application::playback::{
        play_collection::PlayCollectionError, play_queue::PlayQueueError, player::PlayAudioError,
    };

    use super::failure_status;

    #[test]
    fn transparent_failures_keep_the_shared_reason_across_transport() {
        let failure = PlayCollectionError::Play(PlayQueueError::Audio(PlayAudioError::Unreadable(
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        )));
        let status = failure_status(&failure);
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert_eq!(
            tufo_wire::error::reason(&status).as_deref(),
            Some(PlayAudioError::UNREADABLE)
        );
        assert_eq!(status.message(), "the audio file cannot be read");
    }
}
