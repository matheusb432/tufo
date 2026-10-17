//! A local stream this crate owns: recovery must retain its selected host and target.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use cpal::{
    Device, Stream, StreamConfig,
    traits::{DeviceTrait as _, StreamTrait as _},
};
use kira::backend::{Backend, Renderer};
use rtrb::{Consumer, RingBuffer};
use tufo_models::playback::local_output::{
    LocalOutputFailure, LocalOutputFailureReason, LocalOutputIdentity,
};

use super::LocalStreamErrors;

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(2);
const STREAM_ERRORS_MAX: usize = 64;

pub(super) struct LocalBackendSettings {
    pub(super) device: Device,
    pub(super) config: StreamConfig,
    pub(super) identity: LocalOutputIdentity,
}

pub(super) struct LocalBackend {
    settings: Option<LocalBackendSettings>,
    stream: Option<Stream>,
    errors: Option<Consumer<(cpal::Error, u64)>>,
    discarded: Arc<AtomicU64>,
    callbacks: Arc<AtomicU64>,
    callbacks_seen: u64,
    callback_at: Instant,
    polled_at: Option<Instant>,
    identity: LocalOutputIdentity,
    pending_failure: Option<(LocalOutputFailureReason, String, u64)>,
    totals: LocalStreamErrors,
}

impl Backend for LocalBackend {
    type Settings = LocalBackendSettings;
    type Error = cpal::Error;

    fn setup(settings: Self::Settings, _: usize) -> Result<(Self, u32), Self::Error> {
        let rate = settings.config.sample_rate;
        let identity = settings.identity.clone();
        Ok((
            Self {
                settings: Some(settings),
                stream: None,
                errors: None,
                discarded: Arc::default(),
                callbacks: Arc::default(),
                callbacks_seen: 0,
                callback_at: Instant::now(),
                polled_at: None,
                identity,
                pending_failure: None,
                totals: LocalStreamErrors::default(),
            },
            rate,
        ))
    }

    fn start(&mut self, mut renderer: Renderer) -> Result<(), Self::Error> {
        let Some(settings) = self.settings.take() else {
            return Ok(());
        };
        let (mut errors, receiver) = RingBuffer::new(STREAM_ERRORS_MAX);
        self.errors = Some(receiver);
        let discarded = Arc::clone(&self.discarded);
        let callbacks = Arc::clone(&self.callbacks);
        let error_callbacks = Arc::clone(&self.callbacks);
        let config = settings.config;
        let stream = settings.device.build_output_stream(
            config,
            move |data: &mut [f32], _| {
                renderer.on_start_processing();
                renderer.process(data, config.channels);
                callbacks.fetch_add(1, Ordering::Relaxed);
            },
            move |error| {
                if errors
                    .push((error, error_callbacks.load(Ordering::Relaxed)))
                    .is_err()
                {
                    discarded.fetch_add(1, Ordering::Relaxed);
                }
            },
            Some(CALLBACK_TIMEOUT),
        )?;
        stream.play()?;
        self.stream = Some(stream);
        Ok(())
    }
}

impl LocalBackend {
    pub(super) fn identity(&self) -> &LocalOutputIdentity {
        &self.identity
    }

    pub(super) fn has_callback(&self) -> bool {
        self.callbacks.load(Ordering::Relaxed) != 0
    }

    pub(super) fn poll(&mut self) -> LocalStreamErrors {
        self.totals.discarded = self.discarded.load(Ordering::Relaxed);
        let Some(errors) = &mut self.errors else {
            return self.totals;
        };
        for _ in 0..STREAM_ERRORS_MAX {
            let Ok((error, callbacks_at_error)) = errors.pop() else {
                break;
            };
            self.totals.reported += 1;
            tracing::warn!(%error, kind = ?error.kind(), identity = ?self.identity, "local audio stream error");
            let reason = match error.kind() {
                cpal::ErrorKind::DeviceNotAvailable => {
                    Some(LocalOutputFailureReason::DeviceUnavailable)
                }
                cpal::ErrorKind::StreamInvalidated => {
                    Some(LocalOutputFailureReason::StreamInvalidated)
                }
                cpal::ErrorKind::DeviceChanged => Some(LocalOutputFailureReason::DeviceChanged),
                _ => None,
            };
            if let Some(reason) = reason
                && !self
                    .pending_failure
                    .as_ref()
                    .is_some_and(|(reason, _, _)| reason.invalidates_stream())
            {
                self.pending_failure = Some((reason, error.to_string(), callbacks_at_error));
            }
        }
        self.totals
    }

    pub(super) fn failure(&mut self, now: Instant) -> Option<LocalOutputFailure> {
        self.poll();
        // A poller that was suspended itself, as across sleep on Windows, where `Instant` keeps
        // counting, can't tell whether callbacks stopped meanwhile.
        if self
            .polled_at
            .replace(now)
            .is_some_and(|polled_at| now.duration_since(polled_at) >= CALLBACK_TIMEOUT)
        {
            self.callback_at = now;
        }
        if let Some((reason, detail, _)) = &self.pending_failure
            && reason.invalidates_stream()
        {
            return Some(LocalOutputFailure {
                reason: *reason,
                identity: Some(self.identity.clone()),
                detail: detail.clone(),
            });
        }
        let callbacks = self.callbacks.load(Ordering::Relaxed);
        if callbacks != self.callbacks_seen {
            self.callbacks_seen = callbacks;
            self.callback_at = now;
            if self
                .pending_failure
                .as_ref()
                .is_some_and(|(_, _, at_error)| callbacks > *at_error)
            {
                self.pending_failure = None;
            }
        }
        if now.duration_since(self.callback_at) < CALLBACK_TIMEOUT {
            return None;
        }
        let (reason, detail, _) = self.pending_failure.clone().unwrap_or((
            LocalOutputFailureReason::CallbackStopped,
            "the local stream stopped delivering audio callbacks".to_owned(),
            0,
        ));
        Some(LocalOutputFailure {
            reason,
            identity: Some(self.identity.clone()),
            detail,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> LocalBackend {
        LocalBackend {
            settings: None,
            stream: None,
            errors: None,
            discarded: Arc::default(),
            callbacks: Arc::default(),
            callbacks_seen: 0,
            callback_at: Instant::now(),
            polled_at: None,
            identity: LocalOutputIdentity {
                backend: "test".to_owned(),
                device: Some("sink".to_owned()),
                sample_rate: Some(48_000),
            },
            pending_failure: None,
            totals: LocalStreamErrors::default(),
        }
    }

    #[test]
    fn a_silent_transport_loss_is_detected_without_an_error_callback() {
        let mut backend = backend();
        let now = backend.callback_at;
        backend.callbacks.store(1, Ordering::Relaxed);
        assert!(backend.failure(now).is_none());
        assert!(backend.failure(now + CALLBACK_TIMEOUT / 2).is_none());
        let failure = backend.failure(now + CALLBACK_TIMEOUT).unwrap();
        assert_eq!(failure.reason, LocalOutputFailureReason::CallbackStopped);
        assert_eq!(failure.identity, Some(backend.identity.clone()));
        assert_eq!(backend.poll().reported, 0);
    }

    #[test]
    fn a_poller_suspended_past_the_callback_timeout_waits_another_timeout_before_reporting() {
        let mut backend = backend();
        let now = backend.callback_at;
        backend.callbacks.store(1, Ordering::Relaxed);
        assert!(backend.failure(now).is_none());
        assert!(backend.failure(now + CALLBACK_TIMEOUT * 30).is_none());
        let resumed = now + CALLBACK_TIMEOUT * 30;
        assert!(backend.failure(resumed + CALLBACK_TIMEOUT / 2).is_none());
        let failure = backend.failure(resumed + CALLBACK_TIMEOUT).unwrap();
        assert_eq!(failure.reason, LocalOutputFailureReason::CallbackStopped);
    }

    #[test]
    fn a_device_notification_with_continuing_callbacks_is_only_historical() {
        let mut backend = backend();
        let now = backend.callback_at;
        let (mut producer, consumer) = RingBuffer::new(2);
        backend.errors = Some(consumer);
        producer
            .push((
                cpal::Error::with_message(cpal::ErrorKind::DeviceChanged, "changed"),
                0,
            ))
            .unwrap();
        backend.callbacks.store(1, Ordering::Relaxed);
        assert!(backend.failure(now + CALLBACK_TIMEOUT).is_none());
        assert_eq!(backend.poll().reported, 1);
        assert!(backend.pending_failure.is_none());
        backend.callbacks.store(2, Ordering::Relaxed);
        assert!(backend.failure(now + CALLBACK_TIMEOUT * 2).is_none());
        assert_eq!(backend.poll().reported, 1);
    }

    #[test]
    fn a_failed_device_change_keeps_its_reason_and_cumulative_errors() {
        let mut backend = backend();
        let now = backend.callback_at;
        let (mut producer, consumer) = RingBuffer::new(2);
        backend.errors = Some(consumer);
        producer
            .push((
                cpal::Error::with_message(cpal::ErrorKind::DeviceChanged, "route lost"),
                0,
            ))
            .unwrap();
        backend.discarded.store(3, Ordering::Relaxed);
        assert!(backend.failure(now).is_none());
        assert!(backend.failure(now + CALLBACK_TIMEOUT / 2).is_none());
        let failure = backend.failure(now + CALLBACK_TIMEOUT).unwrap();
        assert_eq!(failure.reason, LocalOutputFailureReason::DeviceChanged);
        assert_eq!(
            backend.failure(now + CALLBACK_TIMEOUT * 3 / 2),
            Some(failure)
        );
        assert_eq!(backend.poll().reported, 1);
        assert_eq!(backend.poll().discarded, 3);
    }

    #[test]
    fn an_invalidated_stream_is_not_healthy_because_its_old_callbacks_continue() {
        for reason in [
            cpal::ErrorKind::DeviceNotAvailable,
            cpal::ErrorKind::StreamInvalidated,
        ] {
            let mut backend = backend();
            let (mut producer, consumer) = RingBuffer::new(2);
            backend.errors = Some(consumer);
            producer
                .push((cpal::Error::with_message(reason, "invalidated"), 1))
                .unwrap();
            producer
                .push((
                    cpal::Error::with_message(cpal::ErrorKind::DeviceChanged, "changed"),
                    2,
                ))
                .unwrap();
            backend.callbacks.store(3, Ordering::Relaxed);
            let failure = backend.failure(backend.callback_at).unwrap();
            assert!(failure.reason.invalidates_stream());
            assert_eq!(
                failure.detail,
                cpal::Error::with_message(reason, "invalidated").to_string()
            );
            assert_eq!(backend.poll().reported, 2);
        }
    }
}
