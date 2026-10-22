//! Sound effects decoded once and kept in memory, so firing one starts on the next audio period.
//!
//! One-shot sounds up to [`STATIC_SOUND_DURATION_MAX`] and loops that fit the byte budget decode
//! into memory. Larger sounds stream like music instead.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use super::{
    buffered_track::{AudioFile, OpenTrackError, duration_of_frames, frame_count, frames_in},
    memory::{
        AudioMemory, MemoryReservation, PreparationCancellation, ResidentMemory, allocation_bytes,
        reclaim_unused, retain_allocation,
    },
    packed_frame::PackedFrame,
};

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

/// A sound decoded whole at its own rate. Clones share the frames.
#[derive(Debug, Clone)]
pub struct DecodedSound {
    sample_rate: u32,
    frames: Arc<DecodedFrames>,
}

struct DecodedFrames {
    // Unlock before freeing the allocation; the housekeeping owner is always last.
    _resident: ResidentMemory,
    frames: Vec<PackedFrame>,
    _reservation: MemoryReservation,
}

impl std::fmt::Debug for DecodedFrames {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecodedFrames")
            .field("frames", &self.frames.len())
            .finish_non_exhaustive()
    }
}

impl DecodedSound {
    #[cfg(test)]
    pub(crate) fn new(sample_rate: u32, frames: Vec<PackedFrame>) -> Self {
        let memory = AudioMemory::shared();
        let reservation = memory
            .reserve(frames.capacity() * size_of::<PackedFrame>())
            .unwrap();
        Self::prepared(sample_rate, frames, reservation, &memory).unwrap()
    }

    fn prepared(
        sample_rate: u32,
        frames: Vec<PackedFrame>,
        reservation: MemoryReservation,
        memory: &AudioMemory,
    ) -> Result<Self, OpenTrackError> {
        // SAFETY: the Vec is never resized, and the guard drops before its allocation.
        let resident = unsafe { ResidentMemory::prepare(&frames, memory) };
        let frames = Arc::new(DecodedFrames {
            _resident: resident,
            frames,
            _reservation: reservation,
        });
        retain_allocation(Arc::clone(&frames)).map_err(OpenTrackError::MemoryWorker)?;
        Ok(Self {
            sample_rate,
            frames,
        })
    }

    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[must_use]
    pub fn frames(&self) -> &[PackedFrame] {
        &self.frames.frames
    }

    #[must_use]
    pub fn duration(&self) -> Duration {
        duration_of_frames(frame_count(self.frames().len()), self.sample_rate)
    }

    fn bytes(&self) -> usize {
        self.frames.frames.capacity() * size_of::<PackedFrame>()
    }

    /// Whether a clone, such as a playing sound's, holds the frames too.
    fn is_shared(&self) -> bool {
        Arc::strong_count(&self.frames) > 2
    }
}

/// A sound ready to play: decoded in memory, or too long for that and opened for streaming.
pub enum LoadedSound {
    Decoded(DecodedSound),
    Streamed(AudioFile),
}

/// Decodes at most `options.bytes_max` bytes of stereo frames. Loops may exceed
/// [`STATIC_SOUND_DURATION_MAX`]; one-shots may not. Blocks the calling thread.
pub fn load_sound(path: &Path, options: SoundLoadOptions) -> Result<LoadedSound, OpenTrackError> {
    load_sound_with_memory(
        path,
        options,
        &AudioMemory::shared(),
        &PreparationCancellation::default(),
    )
}

pub fn load_sound_with_memory(
    path: &Path,
    options: SoundLoadOptions,
    memory: &AudioMemory,
    cancellation: &PreparationCancellation,
) -> Result<LoadedSound, OpenTrackError> {
    if cancellation.is_cancelled() {
        return Err(OpenTrackError::PreparationCancelled);
    }
    let file = AudioFile::open(path)?;
    let sample_rate = file.source_sample_rate();
    let frames_max = if options.looping {
        options.bytes_max / size_of::<PackedFrame>()
    } else {
        (options.bytes_max / size_of::<PackedFrame>())
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
        .decode_whole(frames_max, memory, cancellation)
        .map_err(OpenTrackError::Unsupported)?
    {
        Some((frames, reservation)) => {
            if cancellation.is_cancelled() {
                drop(frames);
                drop(reservation);
                return Err(OpenTrackError::PreparationCancelled);
            }
            Ok(LoadedSound::Decoded(DecodedSound::prepared(
                sample_rate,
                frames,
                reservation,
                memory,
            )?))
        }
        None if cancellation.is_cancelled() => Err(OpenTrackError::PreparationCancelled),
        None => AudioFile::reopen(reopened).map(LoadedSound::Streamed),
    }
}

/// Decoded sounds, evicting the least recently used once they exceed a byte budget, and any left
/// unused for longer than an idle limit.
pub struct SoundCache {
    sounds: HashMap<SoundKey, CachedSound>,
    bytes_used: usize,
    bytes_max: usize,
    idle_max: Duration,
    uses: u64,
}

struct CachedSound {
    sound: DecodedSound,
    last_use: u64,
    last_used_at: Instant,
}

impl SoundCache {
    #[must_use]
    pub fn new(bytes_max: usize, idle_max: Duration) -> Self {
        Self {
            sounds: HashMap::new(),
            bytes_used: 0,
            bytes_max,
            idle_max,
            uses: 0,
        }
    }

    pub fn get(&mut self, key: &SoundKey, now: Instant) -> Option<DecodedSound> {
        self.uses += 1;
        let cached = self.sounds.get_mut(key)?;
        cached.last_use = self.uses;
        cached.last_used_at = now;
        Some(cached.sound.clone())
    }

    /// Drops sounds unused for the idle limit. A playing sound counts as used, since dropping it
    /// would free nothing.
    pub fn evict_idle(&mut self, now: Instant) {
        let Self {
            sounds,
            bytes_used,
            idle_max,
            ..
        } = self;
        sounds.retain(|_, cached| {
            if cached.sound.is_shared() {
                cached.last_used_at = now;
            }
            let idle = now.saturating_duration_since(cached.last_used_at) >= *idle_max;
            if idle {
                *bytes_used -= cached.sound.bytes();
            }
            !idle
        });
    }

    pub fn clear(&mut self) {
        self.sounds.clear();
        self.bytes_used = 0;
    }

    /// Releases only allocations without a playing owner; keeper reclamation runs off the mixer.
    pub fn evict_unused_for(&mut self, bytes_needed: usize, memory: &AudioMemory) {
        let bytes_needed = allocation_bytes(bytes_needed).unwrap_or(usize::MAX);
        reclaim_unused();
        while memory.available_bytes() < bytes_needed {
            let Some(key) = self
                .sounds
                .iter()
                .filter(|(_, cached)| !cached.sound.is_shared())
                .min_by_key(|(_, cached)| cached.last_use)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(cached) = self.sounds.remove(&key) {
                let bytes = cached.sound.bytes();
                self.bytes_used -= bytes;
                drop(cached);
                reclaim_unused();
            }
        }
    }

    /// Keeps `sound` unless it alone exceeds the budget.
    pub fn insert(&mut self, key: SoundKey, sound: DecodedSound, now: Instant) {
        let bytes = sound.bytes();
        if bytes > self.bytes_max {
            return;
        }
        if let Some(replaced) = self.sounds.remove(&key) {
            self.bytes_used -= replaced.sound.bytes();
        }
        while self.bytes_used + bytes > self.bytes_max {
            let Some(oldest) = self
                .sounds
                .iter()
                .filter(|(_, cached)| !cached.sound.is_shared())
                .min_by_key(|(_, sound)| sound.last_use)
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            if let Some(evicted) = self.sounds.remove(&oldest) {
                self.bytes_used -= evicted.sound.bytes();
            }
        }
        self.uses += 1;
        self.bytes_used += bytes;
        self.sounds.insert(
            key,
            CachedSound {
                sound,
                last_use: self.uses,
                last_used_at: now,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE_MAX: Duration = Duration::from_mins(10);

    fn key(name: &str) -> SoundKey {
        SoundKey {
            path: PathBuf::from(format!("/sounds/{name}.wav")),
            size_bytes: 1,
            modified: None,
        }
    }

    fn sound(frames: usize) -> DecodedSound {
        DecodedSound::new(48_000, vec![PackedFrame::default(); frames])
    }

    #[test]
    fn the_least_recently_used_sound_leaves_first() {
        let now = Instant::now();
        let frame_bytes = size_of::<PackedFrame>();
        let mut cache = SoundCache::new(100 * frame_bytes, IDLE_MAX);
        cache.insert(key("door"), sound(40), now);
        cache.insert(key("sword"), sound(40), now);
        cache.get(&key("door"), now);

        cache.insert(key("thunder"), sound(40), now);

        assert!(cache.get(&key("door"), now).is_some());
        assert!(cache.get(&key("sword"), now).is_none());
        assert!(cache.get(&key("thunder"), now).is_some());
    }

    #[test]
    fn a_sound_larger_than_the_budget_is_not_kept() {
        let now = Instant::now();
        let mut cache = SoundCache::new(10 * size_of::<PackedFrame>(), IDLE_MAX);
        cache.insert(key("door"), sound(5), now);

        cache.insert(key("storm"), sound(11), now);

        assert!(cache.get(&key("storm"), now).is_none());
        assert!(cache.get(&key("door"), now).is_some());
    }

    #[test]
    fn sounds_unused_for_the_idle_limit_leave_unless_they_play() {
        let start = Instant::now();
        let frame_bytes = size_of::<PackedFrame>();
        let mut cache = SoundCache::new(10 * frame_bytes, IDLE_MAX);
        cache.insert(key("door"), sound(5), start);
        cache.insert(key("rain"), sound(5), start);
        let playing_rain = cache.get(&key("rain"), start);

        cache.evict_idle(start + IDLE_MAX);
        drop(playing_rain);
        cache.evict_idle(start + IDLE_MAX + IDLE_MAX / 2);
        cache.insert(key("storm"), sound(5), start);

        assert!(cache.get(&key("door"), start).is_none());
        assert!(cache.get(&key("rain"), start).is_some());
        assert!(cache.get(&key("storm"), start).is_some());
    }
}
