//! A song's peaks and exact length, for placing its loop markers.

use std::sync::Arc;

use tufo_models::library::{AudioFile, AudioFileId};
use tufo_wire::song_loops::SongWaveform;

use super::{
    Library, LibraryStorageError, library_files::LibraryFiles, song_analysis::AnalyzeSongError,
};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum GetSongWaveformError {
    #[error("the song does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error(transparent)]
    #[meta(transparent)]
    Analysis(#[from] AnalyzeSongError),
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl GetSongWaveformError {
    pub const NOT_FOUND: &'static str = "library.get_song_waveform.not_found";
}

pub async fn execute<F: LibraryFiles>(
    id: AudioFileId,
    library: &Library<F>,
) -> Result<Arc<SongWaveform>, GetSongWaveformError> {
    let file = AudioFile::filter_by_id(id)
        .first()
        .exec(&mut library.database())
        .await
        .map_err(LibraryStorageError::from)?
        .ok_or(GetSongWaveformError::NotFound)?;
    if file.missing_since.is_some() {
        return Err(AnalyzeSongError::Missing.into());
    }
    Ok(library.analyze_song(file.file_path).await?)
}
