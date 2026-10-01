//! What the server plays and where it plays it.

use std::path::PathBuf;

use nutype::nutype;

use crate::discord::VoiceChannelTarget;

pub mod diagnostics;
pub mod music_layers;
pub mod player_settings;
pub mod player_state;
pub mod queue;
pub mod saved_loops;

/// Extensions of the audio formats the server reads, compared case-insensitively. Reading probes
/// content, so a mislabeled file still plays.
pub const AUDIO_FILE_EXTENSIONS: [&str; 10] = [
    "aac", "flac", "m4a", "mp3", "mp4", "oga", "ogg", "opus", "wav", "wave",
];

/// An absolute, UTF-8 path to an audio file on the server's filesystem.
#[nutype(
    validate(predicate = crate::path::is_absolute_utf8),
    derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, AsRef)
)]
pub struct AudioFilePath(PathBuf);

/// Where the server sends audio. Exactly one output is active at a time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputTarget {
    /// The server machine's default audio device.
    #[default]
    Local,
    /// One Discord voice channel, joined by the bot.
    DiscordVoice(VoiceChannelTarget),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_file_paths_must_be_absolute() {
        assert!(AudioFilePath::try_new(std::env::temp_dir().join("tavern.mp3")).is_ok());
        assert!(AudioFilePath::try_new(PathBuf::from("music/rpg/tavern.mp3")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn audio_file_paths_must_be_utf8() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _};

        let path = PathBuf::from(OsStr::from_bytes(b"/music/\xff.mp3"));

        assert!(AudioFilePath::try_new(path).is_err());
    }
}
