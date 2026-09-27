//! The status-detail envelope; application errors own the reason vocabulary.

use prost::Message as _;
use tonic::{Code, Status};

use crate::pb;

const REASON_LENGTH_MAX: usize = 128;

#[must_use]
pub fn status(code: Code, message: impl Into<String>, reason: &str) -> Status {
    let details = pb::ErrorDetails {
        reason_code: reason.to_owned(),
    }
    .encode_to_vec();
    Status::with_details(code, message, details.into())
}

/// Unrecognized codes remain data for the caller; malformed details carry no reason.
#[must_use]
pub fn reason(status: &Status) -> Option<String> {
    if status.details().len() > REASON_LENGTH_MAX + 8 {
        return None;
    }
    let details = pb::ErrorDetails::decode(status.details()).ok()?;
    let reason = details.reason_code;
    (!reason.is_empty()
        && reason.len() <= REASON_LENGTH_MAX
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'.' | b'_')))
    .then_some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_and_legacy_details_carry_no_reason() {
        for details in [vec![0xff], vec![8, 11], vec![], vec![0; 137]] {
            let status = Status::with_details(Code::Internal, "diagnostic", details.into());
            assert_eq!(reason(&status), None);
        }
    }

    #[test]
    fn reason_codes_round_trip_without_a_transport_registry() {
        let failure = status(
            Code::FailedPrecondition,
            "diagnostic",
            "future_operation.rejected",
        );
        assert_eq!(
            reason(&failure).as_deref(),
            Some("future_operation.rejected")
        );
        assert_eq!(failure.message(), "diagnostic");
    }
}
