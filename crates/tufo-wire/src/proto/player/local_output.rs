use tufo_models::playback::local_output::{
    LocalOutputFailure, LocalOutputFailureReason, LocalOutputIdentity, LocalOutputState,
};

use crate::{pb, proto::InvalidMessage};

pub(super) fn message(state: &LocalOutputState) -> pb::LocalOutputHealth {
    use pb::local_output_health::State;
    pb::LocalOutputHealth {
        state: Some(match state {
            LocalOutputState::Released => State::Released(pb::LocalOutputReleased {}),
            LocalOutputState::Ready(identity) => State::Ready(identity_message(identity)),
            LocalOutputState::Recovering { previous_failure } => {
                State::Recovering(pb::LocalOutputRecovering {
                    previous_failure: previous_failure.as_ref().map(failure_message),
                })
            }
            LocalOutputState::Failed(failure) => State::Failed(failure_message(failure)),
        }),
    }
}

fn identity_message(identity: &LocalOutputIdentity) -> pb::LocalOutputIdentity {
    pb::LocalOutputIdentity {
        backend: identity.backend.clone(),
        device: identity.device.clone(),
        sample_rate: identity.sample_rate,
    }
}

fn failure_message(failure: &LocalOutputFailure) -> pb::LocalOutputFailure {
    let reason = match failure.reason {
        LocalOutputFailureReason::NoDevice => pb::LocalOutputFailureReason::NoDevice,
        LocalOutputFailureReason::TargetMissing => pb::LocalOutputFailureReason::TargetMissing,
        LocalOutputFailureReason::DeviceUnavailable => {
            pb::LocalOutputFailureReason::DeviceUnavailable
        }
        LocalOutputFailureReason::StreamInvalidated => {
            pb::LocalOutputFailureReason::StreamInvalidated
        }
        LocalOutputFailureReason::DeviceChanged => pb::LocalOutputFailureReason::DeviceChanged,
        LocalOutputFailureReason::CallbackStopped => pb::LocalOutputFailureReason::CallbackStopped,
        LocalOutputFailureReason::OpenFailed => pb::LocalOutputFailureReason::OpenFailed,
        LocalOutputFailureReason::OpenTimedOut => pb::LocalOutputFailureReason::OpenTimedOut,
    };
    pb::LocalOutputFailure {
        reason: reason.into(),
        identity: failure.identity.as_ref().map(identity_message),
        detail: failure.detail.clone(),
    }
}

pub(super) fn state(message: pb::LocalOutputHealth) -> Result<LocalOutputState, InvalidMessage> {
    use pb::local_output_health::State;
    Ok(
        match message.state.ok_or(InvalidMessage {
            field: "local_output.state",
        })? {
            State::Released(_) => LocalOutputState::Released,
            State::Ready(value) => LocalOutputState::Ready(identity(value)?),
            State::Recovering(value) => LocalOutputState::Recovering {
                previous_failure: value.previous_failure.map(failure).transpose()?,
            },
            State::Failed(value) => LocalOutputState::Failed(failure(value)?),
        },
    )
}

fn identity(message: pb::LocalOutputIdentity) -> Result<LocalOutputIdentity, InvalidMessage> {
    if message.backend.is_empty() || message.sample_rate == Some(0) {
        return Err(InvalidMessage {
            field: "local_output.identity",
        });
    }
    Ok(LocalOutputIdentity {
        backend: message.backend,
        device: message.device,
        sample_rate: message.sample_rate,
    })
}

fn failure(message: pb::LocalOutputFailure) -> Result<LocalOutputFailure, InvalidMessage> {
    let reason = match pb::LocalOutputFailureReason::try_from(message.reason) {
        Ok(pb::LocalOutputFailureReason::NoDevice) => LocalOutputFailureReason::NoDevice,
        Ok(pb::LocalOutputFailureReason::TargetMissing) => LocalOutputFailureReason::TargetMissing,
        Ok(pb::LocalOutputFailureReason::DeviceUnavailable) => {
            LocalOutputFailureReason::DeviceUnavailable
        }
        Ok(pb::LocalOutputFailureReason::StreamInvalidated) => {
            LocalOutputFailureReason::StreamInvalidated
        }
        Ok(pb::LocalOutputFailureReason::DeviceChanged) => LocalOutputFailureReason::DeviceChanged,
        Ok(pb::LocalOutputFailureReason::CallbackStopped) => {
            LocalOutputFailureReason::CallbackStopped
        }
        Ok(pb::LocalOutputFailureReason::OpenFailed) => LocalOutputFailureReason::OpenFailed,
        Ok(pb::LocalOutputFailureReason::OpenTimedOut) => LocalOutputFailureReason::OpenTimedOut,
        _ => {
            return Err(InvalidMessage {
                field: "local_output.failure.reason",
            });
        }
    };
    Ok(LocalOutputFailure {
        reason,
        identity: message.identity.map(identity).transpose()?,
        detail: message.detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_snapshots_retain_current_failure_and_recovery_context() {
        let identity = LocalOutputIdentity {
            backend: "PipeWire".to_owned(),
            device: Some("test-sink".to_owned()),
            sample_rate: Some(44_100),
        };
        let failure = LocalOutputFailure {
            reason: LocalOutputFailureReason::TargetMissing,
            identity: Some(identity.clone()),
            detail: "target disappeared".to_owned(),
        };
        for expected in [
            LocalOutputState::Released,
            LocalOutputState::Ready(identity),
            LocalOutputState::Failed(failure.clone()),
            LocalOutputState::Recovering {
                previous_failure: Some(failure),
            },
        ] {
            assert_eq!(state(message(&expected)).unwrap(), expected);
        }
    }

    #[test]
    fn malformed_health_never_decodes_as_ready_or_released() {
        assert!(state(pb::LocalOutputHealth { state: None }).is_err());
        assert!(
            state(pb::LocalOutputHealth {
                state: Some(pb::local_output_health::State::Failed(
                    pb::LocalOutputFailure {
                        reason: 999,
                        identity: None,
                        detail: String::new()
                    }
                ))
            })
            .is_err()
        );
        assert!(
            identity(pb::LocalOutputIdentity {
                backend: "test".to_owned(),
                device: None,
                sample_rate: Some(0)
            })
            .is_err()
        );
    }
}
