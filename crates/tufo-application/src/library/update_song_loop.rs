//! Saving a song's loop markers against the audio they were placed on.

use jiff::Timestamp;
use tufo_models::library::{
    AudioFile,
    song_loop::{LoopMarkers, LoopRegion, LoopRegionError, SongLoop, SourceFrames},
};
use tufo_wire::song_loops::UpdateSongLoop;

use super::{
    Library, LibraryStorageError, library_files::LibraryFiles, song_analysis::AnalyzeSongError,
};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum UpdateSongLoopError {
    #[error("the song does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error("the song's audio changed since its markers were placed")]
    #[meta(class = FailedPrecondition, reason = Self::SOURCE_CHANGED)]
    SourceChanged,
    #[error("the loop markers are invalid")]
    #[meta(class = InvalidArgument, reason = Self::INVALID_MARKERS)]
    InvalidMarkers(#[source] LoopRegionError),
    #[error(transparent)]
    #[meta(transparent)]
    Analysis(#[from] AnalyzeSongError),
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl UpdateSongLoopError {
    pub const NOT_FOUND: &'static str = "library.update_song_loop.not_found";
    pub const SOURCE_CHANGED: &'static str = "library.update_song_loop.source_changed";
    pub const INVALID_MARKERS: &'static str = "library.update_song_loop.invalid_markers";
}

/// Markers absent from `update` keep their saved values, or the whole file's without a saved
/// loop.
pub async fn execute<F: LibraryFiles>(
    update: UpdateSongLoop,
    library: &Library<F>,
) -> Result<(), UpdateSongLoopError> {
    let mut database = library.database();
    let file = AudioFile::filter_by_id(update.audio_file_id)
        .first()
        .exec(&mut database)
        .await
        .map_err(LibraryStorageError::from)?
        .ok_or(UpdateSongLoopError::NotFound)?;
    if file.missing_since.is_some() {
        return Err(AnalyzeSongError::Missing.into());
    }
    // Analysis can take seconds, so it runs before other changes wait for this one.
    let analysis = library.analyze_song(file.file_path).await?;
    if analysis.fingerprint != update.source_fingerprint {
        return Err(UpdateSongLoopError::SourceChanged);
    }
    let _change = library.begin_change().await;
    let saved = SongLoop::filter_by_audio_file_id(update.audio_file_id)
        .first()
        .exec(&mut database)
        .await
        .map_err(LibraryStorageError::from)?;
    let base = saved.as_ref().map_or(
        LoopMarkers {
            loop_end: analysis.timing.frame_count,
            ..LoopMarkers::default()
        },
        saved_markers,
    );
    let markers = LoopMarkers {
        playback_start: update.playback_start.unwrap_or(base.playback_start),
        loop_start: update.loop_start.unwrap_or(base.loop_start),
        loop_end: update.loop_end.unwrap_or(base.loop_end),
        crossfade: update.crossfade.unwrap_or(base.crossfade),
    };
    let region = LoopRegion::try_new(analysis.timing, markers)
        .map_err(UpdateSongLoopError::InvalidMarkers)?;
    save_region(
        &mut database,
        update.audio_file_id,
        &analysis.fingerprint,
        region,
        saved.is_some(),
    )
    .await?;
    library.publish().await?;
    Ok(())
}

pub(super) fn saved_markers(saved: &SongLoop) -> LoopMarkers {
    LoopMarkers {
        playback_start: SourceFrames(saved.playback_start_frame),
        loop_start: SourceFrames(saved.loop_start_frame),
        loop_end: SourceFrames(saved.loop_end_frame),
        crossfade: SourceFrames(saved.crossfade_frames),
    }
}

pub(super) async fn save_region(
    database: &mut toasty::Db,
    audio_file_id: tufo_models::library::AudioFileId,
    fingerprint: &tufo_models::library::AudioFingerprint,
    region: LoopRegion,
    replaces_saved: bool,
) -> Result<(), LibraryStorageError> {
    let timing = region.timing();
    let markers = region.markers();
    if replaces_saved {
        SongLoop::update_by_audio_file_id(audio_file_id)
            .fingerprint(fingerprint.clone())
            .sample_rate(timing.sample_rate.get())
            .frame_count(timing.frame_count.0)
            .playback_start_frame(markers.playback_start.0)
            .loop_start_frame(markers.loop_start.0)
            .loop_end_frame(markers.loop_end.0)
            .crossfade_frames(markers.crossfade.0)
            .updated_at(Timestamp::now())
            .exec(database)
            .await?;
    } else {
        toasty::create!(SongLoop {
            audio_file_id,
            fingerprint: fingerprint.clone(),
            sample_rate: timing.sample_rate.get(),
            frame_count: timing.frame_count.0,
            playback_start_frame: markers.playback_start.0,
            loop_start_frame: markers.loop_start.0,
            loop_end_frame: markers.loop_end.0,
            crossfade_frames: markers.crossfade.0,
            updated_at: Timestamp::now(),
        })
        .exec(database)
        .await?;
    }
    Ok(())
}
