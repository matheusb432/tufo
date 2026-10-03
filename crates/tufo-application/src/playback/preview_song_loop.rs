//! Auditioning a draft loop before saving it.

use tufo_models::library::AudioFile;
use tufo_wire::song_loops::PreviewSongLoop;

use super::player::{PlayAudioError, Player, PreviewSession, SongPreview};
use crate::{
    error::ErrorMeta,
    library::{
        Library, LibraryStorageError, library_files::LibraryFiles, song_analysis::AnalyzeSongError,
    },
};

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum PreviewSongLoopError {
    #[error("the song does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error("the song's audio changed since the draft was placed")]
    #[meta(class = FailedPrecondition, reason = Self::SOURCE_CHANGED)]
    SourceChanged,
    #[error(transparent)]
    #[meta(transparent)]
    Analysis(#[from] AnalyzeSongError),
    #[error(transparent)]
    #[meta(transparent)]
    Audio(#[from] PlayAudioError),
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl PreviewSongLoopError {
    pub const NOT_FOUND: &'static str = "playback.preview_song_loop.not_found";
    pub const SOURCE_CHANGED: &'static str = "playback.preview_song_loop.source_changed";
}

pub async fn execute<F: LibraryFiles>(
    request: PreviewSongLoop,
    library: &Library<F>,
    player: &impl Player,
) -> Result<PreviewSession, PreviewSongLoopError> {
    let file = AudioFile::filter_by_id(request.audio_file_id)
        .first()
        .exec(&mut library.database())
        .await
        .map_err(LibraryStorageError::from)?
        .ok_or(PreviewSongLoopError::NotFound)?;
    if file.missing_since.is_some() {
        return Err(AnalyzeSongError::Missing.into());
    }
    let analysis = library.analyze_song(file.file_path.clone()).await?;
    if analysis.fingerprint != request.source_fingerprint
        || analysis.timing != request.draft.timing()
    {
        return Err(PreviewSongLoopError::SourceChanged);
    }
    let preview = SongPreview {
        path: file.file_path,
        draft: request.draft,
        mode: request.mode,
    };
    Ok(player.start_preview(preview).await?)
}

pub async fn stop(id: super::player::PreviewId, player: &impl Player) {
    player.stop_preview(id).await;
}
