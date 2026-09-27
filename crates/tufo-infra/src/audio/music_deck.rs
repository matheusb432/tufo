//! The music player inside a mixer: one kira sound that plays tracks back to back.
//!
//! The deck owns every music track the audio thread reads, so it starts the next track on the
//! frame the current one reaches its crossfade point, or on the frame after its last one when
//! crossfading is off, without waiting for the control side. The control side sends commands
//! through a wait-free queue and reads two atomics. Tracks the deck lets go travel back through a
//! second queue, so their buffers are freed off the audio thread.

use std::{
    convert::Infallible,
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use kira::{
    Frame,
    info::Info,
    sound::{Sound, SoundData},
};
use rtrb::{Consumer, Producer, RingBuffer};

use super::{
    buffered_track::{TrackReader, frames_in},
    envelope::Envelope,
};

const COMMAND_CAPACITY: usize = 64;
const RETIRED_CAPACITY: usize = 16;
/// The current track, and tracks still fading out behind it.
const VOICES_MAX: usize = 4;
const SCRATCH_FRAMES: usize = 1024;

/// A track handed to the deck, named by a token the control side chose.
pub struct DeckTrack {
    pub token: NonZeroU64,
    pub reader: TrackReader,
}

/// What the deck is doing once it has applied every command sent to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckState {
    /// Commands are still on their way to the audio thread.
    Pending,
    /// No track is current: nothing started yet, it was stopped, or the last one ended.
    Idle,
    Playing(NonZeroU64),
}

#[derive(Debug, thiserror::Error)]
#[error("the music deck's command queue is full")]
pub struct DeckBusyError;

/// Creates a deck for a mixer running at `sample_rate`: the sound to play on the music track and
/// the handle that controls it.
pub(crate) fn music_deck(sample_rate: u32) -> (MusicDeckSound, MusicDeckHandle) {
    let (command_producer, command_consumer) = RingBuffer::new(COMMAND_CAPACITY);
    let (retired_producer, retired_consumer) = RingBuffer::new(RETIRED_CAPACITY);
    let status = Arc::new(DeckStatus::default());
    let deck = MusicDeck {
        commands: command_consumer,
        retired: retired_producer,
        status: Arc::clone(&status),
        voices: [const { None }; VOICES_MAX],
        current: None,
        armed: None,
        crossfade_frames: 0,
        pause: Envelope::constant(1.0),
        scratch: vec![Frame::ZERO; SCRATCH_FRAMES].into_boxed_slice(),
        commands_applied: 0,
    };
    let handle = MusicDeckHandle {
        commands: command_producer,
        retired: retired_consumer,
        status,
        commands_sent: 0,
        sample_rate,
    };
    (MusicDeckSound(deck), handle)
}

/// Controls a deck from outside the audio thread.
pub struct MusicDeckHandle {
    commands: Producer<DeckCommand>,
    retired: Consumer<TrackReader>,
    status: Arc<DeckStatus>,
    commands_sent: u64,
    sample_rate: u32,
}

impl MusicDeckHandle {
    /// Makes `track` current at once: it fades in over `fade_in` while every other track fades
    /// out over `fade_out`, and the armed track is dropped.
    pub fn start(
        &mut self,
        track: DeckTrack,
        fade_in: Duration,
        fade_out: Duration,
    ) -> Result<(), DeckBusyError> {
        let command = DeckCommand::Start {
            track,
            fade_in_frames: self.frames(fade_in),
            fade_out_frames: self.frames(fade_out),
        };
        self.send(command)
    }

    /// Plays `track` after the current one: from the current track's crossfade point, or right
    /// after its last frame without a crossfade. Replaces an armed track.
    pub fn arm(&mut self, track: DeckTrack) -> Result<(), DeckBusyError> {
        self.send(DeckCommand::Arm(track))
    }

    pub fn disarm(&mut self) -> Result<(), DeckBusyError> {
        self.send(DeckCommand::Disarm)
    }

    pub fn set_crossfade(&mut self, crossfade: Duration) -> Result<(), DeckBusyError> {
        let frames = self.frames(crossfade);
        self.send(DeckCommand::SetCrossfade { frames })
    }

    /// Fades the deck out over `fade`, then stops reading its tracks until [`Self::resume`].
    pub fn pause(&mut self, fade: Duration) -> Result<(), DeckBusyError> {
        let fade_frames = self.frames(fade);
        self.send(DeckCommand::Pause { fade_frames })
    }

    pub fn resume(&mut self, fade: Duration) -> Result<(), DeckBusyError> {
        let fade_frames = self.frames(fade);
        self.send(DeckCommand::Resume { fade_frames })
    }

    /// Fades every track out over `fade` and drops the armed one.
    pub fn stop(&mut self, fade: Duration) -> Result<(), DeckBusyError> {
        let fade_frames = self.frames(fade);
        self.send(DeckCommand::Stop { fade_frames })
    }

    #[must_use]
    pub fn state(&self) -> DeckState {
        // The deck stores the token before the count, so a matching count covers the token.
        if self.status.commands_applied.load(Ordering::Acquire) != self.commands_sent {
            return DeckState::Pending;
        }
        match NonZeroU64::new(self.status.current_token.load(Ordering::Acquire)) {
            Some(token) => DeckState::Playing(token),
            None => DeckState::Idle,
        }
    }

    /// Frees the tracks the deck let go of.
    pub fn drop_retired(&mut self) {
        while let Ok(reader) = self.retired.pop() {
            drop(reader);
        }
    }

    fn send(&mut self, command: DeckCommand) -> Result<(), DeckBusyError> {
        self.commands.push(command).map_err(|_| DeckBusyError)?;
        self.commands_sent += 1;
        Ok(())
    }

    fn frames(&self, duration: Duration) -> u32 {
        u32::try_from(frames_in(duration, self.sample_rate)).unwrap_or(u32::MAX)
    }
}

/// The deck as kira plays it; it never finishes on its own.
pub struct MusicDeckSound(MusicDeck);

impl SoundData for MusicDeckSound {
    type Error = Infallible;
    type Handle = ();

    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), Self::Error> {
        Ok((Box::new(self.0), ()))
    }
}

#[derive(Debug, Default)]
struct DeckStatus {
    current_token: AtomicU64,
    commands_applied: AtomicU64,
}

enum DeckCommand {
    Start {
        track: DeckTrack,
        fade_in_frames: u32,
        fade_out_frames: u32,
    },
    Arm(DeckTrack),
    Disarm,
    SetCrossfade {
        frames: u32,
    },
    Pause {
        fade_frames: u32,
    },
    Resume {
        fade_frames: u32,
    },
    Stop {
        fade_frames: u32,
    },
}

struct Voice {
    track: DeckTrack,
    envelope: Envelope,
    /// Fading out towards removal; only the current voice isn't.
    retiring: bool,
}

struct MusicDeck {
    commands: Consumer<DeckCommand>,
    retired: Producer<TrackReader>,
    status: Arc<DeckStatus>,
    voices: [Option<Voice>; VOICES_MAX],
    /// The slot of the voice playing the current track.
    current: Option<usize>,
    armed: Option<DeckTrack>,
    crossfade_frames: u32,
    /// Full level while playing, silence once paused.
    pause: Envelope,
    scratch: Box<[Frame]>,
    commands_applied: u64,
}

impl MusicDeck {
    fn apply_commands(&mut self) {
        while let Ok(command) = self.commands.pop() {
            match command {
                DeckCommand::Start {
                    track,
                    fade_in_frames,
                    fade_out_frames,
                } => self.start(track, fade_in_frames, fade_out_frames),
                DeckCommand::Arm(track) => {
                    self.retire_armed();
                    self.armed = Some(track);
                }
                DeckCommand::Disarm => self.retire_armed(),
                DeckCommand::SetCrossfade { frames } => self.crossfade_frames = frames,
                DeckCommand::Pause { fade_frames } => self.pause.ramp_to(0.0, fade_frames),
                DeckCommand::Resume { fade_frames } => self.pause.ramp_to(1.0, fade_frames),
                DeckCommand::Stop { fade_frames } => {
                    self.retire_armed();
                    self.fade_out_all(fade_frames);
                }
            }
            self.commands_applied += 1;
        }
    }

    fn start(&mut self, track: DeckTrack, fade_in_frames: u32, fade_out_frames: u32) {
        self.retire_armed();
        self.fade_out_all(fade_out_frames);
        let slot = self.free_slot();
        let envelope = match fade_in_frames {
            0 => Envelope::constant(1.0),
            frames => Envelope::ramp(0.0, 1.0, frames),
        };
        self.voices[slot] = Some(Voice {
            track,
            envelope,
            retiring: false,
        });
        self.current = Some(slot);
    }

    fn fade_out_all(&mut self, fade_frames: u32) {
        for slot in 0..VOICES_MAX {
            if fade_frames == 0 {
                self.retire(slot);
            } else if let Some(voice) = &mut self.voices[slot] {
                voice.retiring = true;
                voice.envelope.ramp_to(0.0, fade_frames);
            }
        }
        self.current = None;
    }

    /// An empty slot, making one by dropping the quietest fading voice when all are taken.
    fn free_slot(&mut self) -> usize {
        if let Some(slot) = self.voices.iter().position(Option::is_none) {
            return slot;
        }
        let quietest = (0..VOICES_MAX)
            .filter(|&slot| Some(slot) != self.current)
            .min_by(|&left, &right| {
                let gain = |slot: usize| {
                    self.voices[slot]
                        .as_ref()
                        .map_or(0.0, |voice| voice.envelope.gain())
                };
                gain(left).total_cmp(&gain(right))
            })
            .unwrap_or(0);
        self.retire(quietest);
        if self.current == Some(quietest) {
            self.current = None;
        }
        quietest
    }

    /// Starts the armed track once the current one is within the crossfade of its end.
    fn start_crossfade_if_due(&mut self) {
        if self.crossfade_frames == 0 || self.armed.is_none() {
            return;
        }
        let Some(current) = self.current else {
            return;
        };
        let Some(voice) = &mut self.voices[current] else {
            return;
        };
        let Some(remaining) = voice.track.reader.remaining_frames() else {
            return;
        };
        // At zero frames the current track has ended, and the gapless handoff takes over.
        if remaining == 0 || remaining > u64::from(self.crossfade_frames) {
            return;
        }
        let length = u32::try_from(remaining).unwrap_or(u32::MAX);
        voice.retiring = true;
        voice.envelope.ramp_to(0.0, length);
        self.current = None;
        let Some(next) = self.armed.take() else {
            return;
        };
        let slot = self.free_slot();
        self.voices[slot] = Some(Voice {
            track: next,
            envelope: Envelope::ramp(0.0, 1.0, length),
            retiring: false,
        });
        self.current = Some(slot);
    }

    fn render(&mut self, chunk: &mut [Frame]) {
        self.start_crossfade_if_due();
        for slot in 0..VOICES_MAX {
            self.render_voice(slot, chunk);
        }
        for frame in chunk {
            *frame *= self.pause.next_gain();
        }
    }

    /// Mixes one voice into `out`. When the current voice runs out mid-chunk, the armed track
    /// takes its slot and fills the rest of the chunk, so no frame of silence separates them.
    fn render_voice(&mut self, slot: usize, out: &mut [Frame]) {
        let Some(voice) = &mut self.voices[slot] else {
            return;
        };
        let scratch = &mut self.scratch[..out.len()];
        let copied = voice.track.reader.read(scratch);
        for (mixed, frame) in out.iter_mut().zip(&scratch[..copied]) {
            *mixed += *frame * voice.envelope.next_gain();
        }
        let ended = copied < out.len() && voice.track.reader.is_exhausted();
        let faded_out =
            voice.retiring && voice.envelope.is_settled() && voice.envelope.target() <= 0.0;
        if !ended && !faded_out {
            return;
        }
        self.retire(slot);
        if self.current != Some(slot) {
            return;
        }
        self.current = None;
        if let Some(next) = self.armed.take().filter(|_| ended) {
            self.voices[slot] = Some(Voice {
                track: next,
                envelope: Envelope::constant(1.0),
                retiring: false,
            });
            self.current = Some(slot);
            self.render_voice(slot, &mut out[copied..]);
        }
    }

    fn retire(&mut self, slot: usize) {
        if let Some(voice) = self.voices[slot].take() {
            self.retire_reader(voice.track.reader);
        }
    }

    fn retire_armed(&mut self) {
        if let Some(track) = self.armed.take() {
            self.retire_reader(track.reader);
        }
    }

    /// Hands a reader back for freeing; with the queue full, the audio thread frees it.
    fn retire_reader(&mut self, reader: TrackReader) {
        let _ = self.retired.push(reader);
    }

    fn publish_status(&self) {
        let token = self
            .current
            .and_then(|slot| self.voices[slot].as_ref())
            .map_or(0, |voice| voice.track.token.get());
        self.status.current_token.store(token, Ordering::Release);
        self.status
            .commands_applied
            .store(self.commands_applied, Ordering::Release);
    }

    fn is_paused(&self) -> bool {
        self.pause.target() <= 0.0 && self.pause.is_settled()
    }
}

impl Sound for MusicDeck {
    fn process(&mut self, out: &mut [Frame], _dt: f64, _info: &Info) {
        self.apply_commands();
        out.fill(Frame::ZERO);
        if !self.is_paused() {
            for chunk in out.chunks_mut(SCRATCH_FRAMES) {
                self.render(chunk);
            }
        }
        self.publish_status();
    }

    fn finished(&self) -> bool {
        false
    }
}
