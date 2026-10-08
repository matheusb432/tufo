//! What the browser host reports to the server that started it, over a pipe: each event is a
//! `BrowserHostEvent` protobuf after its length as a 4-byte little-endian integer.

use std::{
    io::{self, Read, Write},
    num::NonZeroU32,
};

use prost::Message as _;

use crate::pb::browser::{
    BrowserAudioPacket, BrowserAudioStarted, BrowserAudioStopped, BrowserHostEvent,
    BrowserHostReady, BrowserViewChanged, browser_host_event::Event,
};

/// The largest event either side accepts, far above one audio packet.
pub const HOST_EVENT_BYTES_MAX: usize = 1024 * 1024;
const SAMPLE_BYTES: usize = 4;

/// One page's audio stream, named by the host until it stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AudioStreamId(pub NonZeroU32);

#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// The host accepts views and commands on its endpoint.
    Ready,
    AudioStarted {
        stream: AudioStreamId,
        sample_rate: NonZeroU32,
        channels: NonZeroU32,
    },
    /// Interleaved samples in whole frames of the stream's channel count.
    AudioPacket {
        stream: AudioStreamId,
        samples: Vec<f32>,
    },
    AudioStopped {
        stream: AudioStreamId,
    },
    /// Whether a view shows the browser now.
    ViewChanged {
        attached: bool,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ReadHostEventError {
    #[error("reading the browser host's pipe failed")]
    Io(#[from] io::Error),
    #[error("the browser host sent an event of {0} bytes")]
    TooLarge(usize),
    #[error("the browser host sent an event that does not decode")]
    Decode(#[from] prost::DecodeError),
    #[error("the browser host sent an invalid {0}")]
    Invalid(&'static str),
}

/// Writes one event and its length prefix.
pub fn write_host_event(writer: &mut (impl Write + ?Sized), event: &HostEvent) -> io::Result<()> {
    let encoded = host_event_message(event).encode_to_vec();
    let length = u32::try_from(encoded.len())
        .ok()
        .filter(|length| *length as usize <= HOST_EVENT_BYTES_MAX)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "host event too large"))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&encoded)
}

/// Reads the next event, or `None` once the pipe closed between events.
pub fn read_host_event(
    reader: &mut (impl Read + ?Sized),
) -> Result<Option<HostEvent>, ReadHostEventError> {
    let mut length = [0; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_le_bytes(length) as usize;
    if length > HOST_EVENT_BYTES_MAX {
        return Err(ReadHostEventError::TooLarge(length));
    }
    let mut encoded = vec![0; length];
    reader.read_exact(&mut encoded)?;
    host_event(BrowserHostEvent::decode(encoded.as_slice())?).map(Some)
}

fn host_event_message(event: &HostEvent) -> BrowserHostEvent {
    let event = match event {
        HostEvent::Ready => Event::Ready(BrowserHostReady {}),
        HostEvent::AudioStarted {
            stream,
            sample_rate,
            channels,
        } => Event::AudioStarted(BrowserAudioStarted {
            stream_id: stream.0.get(),
            sample_rate: sample_rate.get(),
            channels: channels.get(),
        }),
        HostEvent::AudioPacket { stream, samples } => Event::AudioPacket(BrowserAudioPacket {
            stream_id: stream.0.get(),
            samples: samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect(),
        }),
        HostEvent::AudioStopped { stream } => Event::AudioStopped(BrowserAudioStopped {
            stream_id: stream.0.get(),
        }),
        HostEvent::ViewChanged { attached } => Event::ViewChanged(BrowserViewChanged {
            attached: *attached,
        }),
    };
    BrowserHostEvent { event: Some(event) }
}

fn host_event(message: BrowserHostEvent) -> Result<HostEvent, ReadHostEventError> {
    let stream = |id| {
        NonZeroU32::new(id)
            .map(AudioStreamId)
            .ok_or(ReadHostEventError::Invalid("stream_id"))
    };
    Ok(
        match message.event.ok_or(ReadHostEventError::Invalid("event"))? {
            Event::Ready(_) => HostEvent::Ready,
            Event::AudioStarted(started) => HostEvent::AudioStarted {
                stream: stream(started.stream_id)?,
                sample_rate: NonZeroU32::new(started.sample_rate)
                    .ok_or(ReadHostEventError::Invalid("sample_rate"))?,
                channels: NonZeroU32::new(started.channels)
                    .ok_or(ReadHostEventError::Invalid("channels"))?,
            },
            Event::AudioPacket(packet) => {
                let (samples, partial) = packet.samples.as_chunks::<SAMPLE_BYTES>();
                if !partial.is_empty() {
                    return Err(ReadHostEventError::Invalid("samples"));
                }
                HostEvent::AudioPacket {
                    stream: stream(packet.stream_id)?,
                    samples: samples
                        .iter()
                        .map(|bytes| f32::from_le_bytes(*bytes))
                        .collect(),
                }
            }
            Event::AudioStopped(stopped) => HostEvent::AudioStopped {
                stream: stream(stopped.stream_id)?,
            },
            Event::ViewChanged(changed) => HostEvent::ViewChanged {
                attached: changed.attached,
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(id: u32) -> AudioStreamId {
        AudioStreamId(NonZeroU32::new(id).unwrap())
    }

    #[test]
    fn events_survive_the_pipe_in_order() {
        let events = [
            HostEvent::Ready,
            HostEvent::AudioStarted {
                stream: stream(4),
                sample_rate: NonZeroU32::new(48_000).unwrap(),
                channels: NonZeroU32::new(2).unwrap(),
            },
            HostEvent::AudioPacket {
                stream: stream(4),
                samples: vec![0.25, -0.5, 1.0, -1.0],
            },
            HostEvent::ViewChanged { attached: true },
            HostEvent::AudioStopped { stream: stream(4) },
        ];
        let mut pipe = Vec::new();
        for event in &events {
            write_host_event(&mut pipe, event).unwrap();
        }

        let mut reader = pipe.as_slice();
        for event in events {
            assert_eq!(read_host_event(&mut reader).unwrap(), Some(event));
        }
        assert_eq!(read_host_event(&mut reader).unwrap(), None);
    }

    #[test]
    fn oversized_and_truncated_events_fail() {
        let oversized = u32::try_from(HOST_EVENT_BYTES_MAX + 1)
            .unwrap()
            .to_le_bytes();
        assert!(matches!(
            read_host_event(&mut oversized.as_slice()),
            Err(ReadHostEventError::TooLarge(_))
        ));

        let mut pipe = Vec::new();
        write_host_event(&mut pipe, &HostEvent::Ready).unwrap();
        pipe.extend_from_slice(&5_u32.to_le_bytes());
        pipe.push(0);
        let mut reader = pipe.as_slice();
        assert_eq!(
            read_host_event(&mut reader).unwrap(),
            Some(HostEvent::Ready)
        );
        assert!(matches!(
            read_host_event(&mut reader),
            Err(ReadHostEventError::Io(_))
        ));
    }
}
