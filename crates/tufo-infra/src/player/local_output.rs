use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use tufo_application::playback::{
    recover_local_output::RecoverLocalOutputError, select_output::SelectOutputError,
};
use tufo_models::playback::{
    OutputTarget,
    local_output::{LocalOutputFailure, LocalOutputFailureReason, LocalOutputState},
};

use super::{AudioOutputs, Engine, EnginePlayer, OpenedOutput};
use crate::audio::MixerVolumes;

/// How many times the engine reopens an invalidated local output on its own within
/// [`LOCAL_REOPENS_WINDOW`]; past that, the output waits for the user's recovery.
const LOCAL_REOPENS_MAX: usize = 3;
const LOCAL_REOPENS_WINDOW: Duration = Duration::from_mins(1);

/// When the engine recently reopened the local output on its own.
#[derive(Debug, Default)]
pub(super) struct LocalReopens(VecDeque<Instant>);

impl LocalReopens {
    /// Records a reopen at `now`, unless the recent ones reached the cap.
    fn admit(&mut self, now: Instant) -> bool {
        while self
            .0
            .front()
            .is_some_and(|at| now.duration_since(*at) >= LOCAL_REOPENS_WINDOW)
        {
            self.0.pop_front();
        }
        if self.0.len() >= LOCAL_REOPENS_MAX {
            return false;
        }
        self.0.push_back(now);
        true
    }
}

impl<O: AudioOutputs> EnginePlayer<O> {
    /// Retires a failed local output. An invalidated one reopens on the current default device,
    /// which is how a default-device change arrives on Windows.
    pub(super) fn poll_local_output(&self, engine: &mut Engine) -> bool {
        if engine.target != OutputTarget::Local {
            return false;
        }
        let now = Instant::now();
        let failure = engine
            .output
            .as_mut()
            .and_then(|output| output.mixer.local_failure(now));
        let Some(failure) = failure else {
            return false;
        };
        tracing::error!(?failure, "local audio output failed");
        let retired = engine.detach_output();
        engine.release_at = None;
        let reopen = failure.reason.invalidates_stream() && engine.local_reopens.admit(now);
        engine.local_output = if reopen {
            tracing::info!("reopening the local audio output");
            LocalOutputState::Recovering {
                previous_failure: Some(failure),
            }
        } else {
            LocalOutputState::Failed(failure)
        };
        let reopen_generation = reopen.then_some(engine.output_generation);
        if let Some(player) = self.this.upgrade() {
            tokio::spawn(player.replace_failed_local(retired, reopen_generation));
        }
        true
    }

    /// Closes the failed output before any replacement opens, so the two never share a device.
    async fn replace_failed_local(
        self: Arc<Self>,
        retired: Option<OpenedOutput>,
        reopen_generation: Option<u64>,
    ) {
        if let Some(output) = retired {
            self.outputs.close(output).await;
        }
        if let Some(generation) = reopen_generation {
            self.reopen_local(generation).await;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopens_stop_at_the_cap_until_the_window_passes() {
        let mut reopens = LocalReopens::default();
        let start = Instant::now();
        for attempt in 0..LOCAL_REOPENS_MAX {
            assert!(reopens.admit(start + Duration::from_secs(attempt as u64)));
        }
        assert!(!reopens.admit(start + LOCAL_REOPENS_WINDOW / 2));
        assert!(reopens.admit(start + LOCAL_REOPENS_WINDOW));
        assert!(!reopens.admit(start + LOCAL_REOPENS_WINDOW));
    }
}
