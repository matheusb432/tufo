use std::{num::NonZeroU32, str::FromStr, time::Duration};

use serde::{Deserialize, Deserializer};
use tufo_client::song_loops::{PreviewMode, PreviewSongLoop, UpdateSongLoop};
use tufo_models::library::{
    AudioFileId, AudioFingerprint,
    song_loop::{LoopMarkers, LoopRegion, SeamPreviewSide, SourceFrames, SourceTiming},
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct AudioFileArgument(pub(super) AudioFileId);

impl FromStr for AudioFileArgument {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse()
            .map(AudioFileId::from_uuid)
            .map(Self)
            .map_err(|_| "audio file ID must be a UUID".to_owned())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LoopUpdate {
    source_fingerprint: AudioFingerprint,
    playback_start: Option<SourceFrames>,
    loop_start: Option<SourceFrames>,
    loop_end: Option<SourceFrames>,
    crossfade: Option<SourceFrames>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoopUpdateJson {
    source_fingerprint: String,
    #[serde(default, deserialize_with = "optional_frames")]
    playback_start_frame: Option<SourceFrames>,
    #[serde(default, deserialize_with = "optional_frames")]
    loop_start_frame: Option<SourceFrames>,
    #[serde(default, deserialize_with = "optional_frames")]
    loop_end_frame: Option<SourceFrames>,
    #[serde(default, deserialize_with = "optional_frames")]
    crossfade_frames: Option<SourceFrames>,
}

fn optional_frames<'de, D>(deserializer: D) -> Result<Option<SourceFrames>, D::Error>
where
    D: Deserializer<'de>,
{
    u64::deserialize(deserializer).map(|frames| Some(SourceFrames(frames)))
}

impl FromStr for LoopUpdate {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let raw: LoopUpdateJson = crate::input::parse_json(text)?;
        let source_fingerprint = fingerprint(raw.source_fingerprint)?;
        if raw.loop_end_frame == Some(SourceFrames::ZERO) {
            return Err("loop_end_frame must be greater than zero".to_owned());
        }
        if let (Some(playback_start), Some(loop_start)) =
            (raw.playback_start_frame, raw.loop_start_frame)
            && playback_start > loop_start
        {
            return Err("playback_start_frame must not exceed loop_start_frame".to_owned());
        }
        if let (Some(loop_start), Some(loop_end)) = (raw.loop_start_frame, raw.loop_end_frame)
            && loop_start >= loop_end
        {
            return Err("loop_start_frame must precede loop_end_frame".to_owned());
        }
        if let (Some(playback_start), Some(loop_end)) =
            (raw.playback_start_frame, raw.loop_end_frame)
            && playback_start >= loop_end
        {
            return Err("playback_start_frame must precede loop_end_frame".to_owned());
        }
        if let (Some(crossfade), Some(loop_start)) = (raw.crossfade_frames, raw.loop_start_frame)
            && crossfade > loop_start
        {
            return Err("crossfade_frames exceeds the audio before loop_start_frame".to_owned());
        }
        if let (Some(crossfade), Some(loop_start), Some(loop_end)) = (
            raw.crossfade_frames,
            raw.loop_start_frame,
            raw.loop_end_frame,
        ) && crossfade > loop_end.saturating_sub(loop_start)
        {
            return Err("crossfade_frames exceeds the loop's length".to_owned());
        }
        Ok(Self {
            source_fingerprint,
            playback_start: raw.playback_start_frame,
            loop_start: raw.loop_start_frame,
            loop_end: raw.loop_end_frame,
            crossfade: raw.crossfade_frames,
        })
    }
}

impl LoopUpdate {
    pub(super) fn into_request(self, audio_file_id: AudioFileId) -> UpdateSongLoop {
        UpdateSongLoop {
            audio_file_id,
            source_fingerprint: self.source_fingerprint,
            playback_start: self.playback_start,
            loop_start: self.loop_start,
            loop_end: self.loop_end,
            crossfade: self.crossfade,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LoopPreview {
    source_fingerprint: AudioFingerprint,
    draft: LoopRegion,
    mode: PreviewMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoopPreviewJson {
    source_fingerprint: String,
    draft: LoopDraftJson,
    mode: PreviewModeJson,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoopDraftJson {
    timing: SourceTimingJson,
    playback_start_frame: u64,
    loop_start_frame: u64,
    loop_end_frame: u64,
    crossfade_frames: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceTimingJson {
    sample_rate: NonZeroU32,
    frame_count: u64,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PreviewModeJson {
    FromFrame { start_frame: u64 },
    Seam { side_ms: u64 },
}

impl FromStr for LoopPreview {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let raw: LoopPreviewJson = crate::input::parse_json(text)?;
        let source_fingerprint = fingerprint(raw.source_fingerprint)?;
        let timing = SourceTiming {
            sample_rate: raw.draft.timing.sample_rate,
            frame_count: SourceFrames(raw.draft.timing.frame_count),
        };
        let draft = LoopRegion::try_new(
            timing,
            LoopMarkers {
                playback_start: SourceFrames(raw.draft.playback_start_frame),
                loop_start: SourceFrames(raw.draft.loop_start_frame),
                loop_end: SourceFrames(raw.draft.loop_end_frame),
                crossfade: SourceFrames(raw.draft.crossfade_frames),
            },
        )
        .map_err(|error| format!("invalid draft: {error}"))?;
        let mode = match raw.mode {
            PreviewModeJson::FromFrame { start_frame } => {
                let start = SourceFrames(start_frame);
                if start > timing.frame_count {
                    return Err(
                        "start_frame must not exceed the source file's frame_count".to_owned()
                    );
                }
                PreviewMode::FromFrame(start)
            }
            PreviewModeJson::Seam { side_ms } => PreviewMode::Seam(
                SeamPreviewSide::try_new(Duration::from_millis(side_ms))
                    .map_err(|_| "side_ms must be 1..10000 milliseconds".to_owned())?,
            ),
        };
        Ok(Self {
            source_fingerprint,
            draft,
            mode,
        })
    }
}

impl LoopPreview {
    pub(super) fn into_request(self, audio_file_id: AudioFileId) -> PreviewSongLoop {
        PreviewSongLoop {
            audio_file_id,
            source_fingerprint: self.source_fingerprint,
            draft: self.draft,
            mode: self.mode,
        }
    }
}

fn fingerprint(text: String) -> Result<AudioFingerprint, String> {
    AudioFingerprint::try_new(text)
        .map_err(|_| "source_fingerprint must be 64 lowercase hexadecimal digits".to_owned())
}

#[cfg(test)]
mod tests {
    use anyhow::Context as _;
    use clap::{Parser as _, error::ErrorKind};
    use serde_json::{Value, json};

    use super::*;

    const AUDIO_FILE_ID: &str = "00000000-0000-0000-0000-000000000001";
    const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn preview_json() -> Value {
        json!({
            "source_fingerprint": FINGERPRINT,
            "draft": {
                "timing": {"sample_rate": 48000, "frame_count": 480_000},
                "playback_start_frame": 0,
                "loop_start_frame": 48000,
                "loop_end_frame": 240_000,
                "crossfade_frames": 2400,
            },
            "mode": {"kind": "from_frame", "start_frame": 0},
        })
    }

    #[test]
    fn omitted_markers_remain_omitted_and_zero_is_an_explicit_reset() -> anyhow::Result<()> {
        let id = AUDIO_FILE_ID
            .parse::<AudioFileArgument>()
            .map_err(anyhow::Error::msg)?
            .0;
        let payload = json!({"source_fingerprint": FINGERPRINT});
        let update = payload
            .to_string()
            .parse::<LoopUpdate>()
            .map_err(anyhow::Error::msg)?
            .into_request(id);
        assert_eq!(update.audio_file_id, id);
        assert_eq!(update.source_fingerprint.as_ref(), FINGERPRINT);
        assert_eq!(update.playback_start, None);
        assert_eq!(update.loop_start, None);
        assert_eq!(update.loop_end, None);
        assert_eq!(update.crossfade, None);

        let payload = json!({"source_fingerprint": FINGERPRINT, "crossfade_frames": 0});
        let update = payload
            .to_string()
            .parse::<LoopUpdate>()
            .map_err(anyhow::Error::msg)?
            .into_request(id);
        assert_eq!(update.crossfade, Some(SourceFrames::ZERO));
        assert_eq!(update.playback_start, None);
        assert_eq!(update.loop_start, None);
        assert_eq!(update.loop_end, None);
        Ok(())
    }

    #[test]
    fn malformed_marker_edits_are_clap_value_errors() -> anyhow::Result<()> {
        let base = json!({"source_fingerprint": FINGERPRINT});
        let mut cases = vec![
            json!({"source_fingerprint": "ABCDEF"}),
            json!({"source_fingerprint": FINGERPRINT, "typo": 1}),
            json!({"source_fingerprint": FINGERPRINT, "loop_end_frame": 0}),
            json!({"source_fingerprint": FINGERPRINT, "playback_start_frame": 50, "loop_start_frame": 49}),
            json!({"source_fingerprint": FINGERPRINT, "loop_start_frame": 50, "loop_end_frame": 50}),
            json!({"source_fingerprint": FINGERPRINT, "playback_start_frame": 50, "loop_end_frame": 49}),
            json!({"source_fingerprint": FINGERPRINT, "loop_start_frame": 50, "crossfade_frames": 51}),
            json!({"source_fingerprint": FINGERPRINT, "loop_start_frame": 50, "loop_end_frame": 60, "crossfade_frames": 11}),
            json!({"source_fingerprint": FINGERPRINT, "loop_start_frame": -1}),
        ];
        for field in [
            "playback_start_frame",
            "loop_start_frame",
            "loop_end_frame",
            "crossfade_frames",
        ] {
            let mut payload = base.clone();
            payload[field] = Value::Null;
            cases.push(payload);
        }
        for payload in cases {
            let text = payload.to_string();
            let error = crate::command::Cli::try_parse_from([
                "tufo",
                "loop",
                "update",
                AUDIO_FILE_ID,
                &text,
            ])
            .err()
            .with_context(|| format!("invalid loop edit was accepted: {payload}"))?;
            assert_eq!(
                error.kind(),
                ErrorKind::ValueValidation,
                "{payload}: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn invalid_drafts_and_unknown_nested_fields_are_clap_value_errors() -> anyhow::Result<()> {
        let base = preview_json();
        let mut cases = Vec::new();
        for pointer in ["", "/draft", "/draft/timing", "/mode"] {
            let mut payload = base.clone();
            payload
                .pointer_mut(pointer)
                .context("missing preview object")?["typo"] = json!(true);
            cases.push(payload);
        }
        for (pointer, value) in [
            ("/draft/timing/sample_rate", json!(0)),
            ("/draft/timing/frame_count", json!(0)),
            ("/draft/playback_start_frame", json!(48001)),
            ("/draft/loop_end_frame", json!(480_001)),
            ("/draft/loop_start_frame", json!(240_000)),
            ("/draft/crossfade_frames", json!(48001)),
            (
                "/mode",
                json!({"kind": "from_frame", "start_frame": 480_001}),
            ),
            (
                "/mode",
                json!({"kind": "from_frame", "start_frame": 0, "side_ms": 1000}),
            ),
            ("/mode", json!({"kind": "seam", "side_ms": 0})),
            ("/mode", json!({"kind": "seam", "side_ms": 10001})),
            (
                "/mode",
                json!({"kind": "seam", "side_ms": 1000, "start_frame": 0}),
            ),
        ] {
            let mut payload = base.clone();
            *payload
                .pointer_mut(pointer)
                .context("missing preview field")? = value;
            cases.push(payload);
        }
        for payload in cases {
            let text = payload.to_string();
            let error = crate::command::Cli::try_parse_from([
                "tufo",
                "loop",
                "preview",
                AUDIO_FILE_ID,
                &text,
            ])
            .err()
            .with_context(|| format!("invalid preview was accepted: {payload}"))?;
            assert_eq!(
                error.kind(),
                ErrorKind::ValueValidation,
                "{payload}: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn preview_modes_keep_source_frames_and_millisecond_units() -> anyhow::Result<()> {
        let id = AUDIO_FILE_ID
            .parse::<AudioFileArgument>()
            .map_err(anyhow::Error::msg)?
            .0;
        for start_frame in [0, 300_000, 480_000] {
            let mut payload = preview_json();
            payload["mode"]["start_frame"] = json!(start_frame);
            let request = payload
                .to_string()
                .parse::<LoopPreview>()
                .map_err(anyhow::Error::msg)?
                .into_request(id);
            assert_eq!(
                request.mode,
                PreviewMode::FromFrame(SourceFrames(start_frame))
            );
            assert_eq!(request.draft.timing().sample_rate.get(), 48000);
        }
        let mut payload = preview_json();
        payload["mode"] = json!({"kind": "seam", "side_ms": 1000});
        let request = payload
            .to_string()
            .parse::<LoopPreview>()
            .map_err(anyhow::Error::msg)?
            .into_request(id);
        let PreviewMode::Seam(side) = request.mode else {
            anyhow::bail!("the seam preview became a frame preview");
        };
        assert_eq!(side.into_inner(), Duration::from_secs(1));
        assert_eq!(request.draft.markers().crossfade, SourceFrames(2400));
        Ok(())
    }
}
