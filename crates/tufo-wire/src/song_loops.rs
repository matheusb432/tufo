//! The process-neutral contracts for saving, sharing, and previewing song loops.

use std::{
    num::NonZeroU64,
    time::{Duration, SystemTime},
};

use tufo_models::library::{
    AudioFileId, AudioFingerprint,
    song_loop::{LoopMarkers, LoopRegion, SeamPreviewSide, SourceFrames, SourceTiming},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedSongLoop {
    pub region: LoopRegion,
    /// The file's audio changed since saving, so the song plays without the loop.
    pub needs_review: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongWaveform {
    pub fingerprint: AudioFingerprint,
    pub timing: SourceTiming,
    pub bucket_frames: NonZeroU64,
    /// Each bucket's largest absolute sample, 255 at full scale.
    pub peaks: Vec<u8>,
}

/// `None` keeps a marker as saved, or as the whole file without a saved loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSongLoop {
    pub audio_file_id: AudioFileId,
    pub source_fingerprint: AudioFingerprint,
    pub playback_start: Option<SourceFrames>,
    pub loop_start: Option<SourceFrames>,
    pub loop_end: Option<SourceFrames>,
    pub crossfade: Option<SourceFrames>,
}

impl UpdateSongLoop {
    /// `base` supplies each marker the update leaves out.
    #[must_use]
    pub fn markers_over(&self, base: LoopMarkers) -> LoopMarkers {
        LoopMarkers {
            playback_start: self.playback_start.unwrap_or(base.playback_start),
            loop_start: self.loop_start.unwrap_or(base.loop_start),
            loop_end: self.loop_end.unwrap_or(base.loop_end),
            crossfade: self.crossfade.unwrap_or(base.crossfade),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSongLoops {
    pub document: String,
    pub dry_run: bool,
    pub replace_existing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongLoopImportResult {
    /// As the document names it.
    pub path: String,
    pub audio_file_id: Option<AudioFileId>,
    pub outcome: SongLoopImportOutcome,
}

/// On a dry run, what saving would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SongLoopImportOutcome {
    Saved,
    Replaced,
    Unchanged,
    /// Other markers are saved, and the import doesn't replace them.
    Conflict,
    NoMatch,
    /// The song at the document's path holds other audio.
    IdentityMismatch,
    /// The song decodes at another sample rate or length.
    LengthMismatch,
    InvalidMarkers,
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewSongLoop {
    pub audio_file_id: AudioFileId,
    pub source_fingerprint: AudioFingerprint,
    pub draft: LoopRegion,
    pub mode: PreviewMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewMode {
    /// Plays the draft as the song would, wrapping at its loop end.
    FromFrame(SourceFrames),
    /// Repeats the audio before the loop end, the seam, and the audio after the loop start.
    Seam(SeamPreviewSide),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewStarted {
    pub started_at: SystemTime,
    /// One seam repetition, including the silence after it.
    pub seam_cycle: Option<Duration>,
}
