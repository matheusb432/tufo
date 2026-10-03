//! Explicit mappings between protobuf messages and domain or operation types.
//!
//! Servers parse requests into domain values and reject malformed ones with
//! `INVALID_ARGUMENT`; clients decode responses and treat malformed ones as protocol failures.

pub mod library;
pub mod player;
pub mod server;
pub mod song_loop;
pub mod voice_channel;

use std::{
    fmt::Display,
    num::NonZeroU64,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tonic::Status;
use tufo_models::discord::{GuildId, VoiceChannelId, VoiceChannelTarget};

/// A server response the client cannot decode into domain values.
#[derive(Debug, thiserror::Error)]
#[error("the server sent an invalid {field}")]
pub struct InvalidMessage {
    field: &'static str,
}

fn invalid(field: &str, reason: impl Display) -> Status {
    Status::invalid_argument(format!("invalid {field}: {reason}"))
}

fn required<T>(field: &str, value: Option<T>) -> Result<T, Status> {
    value.ok_or_else(|| Status::invalid_argument(format!("{field} is required")))
}

fn voice_channel_target(
    guild_id: u64,
    channel_id: u64,
) -> Result<VoiceChannelTarget, &'static str> {
    Ok(VoiceChannelTarget {
        guild_id: GuildId::new(NonZeroU64::new(guild_id).ok_or("guild_id must be non-zero")?),
        channel_id: VoiceChannelId::new(
            NonZeroU64::new(channel_id).ok_or("channel_id must be non-zero")?,
        ),
    })
}

fn duration_message(duration: Duration) -> prost_types::Duration {
    prost_types::Duration {
        seconds: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(duration.subsec_nanos()).unwrap_or(0),
    }
}

/// A non-negative protobuf duration, or `None` for a negative or malformed one.
fn duration_value(message: prost_types::Duration) -> Option<Duration> {
    let seconds = u64::try_from(message.seconds).ok()?;
    let nanos = u32::try_from(message.nanos).ok()?;
    (nanos < 1_000_000_000).then(|| Duration::new(seconds, nanos))
}

/// Accepts only the hyphenated lowercase form these messages carry.
fn parse_uuid(text: &str) -> Option<uuid::Uuid> {
    let uuid = uuid::Uuid::parse_str(text).ok()?;
    (uuid.hyphenated().to_string() == text).then_some(uuid)
}

fn timestamp_message(time: SystemTime) -> prost_types::Timestamp {
    let (seconds, nanos) = match time.duration_since(UNIX_EPOCH) {
        Ok(after) => (
            i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
            i32::try_from(after.subsec_nanos()).unwrap_or(0),
        ),
        Err(_) => (0, 0),
    };
    prost_types::Timestamp { seconds, nanos }
}

/// A time at or after the Unix epoch, or `None` for an earlier or malformed one.
fn system_time(message: prost_types::Timestamp) -> Option<SystemTime> {
    let seconds = u64::try_from(message.seconds).ok()?;
    let nanos = u32::try_from(message.nanos).ok()?;
    (nanos < 1_000_000_000).then(|| UNIX_EPOCH + Duration::new(seconds, nanos))
}
