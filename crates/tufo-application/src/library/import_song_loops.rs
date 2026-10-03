//! Saving loops from a portable document, each matched to a song and checked against its audio.

use std::collections::HashMap;

use tufo_models::library::{
    AudioFile, AudioFileId, AudioFingerprint,
    song_loop::{LoopMarkers, LoopRegion, SongLoop},
};
use tufo_wire::song_loops::{ImportSongLoops, SongLoopImportOutcome, SongLoopImportResult};

use super::{
    Library, LibraryStorageError,
    library_files::LibraryFiles,
    song_loop_document::{
        DocumentSong, DocumentSongIdentity, SongLoopDocument, SongLoopDocumentError,
    },
    update_song_loop::{save_region, saved_markers},
};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum ImportSongLoopsError {
    #[error("the song loop document is invalid")]
    #[meta(class = InvalidArgument, reason = Self::INVALID_DOCUMENT)]
    InvalidDocument(#[source] SongLoopDocumentError),
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl ImportSongLoopsError {
    pub const INVALID_DOCUMENT: &'static str = "library.import_song_loops.invalid_document";
}

struct MatchedSong {
    audio_file_id: AudioFileId,
    fingerprint: AudioFingerprint,
    region: LoopRegion,
}

/// Results follow the document's order; a dry run reports what saving would do.
pub async fn execute<F: LibraryFiles>(
    request: ImportSongLoops,
    library: &Library<F>,
) -> Result<Vec<SongLoopImportResult>, ImportSongLoopsError> {
    let document = SongLoopDocument::parse(&request.document)
        .map_err(ImportSongLoopsError::InvalidDocument)?;
    let mut database = library.database();
    let present_files = AudioFile::all()
        .exec(&mut database)
        .await
        .map_err(LibraryStorageError::from)?
        .into_iter()
        .filter(|file| file.missing_since.is_none())
        .collect::<Vec<_>>();
    let mut song_matches = Vec::with_capacity(document.songs.len());
    for song in &document.songs {
        song_matches.push(match_song(song, &present_files, library).await);
    }

    let _change = library.begin_change().await;
    let mut saved_loops = SongLoop::all()
        .exec(&mut database)
        .await
        .map_err(LibraryStorageError::from)?
        .into_iter()
        .map(|saved| {
            let markers = saved_markers(&saved);
            (saved.audio_file_id, (saved.fingerprint, markers))
        })
        .collect::<HashMap<AudioFileId, (AudioFingerprint, LoopMarkers)>>();
    let mut results = Vec::with_capacity(song_matches.len());
    let mut saved_any = false;
    for (song, matched) in document.songs.iter().zip(song_matches) {
        let path = song.path.as_ref().to_string_lossy().into_owned();
        let matched = match matched {
            Ok(matched) => matched,
            Err(outcome) => {
                results.push(SongLoopImportResult {
                    path,
                    audio_file_id: None,
                    outcome,
                });
                continue;
            }
        };
        let saved = saved_loops.get(&matched.audio_file_id);
        let outcome = match saved {
            None => SongLoopImportOutcome::Saved,
            Some((fingerprint, markers))
                if *fingerprint == matched.fingerprint && *markers == matched.region.markers() =>
            {
                SongLoopImportOutcome::Unchanged
            }
            Some(_) if request.replace_existing => SongLoopImportOutcome::Replaced,
            Some(_) => SongLoopImportOutcome::Conflict,
        };
        if matches!(
            outcome,
            SongLoopImportOutcome::Saved | SongLoopImportOutcome::Replaced
        ) {
            if !request.dry_run {
                save_region(
                    &mut database,
                    matched.audio_file_id,
                    &matched.fingerprint,
                    matched.region,
                    saved.is_some(),
                )
                .await?;
                saved_any = true;
            }
            saved_loops.insert(
                matched.audio_file_id,
                (matched.fingerprint, matched.region.markers()),
            );
        }
        results.push(SongLoopImportResult {
            path,
            audio_file_id: Some(matched.audio_file_id),
            outcome,
        });
    }
    if saved_any {
        library.publish().await?;
    }
    Ok(results)
}

/// A fingerprint finds the song wherever it moved, preferring the document's path; a SHA-256
/// only checks the song at that path.
async fn match_song<F: LibraryFiles>(
    song: &DocumentSong,
    present_files: &[AudioFile],
    library: &Library<F>,
) -> Result<MatchedSong, SongLoopImportOutcome> {
    let file_at_path = present_files
        .iter()
        .find(|file| file.file_path == song.path);
    let file = match &song.identity {
        DocumentSongIdentity::Fingerprint(fingerprint) => present_files
            .iter()
            .filter(|file| file.fingerprint == *fingerprint)
            .min_by_key(|file| file.file_path != song.path)
            .ok_or(if file_at_path.is_some() {
                SongLoopImportOutcome::IdentityMismatch
            } else {
                SongLoopImportOutcome::NoMatch
            })?,
        DocumentSongIdentity::Sha256(sha256) => {
            let file = file_at_path.ok_or(SongLoopImportOutcome::NoMatch)?;
            let actual = library
                .files()
                .sha256_of_file(file.file_path.clone())
                .await
                .map_err(|_| SongLoopImportOutcome::Unreadable)?;
            if actual != *sha256 {
                return Err(SongLoopImportOutcome::IdentityMismatch);
            }
            file
        }
    };
    let analysis = library
        .analyze_song(file.file_path.clone())
        .await
        .map_err(|_| SongLoopImportOutcome::Unreadable)?;
    if analysis.timing != song.timing {
        return Err(SongLoopImportOutcome::LengthMismatch);
    }
    let region = LoopRegion::try_new(analysis.timing, song.markers)
        .map_err(|_| SongLoopImportOutcome::InvalidMarkers)?;
    Ok(MatchedSong {
        audio_file_id: file.id,
        fingerprint: analysis.fingerprint.clone(),
        region,
    })
}
