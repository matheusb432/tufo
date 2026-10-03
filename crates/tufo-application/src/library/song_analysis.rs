//! Whole-file analyses, which the loop editor, saving, importing, and previews share.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, PoisonError},
};

use tufo_models::{library::FileStamp, playback::AudioFilePath};
use tufo_wire::song_loops::SongWaveform;

use super::{
    Library,
    library_files::{InspectAudioError, LibraryFiles},
};
use crate::error::ErrorMeta;

const SONG_ANALYSES_CACHED_MAX: usize = 4;

#[derive(Default)]
pub(super) struct SongAnalyses {
    recent: Mutex<VecDeque<CachedAnalysis>>,
}

struct CachedAnalysis {
    path: AudioFilePath,
    stamp: FileStamp,
    waveform: Arc<SongWaveform>,
}

impl SongAnalyses {
    fn find(&self, path: &AudioFilePath, stamp: &FileStamp) -> Option<Arc<SongWaveform>> {
        self.lock()
            .iter()
            .find(|cached| cached.path == *path && cached.stamp == *stamp)
            .map(|cached| Arc::clone(&cached.waveform))
    }

    fn remember(&self, path: AudioFilePath, stamp: FileStamp, waveform: Arc<SongWaveform>) {
        let mut recent = self.lock();
        recent.retain(|cached| cached.path != path);
        if recent.len() == SONG_ANALYSES_CACHED_MAX {
            recent.pop_back();
        }
        recent.push_front(CachedAnalysis {
            path,
            stamp,
            waveform,
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<CachedAnalysis>> {
        self.recent.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum AnalyzeSongError {
    #[error("the song's audio file is missing")]
    #[meta(class = FailedPrecondition, reason = Self::MISSING)]
    Missing,
    #[error("the audio file cannot be read")]
    #[meta(class = FailedPrecondition, reason = Self::UNREADABLE)]
    Unreadable(#[source] std::io::Error),
    #[error("the file holds no audio track Tufo can decode")]
    #[meta(class = FailedPrecondition, reason = Self::UNSUPPORTED)]
    Unsupported(#[source] anyhow::Error),
}

impl AnalyzeSongError {
    pub const MISSING: &'static str = "library.analyze_song.missing";
    pub const UNREADABLE: &'static str = "library.analyze_song.unreadable";
    pub const UNSUPPORTED: &'static str = "library.analyze_song.unsupported";
}

impl<F: LibraryFiles> Library<F> {
    /// Reuses a recent analysis while the file's stamp is unchanged.
    pub(crate) async fn analyze_song(
        &self,
        path: AudioFilePath,
    ) -> Result<Arc<SongWaveform>, AnalyzeSongError> {
        let stamp = self
            .files()
            .stat_file(path.clone())
            .await
            .map_err(AnalyzeSongError::Unreadable)?
            .ok_or(AnalyzeSongError::Missing)?;
        if let Some(waveform) = self.song_analyses.find(&path, &stamp) {
            return Ok(waveform);
        }
        let waveform = Arc::new(self.files().analyze_audio(path.clone()).await.map_err(
            |error| match error {
                InspectAudioError::Unreadable(source) => AnalyzeSongError::Unreadable(source),
                InspectAudioError::Unsupported(source) => AnalyzeSongError::Unsupported(source),
            },
        )?);
        self.song_analyses
            .remember(path, stamp, Arc::clone(&waveform));
        Ok(waveform)
    }
}
