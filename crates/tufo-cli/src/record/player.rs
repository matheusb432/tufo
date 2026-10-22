use serde_json::{Map, Value, json};
use tufo_models::{
    discord::VoiceChannel,
    library::CollectionEntryId,
    playback::{
        OutputTarget,
        browser::{BrowserPlayback, BrowserStatus, BrowserStopReason},
        diagnostics::{
            AudioMemoryDiagnostics, LocalCallbackDiagnostics, PlayerDiagnostics, VoicePacing,
        },
        local_output::{
            LocalOutputFailure, LocalOutputFailureReason, LocalOutputIdentity, LocalOutputState,
        },
        music_layers::{MusicLayerState, MusicLayers},
        player_state::{ActiveSound, MusicState, PlayerState, VoiceConnectionState},
        queue::{QueueEntry, RepeatMode},
    },
};

use super::{duration_ms, loop_region, timestamp_unix_ms};

pub(crate) fn player(value: &PlayerState) -> Value {
    let mut fields = music_fields(value);
    fields.extend([
        ("output".to_owned(), output(value.output)),
        ("local_output".to_owned(), local_output(&value.local_output)),
        ("voice".to_owned(), voice(&value.voice)),
        (
            "queued_entry_ids".to_owned(),
            queued_entry_ids(&value.queued_entry_ids),
        ),
        ("sounds".to_owned(), sounds(&value.sounds)),
        ("sounds_paused".to_owned(), json!(value.sounds_paused)),
        ("browser".to_owned(), browser(value.browser)),
    ]);
    fields.into()
}

pub(crate) fn local_output(value: &LocalOutputState) -> Value {
    match value {
        LocalOutputState::Released => json!({"status": "released"}),
        LocalOutputState::Ready(identity) => {
            json!({"status": "ready", "identity": local_identity(identity)})
        }
        LocalOutputState::Recovering { previous_failure } => {
            json!({"status": "recovering", "previous_failure": previous_failure.as_ref().map(local_failure)})
        }
        LocalOutputState::Failed(failure) => {
            json!({"status": "failed", "failure": local_failure(failure)})
        }
    }
}

fn local_identity(value: &LocalOutputIdentity) -> Value {
    json!({"backend": value.backend, "device": value.device, "sample_rate": value.sample_rate})
}

fn local_failure(value: &LocalOutputFailure) -> Value {
    let reason = match value.reason {
        LocalOutputFailureReason::NoDevice => "no_device",
        LocalOutputFailureReason::TargetMissing => "target_missing",
        LocalOutputFailureReason::DeviceUnavailable => "device_unavailable",
        LocalOutputFailureReason::StreamInvalidated => "stream_invalidated",
        LocalOutputFailureReason::DeviceChanged => "device_changed",
        LocalOutputFailureReason::CallbackStopped => "callback_stopped",
        LocalOutputFailureReason::OpenFailed => "open_failed",
        LocalOutputFailureReason::OpenTimedOut => "open_timed_out",
    };
    json!({"reason": reason, "identity": value.identity.as_ref().map(local_identity), "detail": value.detail})
}

pub(crate) fn browser(value: BrowserPlayback) -> Value {
    let (status, stop_reason) = match value.status {
        BrowserStatus::Closed => ("closed", None),
        BrowserStatus::Starting => ("starting", None),
        BrowserStatus::Open => ("open", None),
        BrowserStatus::Stopped(reason) => (
            "stopped",
            Some(match reason {
                BrowserStopReason::Idle => "idle",
                BrowserStopReason::Crashed => "crashed",
                BrowserStopReason::StartFailed => "start_failed",
                BrowserStopReason::Unavailable => "unavailable",
            }),
        ),
    };
    json!({
        "status": status,
        "stop_reason": stop_reason,
        "volume": value.volume.into_inner(),
        "muted": value.muted,
        "audible": value.audible,
    })
}

pub(crate) fn music_state(value: &PlayerState) -> Value {
    music_fields(value).into()
}

fn music_fields(value: &PlayerState) -> Map<String, Value> {
    let mut fields = settings_fields(value);
    fields.extend([
        (
            "music".to_owned(),
            value.music.as_ref().map_or(Value::Null, music),
        ),
        ("music_layers".to_owned(), music_layers(&value.music_layers)),
    ]);
    fields
}

pub(crate) fn player_settings(value: &PlayerState) -> Value {
    settings_fields(value).into()
}

fn settings_fields(value: &PlayerState) -> Map<String, Value> {
    [
        ("volume", json!(value.volume.into_inner())),
        ("effects_volume", json!(value.effects_volume.into_inner())),
        ("muted", json!(value.muted)),
        ("shuffle", json!(value.modes.shuffle)),
        ("repeat", json!(repeat_mode(value.modes.repeat))),
        (
            "crossfade_ms",
            json!(duration_ms(value.crossfade.into_inner())),
        ),
        ("browser_volume", json!(value.browser.volume.into_inner())),
        ("browser_muted", json!(value.browser.muted)),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect()
}

pub(crate) fn queued_entry_ids(ids: &[CollectionEntryId]) -> Value {
    json!(ids.iter().map(ToString::to_string).collect::<Vec<_>>())
}

pub(crate) fn music_layers(layers: &MusicLayers<MusicLayerState>) -> Value {
    layers
        .iter()
        .map(|(slot, layer)| {
            json!({
                "slot": slot,
                "music": music(&layer.music),
                "volume": layer.volume.into_inner(),
                "looping": layer.looping,
            })
        })
        .collect()
}

pub(crate) fn sounds(sounds: &[ActiveSound]) -> Value {
    sounds.iter().map(sound).collect()
}

pub(crate) fn output(value: OutputTarget) -> Value {
    match value {
        OutputTarget::Local => json!({"kind": "local"}),
        OutputTarget::DiscordVoice(target) => json!({
            "kind": "discord_voice",
            "guild_id": target.guild_id.into_inner().get().to_string(),
            "channel_id": target.channel_id.into_inner().get().to_string(),
        }),
    }
}

pub(crate) fn voice_channel(value: &VoiceChannel) -> Value {
    json!({
        "guild_id": value.target.guild_id.into_inner().get().to_string(),
        "channel_id": value.target.channel_id.into_inner().get().to_string(),
        "guild_name": value.guild_name,
        "channel_name": value.channel_name,
        "position": value.position,
    })
}

pub(crate) fn diagnostics(value: &PlayerDiagnostics) -> Value {
    json!({
        "music_underrun_frames": value.music_underrun_frames,
        "sound_underrun_frames": value.sound_underrun_frames,
        "local_stream_errors": value.local_stream_errors,
        "local_stream_errors_discarded": value.local_stream_errors_discarded,
        "browser_underrun_frames": value.browser_underrun_frames,
        "browser_dropped_frames": value.browser_dropped_frames,
        "voice_pacing": value.voice_pacing.as_ref().map(voice_pacing),
        "local_callbacks": value.local_callbacks.as_ref().map(local_callbacks),
        "audio_memory": value.audio_memory.as_ref().map(audio_memory),
    })
}

fn local_callbacks(value: &LocalCallbackDiagnostics) -> Value {
    let millis = |duration: std::time::Duration| duration.as_secs_f64() * 1_000.0;
    json!({
        "session_started_at_unix_ms": value.session_started_at_unix_ms,
        "sampled_at_ms": millis(value.sampled_at),
        "streams_created": value.streams_created,
        "latest_stream_id": value.latest_stream_id,
        "callbacks": value.callbacks,
        "sample_rate": value.sample_rate,
        "callback_frames_last": value.callback_frames_last,
        "callback_frames_min": value.callback_frames_min,
        "callback_frames_max": value.callback_frames_max,
        "callback_gap_p99_ms": millis(value.callback_gap_p99),
        "callback_gap_max_ms": millis(value.callback_gap_max),
        "render_duration_p99_ms": millis(value.render_duration_p99),
        "render_duration_max_ms": millis(value.render_duration_max),
        "late_callbacks": value.late_callbacks,
        "render_over_budget": value.render_over_budget,
        "first_anomaly_at_ms": value.first_anomaly_at.map(millis),
        "latest_anomaly_at_ms": value.latest_anomaly_at.map(millis),
        "playback_lead_ms": value.playback_lead.map(millis),
    })
}

fn audio_memory(value: &AudioMemoryDiagnostics) -> Value {
    json!({
        "bytes_used": value.bytes_used,
        "bytes_max": value.bytes_max,
        "locked_bytes": value.locked_bytes,
        "lock_failures": value.lock_failures,
        "lock_failed_bytes": value.lock_failed_bytes,
        "locking_supported": value.locking_supported,
    })
}

pub(crate) fn voice(value: &VoiceConnectionState) -> Value {
    match value {
        VoiceConnectionState::NotConfigured => json!({"state": "not_configured"}),
        VoiceConnectionState::Disabled => json!({"state": "disabled"}),
        VoiceConnectionState::Disconnected { reason } => {
            json!({"state": "disconnected", "reason": reason})
        }
        VoiceConnectionState::Connecting => json!({"state": "connecting"}),
        VoiceConnectionState::Connected => json!({"state": "connected"}),
        VoiceConnectionState::Reconnecting { attempt } => {
            json!({"state": "reconnecting", "attempt": attempt})
        }
    }
}

fn music(value: &MusicState) -> Value {
    json!({
        "collection_id": value.collection_id.to_string(),
        "entry": queue_entry(&value.entry),
        "position": {
            "position_ms": duration_ms(value.position.position),
            "sampled_at_unix_ms": timestamp_unix_ms(value.position.sampled_at),
        },
        "duration_ms": value.duration.map(duration_ms),
        "paused": value.paused,
        "loop_region": value.loop_region.as_ref().map(loop_region),
    })
}

fn queue_entry(value: &QueueEntry) -> Value {
    json!({
        "entry_id": value.entry_id.to_string(),
        "audio_file_id": value.audio_file_id.to_string(),
        "path": value.path.as_ref().display().to_string(),
        "title": value.title,
        "duration_ms": value.duration.map(duration_ms),
    })
}

fn sound(value: &ActiveSound) -> Value {
    json!({
        "entry_id": value.entry_id.to_string(),
        "started_at_unix_ms": timestamp_unix_ms(value.started_at),
        "duration_ms": duration_ms(value.duration),
        "looping": value.looping,
    })
}

fn repeat_mode(value: RepeatMode) -> &'static str {
    match value {
        RepeatMode::Off => "off",
        RepeatMode::Playlist => "playlist",
        RepeatMode::Track => "track",
    }
}

fn voice_pacing(value: &VoicePacing) -> Value {
    json!({
        "ticks": value.ticks,
        "late_ticks": value.late_ticks,
        "very_late_ticks": value.very_late_ticks,
        "lateness_p50_ms": value.lateness_p50.as_secs_f64() * 1_000.0,
        "lateness_p99_ms": value.lateness_p99.as_secs_f64() * 1_000.0,
        "lateness_p999_ms": value.lateness_p999.as_secs_f64() * 1_000.0,
        "lateness_max_ms": value.lateness_max.as_secs_f64() * 1_000.0,
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use tufo_models::{
        library::{AudioFileId, CollectionEntryId, CollectionId, Volume},
        playback::{AudioFilePath, music_layers::MusicLayerState, player_state::PositionSample},
    };

    use super::*;

    #[test]
    fn callback_records_keep_precision_session_identity_and_optional_anomalies() {
        let record = diagnostics(&PlayerDiagnostics {
            local_callbacks: Some(LocalCallbackDiagnostics {
                session_started_at_unix_ms: 1234,
                render_duration_p99: Duration::from_micros(50),
                latest_anomaly_at: Some(Duration::from_micros(1500)),
                ..LocalCallbackDiagnostics::default()
            }),
            audio_memory: Some(AudioMemoryDiagnostics {
                locked_bytes: 4096,
                locking_supported: true,
                ..AudioMemoryDiagnostics::default()
            }),
            ..PlayerDiagnostics::default()
        });
        assert_eq!(
            record["local_callbacks"]["session_started_at_unix_ms"],
            1234
        );
        assert_eq!(record["local_callbacks"]["render_duration_p99_ms"], 0.05);
        assert_eq!(record["local_callbacks"]["latest_anomaly_at_ms"], 1.5);
        assert!(record["local_callbacks"]["first_anomaly_at_ms"].is_null());
        assert_eq!(record["audio_memory"]["locked_bytes"], 4096);
    }

    #[test]
    fn diagnostics_keep_submillisecond_voice_lateness() -> anyhow::Result<()> {
        let value = PlayerDiagnostics {
            voice_pacing: Some(VoicePacing {
                lateness_p50: Duration::from_micros(50),
                lateness_p99: Duration::from_micros(950),
                ..VoicePacing::default()
            }),
            ..PlayerDiagnostics::default()
        };

        let record = diagnostics(&value);
        let pacing = &record["voice_pacing"];
        let p50 = pacing["lateness_p50_ms"]
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("missing p50 milliseconds"))?;
        let p99 = pacing["lateness_p99_ms"]
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("missing p99 milliseconds"))?;

        assert!((p50 - 0.05).abs() < f64::EPSILON);
        assert!((p99 - 0.95).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn layer_records_keep_slot_identity_and_the_original_position_sample() -> anyhow::Result<()> {
        let music = MusicState {
            collection_id: CollectionId::from_uuid("00000000-0000-0000-0000-000000000001".parse()?),
            entry: QueueEntry {
                entry_id: CollectionEntryId::from_uuid(
                    "00000000-0000-0000-0000-000000000002".parse()?,
                ),
                audio_file_id: AudioFileId::from_uuid(
                    "00000000-0000-0000-0000-000000000003".parse()?,
                ),
                path: AudioFilePath::try_new(std::env::temp_dir().join("layer.wav"))?,
                title: "Layer".to_owned(),
                duration: None,
            },
            position: PositionSample {
                position: Duration::from_millis(456),
                sampled_at: SystemTime::UNIX_EPOCH + Duration::from_millis(123),
            },
            duration: None,
            paused: false,
            loop_region: None,
        };
        let layer = MusicLayerState {
            music,
            volume: Volume::default(),
            looping: false,
        };
        let mut state = PlayerState::default();
        assert_eq!(state.music_layers.try_insert(layer.clone()), Ok(0));
        assert_eq!(state.music_layers.try_insert(layer), Ok(1));
        assert!(state.music_layers.remove(0).is_some());

        let record = player(&state);
        let layers = record["music_layers"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing music layers"))?;

        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0]["slot"], 1);
        assert_eq!(layers[0]["music"]["position"]["position_ms"], 456);
        assert_eq!(layers[0]["music"]["position"]["sampled_at_unix_ms"], 123);
        assert!(layers[0]["music"]["duration_ms"].is_null());
        Ok(())
    }
}
