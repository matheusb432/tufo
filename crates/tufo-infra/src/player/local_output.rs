use std::{sync::Arc, time::Instant};

use tufo_application::playback::{
    recover_local_output::RecoverLocalOutputError, select_output::SelectOutputError,
};
use tufo_models::playback::{
    OutputTarget,
    local_output::{LocalOutputFailure, LocalOutputFailureReason, LocalOutputState},
};

use super::{AudioOutputs, Engine, EnginePlayer, OpenedOutput};
use crate::audio::MixerVolumes;

impl<O: AudioOutputs> EnginePlayer<O> {
    pub(super) fn poll_local_output(&self, engine: &mut Engine) -> bool {
        if engine.target != OutputTarget::Local {
            return false;
        }
        let failure = engine
            .output
            .as_mut()
            .and_then(|output| output.mixer.local_failure(Instant::now()));
        let Some(failure) = failure else {
            return false;
        };
        tracing::error!(?failure, "local audio output failed");
        let retired = engine.detach_output();
        engine.release_at = None;
        engine.local_output = LocalOutputState::Failed(failure);
        if let (Some(output), Some(player)) = (retired, self.this.upgrade()) {
            tokio::spawn(async move {
                player.outputs.close(output).await;
            });
        }
        true
    }

    pub(super) async fn start_local_recovery(&self) -> Result<(), RecoverLocalOutputError> {
        let mut engine = self.engine.lock().await;
        if engine.target != OutputTarget::Local {
            return Err(RecoverLocalOutputError::NotLocal);
        }
        let previous_failure = match &engine.local_output {
            LocalOutputState::Ready(_) | LocalOutputState::Recovering { .. } => return Ok(()),
            LocalOutputState::Failed(failure) => Some(failure.clone()),
            LocalOutputState::Released => None,
        };
        engine.output_generation += 1;
        engine.release_at = None;
        let generation = engine.output_generation;
        engine.local_output = LocalOutputState::Recovering { previous_failure };
        self.publish(&engine);
        if let Some(player) = self.this.upgrade() {
            tokio::spawn(player.reopen_local(generation));
        }
        Ok(())
    }

    async fn reopen_local(self: Arc<Self>, generation: u64) {
        let opened = self
            .outputs
            .open(OutputTarget::Local, MixerVolumes::SILENT)
            .await;
        let mut engine = self.engine.lock().await;
        if engine.output_generation != generation || engine.target != OutputTarget::Local {
            drop(engine);
            if let Ok(output) = opened {
                self.outputs.close(output).await;
            }
            return;
        }
        match opened {
            Ok(opened) => self.install_output(&mut engine, opened).await,
            Err(error) => engine.record_local_open_failure(&error),
        }
        self.publish(&engine);
    }
}

impl Engine {
    pub(super) fn detach_output(&mut self) -> Option<OpenedOutput> {
        let output = self.take_output()?;
        if let Some(host) = self.browser.host() {
            host.detach();
        }
        self.output_generation += 1;
        self.music_generation += 1;
        let mut retired_underruns = 0;
        if let Some(music) = &mut self.music {
            music.current.start_offset = music.current.position();
            retired_underruns += music.current.underrun_frames();
            music.current.handle = None;
            if let Some(armed) = music.armed.take() {
                retired_underruns += armed.underrun_frames();
            }
            music.changing = false;
            music.arm_requested = false;
        }
        self.retired_underrun_frames += retired_underruns;
        self.suspend_layers();
        self.sounds.retain(|_, sound| sound.cue.settings.looping);
        for sound in self.sounds.values_mut() {
            sound.playback = None;
        }
        Some(output)
    }

    pub(super) fn record_local_open_failure(&mut self, error: &SelectOutputError) {
        let mut failure = error.local_failure().unwrap_or_else(|| LocalOutputFailure {
            reason: LocalOutputFailureReason::OpenFailed,
            identity: None,
            detail: error.to_string(),
        });
        if failure.identity.is_none() {
            failure.identity = match &self.local_output {
                LocalOutputState::Recovering {
                    previous_failure: Some(previous),
                }
                | LocalOutputState::Failed(previous) => previous.identity.clone(),
                LocalOutputState::Ready(identity) => Some(identity.clone()),
                _ => None,
            };
        }
        tracing::error!(?failure, "opening the local audio output failed");
        self.local_output = LocalOutputState::Failed(failure);
    }
}
