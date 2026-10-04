use serde_json::{Value, json};
use tufo_client::song_loops::{SavedSongLoop, SongWaveform};
use tufo_models::library::song_loop::{LoopRegion, SourceTiming};

pub(crate) fn waveform(value: &SongWaveform) -> Value {
    json!({
        "fingerprint": value.fingerprint.as_ref(),
        "timing": source_timing(value.timing),
        "bucket_frames": value.bucket_frames.get(),
        "peaks": value.peaks,
    })
}

pub(crate) fn saved_loop(value: &SavedSongLoop) -> Value {
    json!({
        "region": loop_region(&value.region),
        "needs_review": value.needs_review,
    })
}

pub(crate) fn loop_region(value: &LoopRegion) -> Value {
    let markers = value.markers();
    json!({
        "timing": source_timing(value.timing()),
        "playback_start_frame": markers.playback_start.0,
        "loop_start_frame": markers.loop_start.0,
        "loop_end_frame": markers.loop_end.0,
        "crossfade_frames": markers.crossfade.0,
    })
}

fn source_timing(value: SourceTiming) -> Value {
    json!({
        "sample_rate": value.sample_rate.get(),
        "frame_count": value.frame_count.0,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use tufo_models::library::song_loop::{LoopMarkers, SourceFrames};

    use super::*;

    #[test]
    fn source_frames_remain_exact_in_the_encoded_json() -> anyhow::Result<()> {
        let markers = LoopMarkers {
            playback_start: SourceFrames(9_007_199_254_740_993),
            loop_start: SourceFrames(9_007_199_254_740_995),
            loop_end: SourceFrames(9_007_199_254_740_997),
            crossfade: SourceFrames(1),
        };
        let region = LoopRegion::try_new(
            SourceTiming {
                sample_rate: NonZeroU32::MIN,
                frame_count: SourceFrames(u64::MAX),
            },
            markers,
        )?;

        let encoded = serde_json::to_string(&loop_region(&region))?;
        let decoded: Value = serde_json::from_str(&encoded)?;

        assert_eq!(decoded["timing"]["frame_count"].as_u64(), Some(u64::MAX));
        assert_eq!(
            decoded["playback_start_frame"].as_u64(),
            Some(markers.playback_start.0)
        );
        assert_eq!(
            decoded["loop_start_frame"].as_u64(),
            Some(markers.loop_start.0)
        );
        assert_eq!(decoded["loop_end_frame"].as_u64(), Some(markers.loop_end.0));
        Ok(())
    }
}
