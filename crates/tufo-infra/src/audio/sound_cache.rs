//! Sound effects decoded once and kept in memory, so firing one starts on the next audio period.
//!
//! One-shot sounds up to [`STATIC_SOUND_DURATION_MAX`] and loops that fit the byte budget decode
//! into memory. Larger sounds stream like music instead.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use kira::{
    Frame,
    sound::static_sound::{StaticSoundData, StaticSoundSettings},
};

use super::buffered_track::{AudioFile, OpenTrackError, frames_in};

/// The longest one-shot sound decoded in memory.
pub const STATIC_SOUND_DURATION_MAX: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy)]
pub struct SoundLoadOptions {
    pub looping: bool,
    pub bytes_max: usize,
}

/// Identifies one version of a sound file, so an edited file decodes again.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SoundKey {
    path: PathBuf,
    size_bytes: u64,
    modified: Option<SystemTime>,
}

impl SoundKey {
    /// Reads the file's size and modification time. Blocks the calling thread.
    pub fn read(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            path: path.to_owned(),
            size_bytes: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

/// A sound ready to play: decoded in memory, or too long for that and opened for streaming.
pub enum LoadedSound {
    Decoded(Box<StaticSoundData>),
    Streamed(AudioFile),
}

/// Decodes at most `options.bytes_max` bytes of stereo frames. Loops may exceed
/// [`STATIC_SOUND_DURATION_MAX`]; one-shots may not. Blocks the calling thread.
pub fn load_sound(path: &Path, options: SoundLoadOptions) -> Result<LoadedSound, OpenTrackError> {
    let file = AudioFile::open(path)?;
    let sample_rate = file.source_sample_rate();
    let frames_max = if options.looping {
        options.bytes_max / size_of::<Frame>()
    } else {
        (options.bytes_max / size_of::<Frame>())
            .min(frames_in(STATIC_SOUND_DURATION_MAX, sample_rate))
    };
    if file
        .duration()
        .is_some_and(|duration| frames_in(duration, sample_rate) > frames_max)
    {
        return Ok(LoadedSound::Streamed(file));
    }
    let reopened = file.file().clone();
    match file
        .into_probed()
        .decode_whole(frames_max)
        .map_err(OpenTrackError::Unsupported)?
    {
        Some(frames) => Ok(LoadedSound::Decoded(Box::new(StaticSoundData {
            sample_rate,
            frames: frames.into(),
            settings: StaticSoundSettings::default(),
            slice: None,
        }))),
        None => AudioFile::reopen(reopened).map(LoadedSound::Streamed),
    }
}

/// Decoded sounds, evicting the least recently used once they exceed a byte budget.
pub struct SoundCache {
    sounds: HashMap<SoundKey, CachedSound>,
    bytes_used: usize,
    bytes_max: usize,
    uses: u64,
}

struct CachedSound {
    data: StaticSoundData,
    bytes: usize,
    last_use: u64,
}

impl SoundCache {
    #[must_use]
    pub fn new(bytes_max: usize) -> Self {
        Self {
            sounds: HashMap::new(),
            bytes_used: 0,
            bytes_max,
            uses: 0,
        }
    }

    pub fn get(&mut self, key: &SoundKey) -> Option<StaticSoundData> {
        self.uses += 1;
        let sound = self.sounds.get_mut(key)?;
        sound.last_use = self.uses;
        Some(sound.data.clone())
    }

    pub fn clear(&mut self) {
        self.sounds.clear();
        self.bytes_used = 0;
    }

    /// Keeps `data` unless it alone exceeds the budget.
    pub fn insert(&mut self, key: SoundKey, data: StaticSoundData) {
        let bytes = data.frames.len() * size_of::<Frame>();
        if bytes > self.bytes_max {
            return;
        }
        if let Some(replaced) = self.sounds.remove(&key) {
            self.bytes_used -= replaced.bytes;
        }
        while self.bytes_used + bytes > self.bytes_max {
            let Some(oldest) = self
                .sounds
                .iter()
                .min_by_key(|(_, sound)| sound.last_use)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(evicted) = self.sounds.remove(&oldest) {
                self.bytes_used -= evicted.bytes;
            }
        }
        self.uses += 1;
        self.bytes_used += bytes;
        self.sounds.insert(
            key,
            CachedSound {
                data,
                bytes,
                last_use: self.uses,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> SoundKey {
        SoundKey {
            path: PathBuf::from(format!("/sounds/{name}.wav")),
            size_bytes: 1,
            modified: None,
        }
    }

    fn sound(frames: usize) -> StaticSoundData {
        StaticSoundData {
            sample_rate: 48_000,
            frames: vec![Frame::ZERO; frames].into(),
            settings: StaticSoundSettings::default(),
            slice: None,
        }
    }

    #[test]
    fn the_least_recently_used_sound_leaves_first() {
        let frame_bytes = size_of::<Frame>();
        let mut cache = SoundCache::new(100 * frame_bytes);
        cache.insert(key("door"), sound(40));
        cache.insert(key("sword"), sound(40));
        cache.get(&key("door"));

        cache.insert(key("thunder"), sound(40));

        assert!(cache.get(&key("door")).is_some());
        assert!(cache.get(&key("sword")).is_none());
        assert!(cache.get(&key("thunder")).is_some());
    }

    #[test]
    fn a_sound_larger_than_the_budget_is_not_kept() {
        let mut cache = SoundCache::new(10 * size_of::<Frame>());
        cache.insert(key("door"), sound(5));

        cache.insert(key("storm"), sound(11));

        assert!(cache.get(&key("storm")).is_none());
        assert!(cache.get(&key("door")).is_some());
    }
}
