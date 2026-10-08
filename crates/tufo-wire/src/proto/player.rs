//! Protobuf mappings for the player.

use std::{path::PathBuf, time::Duration};

use tonic::Status;
use tufo_models::{
    library::{AudioFileId, CollectionEntryId, CollectionId, Volume},
    playback::{
        AudioFilePath, OutputTarget,
        browser::{BrowserPlayback, BrowserStatus, BrowserStopReason},
        diagnostics::{PlayerDiagnostics, VoicePacing},
        music_layers::{MusicLayerState, MusicLayers},
        player_state::{
            ActiveSound, CrossfadeDuration, MusicState, PlayerState, PositionSample,
            VoiceConnectionState,
        },
        queue::{QueueEntry, QueueModes, RepeatMode},
    },
};

use super::{
    InvalidMessage, duration_message, duration_value, invalid, parse_uuid, required,
    song_loop::{loop_region, song_loop_message},
    system_time, timestamp_message, voice_channel_target,
};
use crate::{
    pb,
    player::{UpdateMusicLayer, UpdatePlayerSettings},
};

// Requests the server parses.

pub fn add_music_layer_request(
    request: &pb::AddMusicLayerRequest,
) -> Result<CollectionEntryId, Status> {
    entry_id(&request.collection_entry_id, "collection_entry_id")
}

pub fn add_to_queue_request(request: &pb::AddToQueueRequest) -> Result<CollectionEntryId, Status> {
    entry_id(&request.collection_entry_id, "collection_entry_id")
}

pub fn remove_from_queue_request(
    request: &pb::RemoveFromQueueRequest,
) -> Result<CollectionEntryId, Status> {
    entry_id(&request.collection_entry_id, "collection_entry_id")
}

pub fn stop_music_layer_request(
    request: &pb::StopMusicLayerRequest,
) -> Result<CollectionEntryId, Status> {
    entry_id(&request.collection_entry_id, "collection_entry_id")
}

pub fn update_music_layer_request(
    request: &pb::UpdateMusicLayerRequest,
) -> Result<(CollectionEntryId, UpdateMusicLayer), Status> {
    Ok((
        entry_id(&request.collection_entry_id, "collection_entry_id")?,
        UpdateMusicLayer {
            volume: request
                .volume
                .map(|volume| {
                    Volume::try_new(volume)
                        .map_err(|_| invalid("volume", "must be between 0 and 1"))
                })
                .transpose()?,
            looping: request.looping,
        },
    ))
}

pub fn play_collection_request(
    request: &pb::PlayCollectionRequest,
) -> Result<(CollectionId, Option<CollectionEntryId>), Status> {
    let collection_id = parse_uuid(&request.collection_id)
        .map(CollectionId::from_uuid)
        .ok_or_else(|| invalid("collection_id", "must be a canonical UUID"))?;
    let start = request
        .start_collection_entry_id
        .as_deref()
        .map(|id| entry_id(id, "start_collection_entry_id"))
        .transpose()?;
    Ok((collection_id, start))
}

pub fn seek_music_request(request: pb::SeekMusicRequest) -> Result<Duration, Status> {
    required("position", request.position).and_then(|position| {
        duration_value(position).ok_or_else(|| invalid("position", "must not be negative"))
    })
}

pub fn update_player_settings_request(
    request: pb::UpdatePlayerSettingsRequest,
) -> Result<UpdatePlayerSettings, Status> {
    let volume = |field, volume: Option<f32>| {
        volume
            .map(|volume| {
                Volume::try_new(volume).map_err(|_| invalid(field, "must be between 0 and 1"))
            })
            .transpose()
    };
    Ok(UpdatePlayerSettings {
        effects_volume: volume("effects_volume", request.effects_volume)?,
        volume: volume("volume", request.volume)?,
        browser_volume: volume("browser_volume", request.browser_volume)?,
        browser_muted: request.browser_muted,
        muted: request.muted,
        shuffle: request.shuffle,
        repeat: request
            .repeat
            .map(|repeat| repeat_mode(repeat).map_err(|reason| invalid("repeat", reason)))
            .transpose()?,
        crossfade: request
            .crossfade
            .map(|crossfade| {
                duration_value(crossfade)
                    .and_then(|duration| CrossfadeDuration::try_new(duration).ok())
                    .ok_or_else(|| invalid("crossfade", "must be between 0 and 10 s"))
            })
            .transpose()?,
    })
}

pub fn play_sound_request(request: &pb::PlaySoundRequest) -> Result<CollectionEntryId, Status> {
    entry_id(&request.collection_entry_id, "collection_entry_id")
}

pub fn stop_sound_request(request: &pb::StopSoundRequest) -> Result<CollectionEntryId, Status> {
    entry_id(&request.collection_entry_id, "collection_entry_id")
}

pub fn select_output_request(request: pb::SelectOutputRequest) -> Result<OutputTarget, Status> {
    output_target(required("output", request.output)?).map_err(|reason| invalid("output", reason))
}

// Responses the server builds.

#[must_use]
pub fn watch_player_response(state: &PlayerState) -> pb::WatchPlayerResponse {
    pb::WatchPlayerResponse {
        state: Some(pb::PlayerState {
            output: Some(output_message(state.output)),
            voice: Some(voice_message(&state.voice)),
            music: state.music.as_ref().map(music_message),
            queued_collection_entry_ids: state
                .queued_entry_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
            music_layers: state
                .music_layers
                .iter()
                .map(|(_, layer)| pb::MusicLayerState {
                    music: Some(music_message(&layer.music)),
                    volume: layer.volume.into_inner(),
                    looping: layer.looping,
                })
                .collect(),
            volume: state.volume.into_inner(),
            effects_volume: state.effects_volume.into_inner(),
            muted: state.muted,
            shuffle: state.modes.shuffle,
            repeat: repeat_mode_message(state.modes.repeat).into(),
            crossfade: Some(duration_message(state.crossfade.into_inner())),
            sounds: state.sounds.iter().map(sound_message).collect(),
            sounds_paused: state.sounds_paused,
            browser: Some(browser_playback_message(state.browser)),
        }),
    }
}

#[must_use]
pub fn diagnostics_response(diagnostics: &PlayerDiagnostics) -> pb::GetPlayerDiagnosticsResponse {
    pb::GetPlayerDiagnosticsResponse {
        diagnostics: Some(pb::PlayerDiagnostics {
            music_underrun_frames: diagnostics.music_underrun_frames,
            sound_underrun_frames: diagnostics.sound_underrun_frames,
            local_stream_errors: diagnostics.local_stream_errors,
            local_stream_errors_discarded: diagnostics.local_stream_errors_discarded,
            browser_underrun_frames: diagnostics.browser_underrun_frames,
            browser_dropped_frames: diagnostics.browser_dropped_frames,
            voice_pacing: diagnostics.voice_pacing.map(|pacing| pb::VoicePacing {
                ticks: pacing.ticks,
                late_ticks: pacing.late_ticks,
                very_late_ticks: pacing.very_late_ticks,
                lateness_p50: Some(duration_message(pacing.lateness_p50)),
                lateness_p99: Some(duration_message(pacing.lateness_p99)),
                lateness_p999: Some(duration_message(pacing.lateness_p999)),
                lateness_max: Some(duration_message(pacing.lateness_max)),
            }),
        }),
    }
}

// Requests the client builds.

#[must_use]
pub fn play_collection_message(
    collection_id: CollectionId,
    start: Option<CollectionEntryId>,
) -> pb::PlayCollectionRequest {
    pb::PlayCollectionRequest {
        collection_id: collection_id.to_string(),
        start_collection_entry_id: start.map(|id| id.to_string()),
    }
}

#[must_use]
pub fn seek_music_message(position: Duration) -> pb::SeekMusicRequest {
    pb::SeekMusicRequest {
        position: Some(duration_message(position)),
    }
}

#[must_use]
pub fn update_player_settings_message(
    update: &UpdatePlayerSettings,
) -> pb::UpdatePlayerSettingsRequest {
    pb::UpdatePlayerSettingsRequest {
        volume: update.volume.map(Volume::into_inner),
        effects_volume: update.effects_volume.map(Volume::into_inner),
        browser_volume: update.browser_volume.map(Volume::into_inner),
        browser_muted: update.browser_muted,
        muted: update.muted,
        shuffle: update.shuffle,
        repeat: update
            .repeat
            .map(|repeat| repeat_mode_message(repeat).into()),
        crossfade: update
            .crossfade
            .map(|crossfade| duration_message(crossfade.into_inner())),
    }
}

#[must_use]
pub fn play_sound_message(id: CollectionEntryId) -> pb::PlaySoundRequest {
    pb::PlaySoundRequest {
        collection_entry_id: id.to_string(),
    }
}

#[must_use]
pub fn stop_sound_message(id: CollectionEntryId) -> pb::StopSoundRequest {
    pb::StopSoundRequest {
        collection_entry_id: id.to_string(),
    }
}

#[must_use]
pub fn select_output_message(target: OutputTarget) -> pb::SelectOutputRequest {
    pb::SelectOutputRequest {
        output: Some(output_message(target)),
    }
}

// Responses the client decodes.

fn music_layers_state(
    messages: Vec<pb::MusicLayerState>,
) -> Result<MusicLayers<MusicLayerState>, InvalidMessage> {
    let mut layers = MusicLayers::default();
    for message in messages {
        let layer = MusicLayerState {
            music: music_state(message.music.ok_or(InvalidMessage {
                field: "music_layers.music",
            })?)?,
            volume: Volume::try_new(message.volume).map_err(|_| InvalidMessage {
                field: "music_layers.volume",
            })?,
            looping: message.looping,
        };
        layers.try_insert(layer).map_err(|_| InvalidMessage {
            field: "music_layers",
        })?;
    }
    Ok(layers)
}

pub fn player_state(response: pb::WatchPlayerResponse) -> Result<PlayerState, InvalidMessage> {
    let state = response.state.ok_or(InvalidMessage { field: "state" })?;
    let invalid = |field| InvalidMessage { field };
    Ok(PlayerState {
        output: state
            .output
            .ok_or("is required")
            .and_then(output_target)
            .map_err(|_| invalid("output"))?,
        voice: voice_state(state.voice.ok_or(invalid("voice"))?)?,
        music: state.music.map(music_state).transpose()?,
        queued_entry_ids: state
            .queued_collection_entry_ids
            .iter()
            .map(|id| {
                parse_uuid(id)
                    .map(CollectionEntryId::from_uuid)
                    .ok_or(invalid("queued_collection_entry_ids"))
            })
            .collect::<Result<_, _>>()?,
        music_layers: music_layers_state(state.music_layers)?,
        volume: Volume::try_new(state.volume).map_err(|_| invalid("volume"))?,
        effects_volume: Volume::try_new(state.effects_volume)
            .map_err(|_| invalid("effects_volume"))?,
        muted: state.muted,
        modes: QueueModes {
            shuffle: state.shuffle,
            repeat: repeat_mode(state.repeat).map_err(|_| invalid("repeat"))?,
        },
        crossfade: state
            .crossfade
            .and_then(duration_value)
            .and_then(|duration| CrossfadeDuration::try_new(duration).ok())
            .ok_or(invalid("crossfade"))?,
        sounds: state
            .sounds
            .iter()
            .map(active_sound)
            .collect::<Result<_, _>>()?,
        sounds_paused: state.sounds_paused,
        browser: browser_playback(state.browser.ok_or(invalid("browser"))?)?,
    })
}

pub fn player_diagnostics(
    response: pb::GetPlayerDiagnosticsResponse,
) -> Result<PlayerDiagnostics, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    let diagnostics = response.diagnostics.ok_or(invalid("diagnostics"))?;
    let lateness = |value: Option<prost_types::Duration>| {
        value
            .and_then(duration_value)
            .ok_or(invalid("voice_pacing"))
    };
    let voice_pacing = diagnostics
        .voice_pacing
        .map(|pacing| {
            Ok::<_, InvalidMessage>(VoicePacing {
                ticks: pacing.ticks,
                late_ticks: pacing.late_ticks,
                very_late_ticks: pacing.very_late_ticks,
                lateness_p50: lateness(pacing.lateness_p50)?,
                lateness_p99: lateness(pacing.lateness_p99)?,
                lateness_p999: lateness(pacing.lateness_p999)?,
                lateness_max: lateness(pacing.lateness_max)?,
            })
        })
        .transpose()?;
    Ok(PlayerDiagnostics {
        music_underrun_frames: diagnostics.music_underrun_frames,
        sound_underrun_frames: diagnostics.sound_underrun_frames,
        local_stream_errors: diagnostics.local_stream_errors,
        local_stream_errors_discarded: diagnostics.local_stream_errors_discarded,
        browser_underrun_frames: diagnostics.browser_underrun_frames,
        browser_dropped_frames: diagnostics.browser_dropped_frames,
        voice_pacing,
    })
}

// Shared conversions.

fn entry_id(text: &str, field: &str) -> Result<CollectionEntryId, Status> {
    parse_uuid(text)
        .map(CollectionEntryId::from_uuid)
        .ok_or_else(|| invalid(field, "must be a canonical UUID"))
}

fn music_message(music: &MusicState) -> pb::MusicState {
    pb::MusicState {
        collection_id: music.collection_id.to_string(),
        collection_entry_id: music.entry.entry_id.to_string(),
        title: music.entry.title.clone(),
        position: Some(duration_message(music.position.position)),
        position_sampled_at: Some(timestamp_message(music.position.sampled_at)),
        duration: music.duration.map(duration_message),
        paused: music.paused,
        path: music.entry.path.as_ref().to_string_lossy().into_owned(),
        audio_file_id: music.entry.audio_file_id.to_string(),
        r#loop: music.loop_region.map(song_loop_message),
    }
}

fn music_state(message: pb::MusicState) -> Result<MusicState, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    let duration = message
        .duration
        .map(|duration| duration_value(duration).ok_or(invalid("duration")))
        .transpose()?;
    Ok(MusicState {
        collection_id: parse_uuid(&message.collection_id)
            .map(CollectionId::from_uuid)
            .ok_or(invalid("collection_id"))?,
        entry: QueueEntry {
            entry_id: parse_uuid(&message.collection_entry_id)
                .map(CollectionEntryId::from_uuid)
                .ok_or(invalid("collection_entry_id"))?,
            audio_file_id: parse_uuid(&message.audio_file_id)
                .map(AudioFileId::from_uuid)
                .ok_or(invalid("audio_file_id"))?,
            path: AudioFilePath::try_new(PathBuf::from(message.path))
                .map_err(|_| invalid("path"))?,
            title: message.title,
            duration,
        },
        position: PositionSample {
            position: message
                .position
                .and_then(duration_value)
                .ok_or(invalid("position"))?,
            sampled_at: message
                .position_sampled_at
                .and_then(system_time)
                .ok_or(invalid("position_sampled_at"))?,
        },
        duration,
        paused: message.paused,
        loop_region: message
            .r#loop
            .map(|region| loop_region(region).map_err(|_| invalid("loop")))
            .transpose()?,
    })
}

fn sound_message(sound: &ActiveSound) -> pb::ActiveSound {
    pb::ActiveSound {
        collection_entry_id: sound.entry_id.to_string(),
        started_at: Some(timestamp_message(sound.started_at)),
        duration: Some(duration_message(sound.duration)),
        looping: sound.looping,
    }
}

fn active_sound(message: &pb::ActiveSound) -> Result<ActiveSound, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    Ok(ActiveSound {
        entry_id: parse_uuid(&message.collection_entry_id)
            .map(CollectionEntryId::from_uuid)
            .ok_or(invalid("collection_entry_id"))?,
        started_at: message
            .started_at
            .and_then(system_time)
            .ok_or(invalid("started_at"))?,
        duration: message
            .duration
            .and_then(duration_value)
            .ok_or(invalid("duration"))?,
        looping: message.looping,
    })
}

fn voice_message(voice: &VoiceConnectionState) -> pb::VoiceConnection {
    let state = match voice {
        VoiceConnectionState::NotConfigured => {
            pb::voice_connection::State::NotConfigured(pb::VoiceNotConfigured {})
        }
        VoiceConnectionState::Disconnected { reason } => {
            pb::voice_connection::State::Disconnected(pb::VoiceDisconnected {
                reason: reason.clone(),
            })
        }
        VoiceConnectionState::Connecting => {
            pb::voice_connection::State::Connecting(pb::VoiceConnecting {})
        }
        VoiceConnectionState::Connected => {
            pb::voice_connection::State::Connected(pb::VoiceConnected {})
        }
        VoiceConnectionState::Reconnecting { attempt } => {
            pb::voice_connection::State::Reconnecting(pb::VoiceReconnecting { attempt: *attempt })
        }
        VoiceConnectionState::Disabled => {
            pb::voice_connection::State::Disabled(pb::VoiceDisabled {})
        }
    };
    pb::VoiceConnection { state: Some(state) }
}

fn voice_state(message: pb::VoiceConnection) -> Result<VoiceConnectionState, InvalidMessage> {
    Ok(
        match message.state.ok_or(InvalidMessage { field: "voice" })? {
            pb::voice_connection::State::NotConfigured(_) => VoiceConnectionState::NotConfigured,
            pb::voice_connection::State::Disconnected(disconnected) => {
                VoiceConnectionState::Disconnected {
                    reason: disconnected.reason,
                }
            }
            pb::voice_connection::State::Connecting(_) => VoiceConnectionState::Connecting,
            pb::voice_connection::State::Connected(_) => VoiceConnectionState::Connected,
            pb::voice_connection::State::Reconnecting(reconnecting) => {
                VoiceConnectionState::Reconnecting {
                    attempt: reconnecting.attempt,
                }
            }
            pb::voice_connection::State::Disabled(_) => VoiceConnectionState::Disabled,
        },
    )
}

fn browser_playback_message(browser: BrowserPlayback) -> pb::BrowserPlayback {
    let status = match browser.status {
        BrowserStatus::Closed => pb::BrowserStatus::Closed,
        BrowserStatus::Starting => pb::BrowserStatus::Starting,
        BrowserStatus::Open => pb::BrowserStatus::Open,
        BrowserStatus::Stopped(BrowserStopReason::Idle) => pb::BrowserStatus::StoppedIdle,
        BrowserStatus::Stopped(BrowserStopReason::Crashed) => pb::BrowserStatus::StoppedCrashed,
        BrowserStatus::Stopped(BrowserStopReason::StartFailed) => {
            pb::BrowserStatus::StoppedStartFailed
        }
        BrowserStatus::Stopped(BrowserStopReason::Unavailable) => {
            pb::BrowserStatus::StoppedUnavailable
        }
    };
    pb::BrowserPlayback {
        status: status.into(),
        volume: browser.volume.into_inner(),
        muted: browser.muted,
        audible: browser.audible,
    }
}

fn browser_playback(message: pb::BrowserPlayback) -> Result<BrowserPlayback, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    let status = match pb::BrowserStatus::try_from(message.status) {
        Ok(pb::BrowserStatus::Closed) => BrowserStatus::Closed,
        Ok(pb::BrowserStatus::Starting) => BrowserStatus::Starting,
        Ok(pb::BrowserStatus::Open) => BrowserStatus::Open,
        Ok(pb::BrowserStatus::StoppedIdle) => BrowserStatus::Stopped(BrowserStopReason::Idle),
        Ok(pb::BrowserStatus::StoppedCrashed) => BrowserStatus::Stopped(BrowserStopReason::Crashed),
        Ok(pb::BrowserStatus::StoppedStartFailed) => {
            BrowserStatus::Stopped(BrowserStopReason::StartFailed)
        }
        Ok(pb::BrowserStatus::StoppedUnavailable) => {
            BrowserStatus::Stopped(BrowserStopReason::Unavailable)
        }
        Ok(pb::BrowserStatus::Unspecified) | Err(_) => return Err(invalid("browser.status")),
    };
    Ok(BrowserPlayback {
        status,
        volume: Volume::try_new(message.volume).map_err(|_| invalid("browser.volume"))?,
        muted: message.muted,
        audible: message.audible,
    })
}

fn repeat_mode(repeat: i32) -> Result<RepeatMode, &'static str> {
    match pb::RepeatMode::try_from(repeat) {
        Ok(pb::RepeatMode::Off) => Ok(RepeatMode::Off),
        Ok(pb::RepeatMode::Playlist) => Ok(RepeatMode::Playlist),
        Ok(pb::RepeatMode::Track) => Ok(RepeatMode::Track),
        Ok(pb::RepeatMode::Unspecified) | Err(_) => Err("must be off, playlist, or track"),
    }
}

fn repeat_mode_message(repeat: RepeatMode) -> pb::RepeatMode {
    match repeat {
        RepeatMode::Off => pb::RepeatMode::Off,
        RepeatMode::Playlist => pb::RepeatMode::Playlist,
        RepeatMode::Track => pb::RepeatMode::Track,
    }
}

fn output_message(target: OutputTarget) -> pb::Output {
    let target = match target {
        OutputTarget::Local => pb::output::Target::Local(pb::LocalOutput {}),
        OutputTarget::DiscordVoice(voice) => {
            pb::output::Target::DiscordVoice(pb::DiscordVoiceOutput {
                guild_id: voice.guild_id.into_inner().get(),
                channel_id: voice.channel_id.into_inner().get(),
            })
        }
    };
    pb::Output {
        target: Some(target),
    }
}

fn output_target(output: pb::Output) -> Result<OutputTarget, &'static str> {
    match output.target.ok_or("target is required")? {
        pb::output::Target::Local(_) => Ok(OutputTarget::Local),
        pb::output::Target::DiscordVoice(voice) => {
            voice_channel_target(voice.guild_id, voice.channel_id).map(OutputTarget::DiscordVoice)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, time::SystemTime};

    use tonic::Code;
    use tufo_models::{
        discord::{GuildId, VoiceChannelId, VoiceChannelTarget},
        library::song_loop::{LoopMarkers, LoopRegion, SourceFrames, SourceTiming},
    };

    use super::*;

    #[test]
    fn diagnostics_preserve_decoder_and_local_stream_failures() {
        let diagnostics = PlayerDiagnostics {
            music_underrun_frames: 128,
            sound_underrun_frames: 256,
            local_stream_errors: 3,
            local_stream_errors_discarded: 4,
            browser_underrun_frames: 5,
            browser_dropped_frames: 6,
            voice_pacing: None,
        };

        assert_eq!(
            player_diagnostics(diagnostics_response(&diagnostics)).unwrap(),
            diagnostics
        );
    }

    #[test]
    fn player_states_survive_the_round_trip() {
        let id = uuid::Uuid::from_u128(5);
        let mut state = PlayerState {
            output: OutputTarget::DiscordVoice(VoiceChannelTarget {
                guild_id: GuildId::new(NonZeroU64::new(959_535_519_157_612_664).unwrap()),
                channel_id: VoiceChannelId::new(NonZeroU64::new(42).unwrap()),
            }),
            voice: VoiceConnectionState::Reconnecting { attempt: 2 },
            music: Some(MusicState {
                collection_id: CollectionId::from_uuid(id),
                entry: QueueEntry {
                    entry_id: CollectionEntryId::from_uuid(id),
                    audio_file_id: AudioFileId::from_uuid(id),
                    path: AudioFilePath::try_new(std::env::temp_dir().join("tavern.mp3")).unwrap(),
                    title: "Tavern".to_owned(),
                    duration: Some(Duration::from_secs(95)),
                },
                position: PositionSample {
                    position: Duration::from_millis(12_500),
                    sampled_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000),
                },
                duration: Some(Duration::from_secs(95)),
                paused: true,
                loop_region: Some(
                    LoopRegion::try_new(
                        SourceTiming {
                            sample_rate: std::num::NonZeroU32::new(44_100).unwrap(),
                            frame_count: SourceFrames(4_189_500),
                        },
                        LoopMarkers {
                            playback_start: SourceFrames::ZERO,
                            loop_start: SourceFrames(441_000),
                            loop_end: SourceFrames(4_000_000),
                            crossfade: SourceFrames::ZERO,
                        },
                    )
                    .unwrap(),
                ),
            }),
            volume: Volume::try_new(0.75).unwrap(),
            effects_volume: Volume::try_new(0.5).unwrap(),
            muted: true,
            modes: QueueModes {
                shuffle: true,
                repeat: RepeatMode::Track,
            },
            crossfade: CrossfadeDuration::try_new(Duration::ZERO).unwrap(),
            sounds: vec![ActiveSound {
                entry_id: CollectionEntryId::from_uuid(id),
                started_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_001),
                duration: Duration::from_secs(3),
                looping: true,
            }],
            sounds_paused: true,
            queued_entry_ids: vec![
                CollectionEntryId::from_uuid(uuid::Uuid::from_u128(6)),
                CollectionEntryId::from_uuid(id),
            ],
            music_layers: MusicLayers::default(),
            browser: BrowserPlayback {
                status: BrowserStatus::Stopped(BrowserStopReason::Crashed),
                volume: Volume::try_new(0.3).unwrap(),
                muted: true,
                audible: true,
            },
        };

        state
            .music_layers
            .try_insert(MusicLayerState {
                music: state.music.clone().unwrap(),
                volume: Volume::try_new(0.4).unwrap(),
                looping: true,
            })
            .unwrap();

        assert_eq!(player_state(watch_player_response(&state)).unwrap(), state);
        let mut response = watch_player_response(&state);
        let wire_state = response.state.as_mut().unwrap();
        wire_state.music_layers = vec![wire_state.music_layers[0].clone(); 3];
        assert_eq!(player_state(response).unwrap_err().field, "music_layers");
    }

    #[test]
    fn out_of_range_settings_are_invalid_arguments() {
        let loud = update_player_settings_request(pb::UpdatePlayerSettingsRequest {
            volume: Some(2.0),
            ..pb::UpdatePlayerSettingsRequest::default()
        });
        let loud_browser = update_player_settings_request(pb::UpdatePlayerSettingsRequest {
            browser_volume: Some(1.5),
            ..pb::UpdatePlayerSettingsRequest::default()
        });
        let long_crossfade = update_player_settings_request(pb::UpdatePlayerSettingsRequest {
            crossfade: Some(duration_message(Duration::from_secs(11))),
            ..pb::UpdatePlayerSettingsRequest::default()
        });
        let untouched =
            update_player_settings_request(pb::UpdatePlayerSettingsRequest::default()).unwrap();

        assert_eq!(loud.unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(loud_browser.unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(long_crossfade.unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(untouched, UpdatePlayerSettings::default());
    }

    #[test]
    fn layer_updates_preserve_false_and_zero_and_reject_invalid_gain() {
        let update = update_music_layer_request(&pb::UpdateMusicLayerRequest {
            collection_entry_id: uuid::Uuid::from_u128(1).to_string(),
            volume: Some(0.0),
            looping: Some(false),
        })
        .unwrap()
        .1;
        assert_eq!(
            update.volume.unwrap().into_inner().to_bits(),
            0.0_f32.to_bits()
        );
        assert_eq!(update.looping, Some(false));
        for volume in [f32::NAN, -0.1, 1.1] {
            assert_eq!(
                update_music_layer_request(&pb::UpdateMusicLayerRequest {
                    collection_entry_id: uuid::Uuid::from_u128(1).to_string(),
                    volume: Some(volume),
                    looping: None,
                })
                .unwrap_err()
                .code(),
                Code::InvalidArgument
            );
        }
    }

    #[test]
    fn select_output_rejects_missing_and_zero_targets() {
        let missing = select_output_request(pb::SelectOutputRequest { output: None });
        let zero_guild = select_output_request(pb::SelectOutputRequest {
            output: Some(pb::Output {
                target: Some(pb::output::Target::DiscordVoice(pb::DiscordVoiceOutput {
                    guild_id: 0,
                    channel_id: 1,
                })),
            }),
        });

        for result in [missing, zero_guild] {
            assert_eq!(result.unwrap_err().code(), Code::InvalidArgument);
        }
    }
}
