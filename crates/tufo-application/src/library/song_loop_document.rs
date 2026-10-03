//! The portable JSON document that carries saved loops between libraries.

use std::{num::NonZeroU32, path::PathBuf};

use serde::{Deserialize, Serialize};
use tufo_models::{
    library::{
        AudioFingerprint, FileSha256,
        song_loop::{LoopMarkers, SourceFrames, SourceTiming},
    },
    playback::AudioFilePath,
};

const DOCUMENT_FORMAT: &str = "song-loops";
const DOCUMENT_VERSION: u32 = 1;
pub const DOCUMENT_SONGS_MAX: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongLoopDocument {
    pub songs: Vec<DocumentSong>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSong {
    pub path: AudioFilePath,
    pub identity: DocumentSongIdentity,
    pub timing: SourceTiming,
    /// Checked against the matched song's timing on import.
    pub markers: LoopMarkers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentSongIdentity {
    /// Finds the song wherever it moved.
    Fingerprint(AudioFingerprint),
    /// Finds the song only at its path.
    Sha256(FileSha256),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SongLoopDocumentError {
    #[error("the document is not JSON in the expected shape: {0}")]
    Malformed(String),
    #[error("the document is not a song loop document of version {DOCUMENT_VERSION}")]
    UnsupportedFormat,
    #[error("the document lists more than {DOCUMENT_SONGS_MAX} songs")]
    TooManySongs,
    #[error("song {index} has an invalid {field}")]
    InvalidSong { index: usize, field: &'static str },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentJson {
    format: String,
    version: u32,
    songs: Vec<SongJson>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SongJson {
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    sample_rate: u32,
    frame_count: u64,
    playback_start_frame: u64,
    loop_start_frame: u64,
    loop_end_frame: u64,
    crossfade_frames: u64,
}

impl SongLoopDocument {
    pub fn parse(text: &str) -> Result<Self, SongLoopDocumentError> {
        let document = serde_json::from_str::<DocumentJson>(text)
            .map_err(|error| SongLoopDocumentError::Malformed(error.to_string()))?;
        if document.format != DOCUMENT_FORMAT || document.version != DOCUMENT_VERSION {
            return Err(SongLoopDocumentError::UnsupportedFormat);
        }
        if document.songs.len() > DOCUMENT_SONGS_MAX {
            return Err(SongLoopDocumentError::TooManySongs);
        }
        let songs = document
            .songs
            .into_iter()
            .enumerate()
            .map(|(index, song)| parse_song(index, song))
            .collect::<Result<_, _>>()?;
        Ok(Self { songs })
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        let document = DocumentJson {
            format: DOCUMENT_FORMAT.to_owned(),
            version: DOCUMENT_VERSION,
            songs: self.songs.iter().map(song_json).collect(),
        };
        serde_json::to_string_pretty(&document).unwrap_or_default()
    }
}

fn parse_song(index: usize, song: SongJson) -> Result<DocumentSong, SongLoopDocumentError> {
    let invalid = |field| SongLoopDocumentError::InvalidSong { index, field };
    let identity = match (song.fingerprint, song.sha256) {
        (Some(fingerprint), _) => DocumentSongIdentity::Fingerprint(
            AudioFingerprint::try_new(fingerprint).map_err(|_| invalid("fingerprint"))?,
        ),
        (None, Some(sha256)) => DocumentSongIdentity::Sha256(
            FileSha256::try_new(sha256).map_err(|_| invalid("sha256"))?,
        ),
        (None, None) => return Err(invalid("fingerprint or sha256")),
    };
    Ok(DocumentSong {
        path: AudioFilePath::try_new(PathBuf::from(song.path)).map_err(|_| invalid("path"))?,
        identity,
        timing: SourceTiming {
            sample_rate: NonZeroU32::new(song.sample_rate).ok_or_else(|| invalid("sample_rate"))?,
            frame_count: SourceFrames(song.frame_count),
        },
        markers: LoopMarkers {
            playback_start: SourceFrames(song.playback_start_frame),
            loop_start: SourceFrames(song.loop_start_frame),
            loop_end: SourceFrames(song.loop_end_frame),
            crossfade: SourceFrames(song.crossfade_frames),
        },
    })
}

fn song_json(song: &DocumentSong) -> SongJson {
    let (fingerprint, sha256) = match &song.identity {
        DocumentSongIdentity::Fingerprint(fingerprint) => (Some(fingerprint.to_string()), None),
        DocumentSongIdentity::Sha256(sha256) => (None, Some(sha256.to_string())),
    };
    SongJson {
        path: song.path.as_ref().to_string_lossy().into_owned(),
        fingerprint,
        sha256,
        sample_rate: song.timing.sample_rate.get(),
        frame_count: song.timing.frame_count.0,
        playback_start_frame: song.markers.playback_start.0,
        loop_start_frame: song.markers.loop_start.0,
        loop_end_frame: song.markers.loop_end.0,
        crossfade_frames: song.markers.crossfade.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(identity: DocumentSongIdentity) -> DocumentSong {
        DocumentSong {
            path: AudioFilePath::try_new(std::env::temp_dir().join("boss.ogg")).unwrap(),
            identity,
            timing: SourceTiming {
                sample_rate: NonZeroU32::new(48_000).unwrap(),
                frame_count: SourceFrames(9_600_000),
            },
            markers: LoopMarkers {
                playback_start: SourceFrames(480),
                loop_start: SourceFrames(671_738),
                loop_end: SourceFrames(4_015_879),
                crossfade: SourceFrames(0),
            },
        }
    }

    #[test]
    fn documents_survive_the_round_trip_with_either_identity() {
        let document = SongLoopDocument {
            songs: vec![
                song(DocumentSongIdentity::Fingerprint(
                    AudioFingerprint::try_new("ab".repeat(32)).unwrap(),
                )),
                song(DocumentSongIdentity::Sha256(
                    FileSha256::try_new("cd".repeat(32)).unwrap(),
                )),
            ],
        };

        assert_eq!(SongLoopDocument::parse(&document.to_json()), Ok(document));
    }

    #[test]
    fn songs_need_an_identity_and_documents_their_format() {
        let without_identity = r#"{"format":"song-loops","version":1,"songs":[{"path":"/boss.ogg",
            "sample_rate":48000,"frame_count":10,"playback_start_frame":0,"loop_start_frame":0,
            "loop_end_frame":10,"crossfade_frames":0}]}"#;
        let other_format = r#"{"format":"playlists","version":1,"songs":[]}"#;

        assert_eq!(
            SongLoopDocument::parse(without_identity),
            Err(SongLoopDocumentError::InvalidSong {
                index: 0,
                field: "fingerprint or sha256"
            })
        );
        assert_eq!(
            SongLoopDocument::parse(other_format),
            Err(SongLoopDocumentError::UnsupportedFormat)
        );
    }
}
