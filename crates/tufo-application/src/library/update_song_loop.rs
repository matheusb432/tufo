//! Saving a song's loop markers against the audio they were placed on.

use tufo_models::library::{
    AudioFile,
    song_loop::{LoopMarkers, LoopRegion, LoopRegionError, SongLoop},
};
use tufo_wire::song_loops::UpdateSongLoop;

use super::{
    Library, LibraryStorageError, library_files::LibraryFiles, song_analysis::AnalyzeSongError,
    store,
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
    let base = saved.as_ref().map_or_else(
        || LoopMarkers::whole_file(analysis.timing.frame_count),
        SongLoop::markers,
    );
    let region = LoopRegion::try_new(analysis.timing, update.markers_over(base))
        .map_err(UpdateSongLoopError::InvalidMarkers)?;
    store::save_song_loop(
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
