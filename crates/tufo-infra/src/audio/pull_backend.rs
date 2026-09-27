//! A kira backend whose renderer is pulled by an external clock instead of a sound card.

use std::sync::mpsc::SyncSender;

use kira::backend::{Backend, Renderer};

pub struct PullBackendSettings {
    pub sample_rate: u32,
    /// Receives the renderer once the mixer starts; the puller owns it from then on.
    pub renderer_sender: SyncSender<Renderer>,
}

#[derive(Debug, thiserror::Error)]
pub enum PullBackendError {
    #[error("the mixer was already started")]
    AlreadyStarted,
    #[error("the renderer's puller is gone")]
    PullerGone,
}

pub struct PullBackend {
    renderer_sender: Option<SyncSender<Renderer>>,
}

impl Backend for PullBackend {
    type Settings = PullBackendSettings;
    type Error = PullBackendError;

    fn setup(
        settings: Self::Settings,
        _internal_buffer_size: usize,
    ) -> Result<(Self, u32), Self::Error> {
        Ok((
            Self {
                renderer_sender: Some(settings.renderer_sender),
            },
            settings.sample_rate,
        ))
    }

    fn start(&mut self, renderer: Renderer) -> Result<(), Self::Error> {
        self.renderer_sender
            .take()
            .ok_or(PullBackendError::AlreadyStarted)?
            .send(renderer)
            .map_err(|_| PullBackendError::PullerGone)
    }
}
