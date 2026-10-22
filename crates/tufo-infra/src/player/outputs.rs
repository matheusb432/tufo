//! Where the engine's mixers come from: the sound card, a Discord voice channel, or, in tests,
//! a mixer the test pulls itself.

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::{Semaphore, oneshot};
use tufo_application::playback::select_output::SelectOutputError;
use tufo_models::playback::{
    OutputTarget,
    diagnostics::{LocalCallbackDiagnostics, VoicePacing},
    local_output::{LocalOutputFailureReason, LocalOutputIdentity},
};

use crate::{
    audio::{Mixer, MixerVolumes, callback_diagnostics::LocalCallbackCounters},
    discord::{
        DISCORD_SAMPLE_RATE, JoinVoiceError, SongbirdDiscordClient, VoiceAvailability, VoiceSession,
    },
};

/// Opens and closes the mixers the engine plays on.
pub trait AudioOutputs: Send + Sync + 'static {
    fn voice_availability(&self) -> VoiceAvailability;

    /// Opens a mixer for `target`, joining its voice channel when it is one.
    /// Tracks start at `volumes` from the first rendered frame.
    fn open(
        &self,
        target: OutputTarget,
        volumes: MixerVolumes,
    ) -> impl Future<Output = Result<OpenedOutput, SelectOutputError>> + Send;

    /// Closes an output the engine no longer uses, leaving its voice channel.
    fn close(&self, output: OpenedOutput) -> impl Future<Output = ()> + Send;

    /// Voice tick lateness since the server started, when voice is configured.
    fn voice_pacing(&self) -> Option<VoicePacing>;

    /// Local callback observations across stream replacements, absent for synthetic outputs.
    fn local_callbacks(&self) -> Option<LocalCallbackDiagnostics> {
        None
    }
}

/// A mixer bound to an output, with the signal that its voice connection dropped.
pub struct OpenedOutput {
    pub(crate) mixer: Mixer,
    voice_lost: Option<oneshot::Receiver<String>>,
    session: Option<VoiceSession>,
}

impl OpenedOutput {
    /// An output whose connection drops when `voice_lost` resolves; `None` never drops.
    #[must_use]
    pub fn new(mixer: Mixer, voice_lost: Option<oneshot::Receiver<String>>) -> Self {
        Self {
            mixer,
            voice_lost,
            session: None,
        }
    }

    pub(crate) fn take_voice_lost(&mut self) -> Option<oneshot::Receiver<String>> {
        self.voice_lost.take()
    }
}

/// The real outputs: the default audio device and the Discord bot's voice channels.
pub struct DeviceOutputs {
    discord: SongbirdDiscordClient,
    local_open: Arc<Semaphore>,
    local_callbacks: Arc<LocalCallbackCounters>,
}

impl DeviceOutputs {
    #[must_use]
    pub fn new(discord: SongbirdDiscordClient) -> Self {
        Self {
            discord,
            local_open: Arc::new(Semaphore::new(1)),
            local_callbacks: Arc::new(LocalCallbackCounters::default()),
        }
    }

    async fn open_local(&self, volumes: MixerVolumes) -> Result<OpenedOutput, SelectOutputError> {
        let identity = local_identity_hint();
        let permit = Arc::clone(&self.local_open)
            .try_acquire_owned()
            .map_err(|error| SelectOutputError::LocalDevice {
                reason: LocalOutputFailureReason::OpenTimedOut,
                identity: identity.clone(),
                source: error.into(),
            })?;
        let worker_identity = identity.clone();
        let callback_counters = Arc::clone(&self.local_callbacks);
        let worker = tokio::task::spawn_blocking(move || {
            // A native call can outlive its deadline. Keep admission closed until it returns.
            let _permit = permit;
            open_local_mixer(volumes, worker_identity, callback_counters)
        });
        match tokio::time::timeout(Duration::from_secs(6), worker).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(SelectOutputError::LocalDevice {
                reason: LocalOutputFailureReason::OpenFailed,
                identity,
                source: error.into(),
            }),
            Err(error) => Err(SelectOutputError::LocalDevice {
                reason: LocalOutputFailureReason::OpenTimedOut,
                identity,
                source: error.into(),
            }),
        }
    }
}

fn open_local_mixer(
    volumes: MixerVolumes,
    identity: Option<LocalOutputIdentity>,
    callback_counters: Arc<LocalCallbackCounters>,
) -> Result<OpenedOutput, SelectOutputError> {
    let mut mixer = Mixer::open_local_observed(volumes, callback_counters).map_err(|error| {
        SelectOutputError::LocalDevice {
            reason: error.reason(),
            identity: error.identity().cloned().or(identity),
            source: error.into(),
        }
    })?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !mixer.local_has_callback() {
        if Instant::now() >= deadline {
            return Err(SelectOutputError::LocalDevice {
                reason: LocalOutputFailureReason::CallbackStopped,
                identity: Some(mixer.local_identity()),
                source: anyhow::anyhow!("the opened local stream delivered no audio callbacks"),
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Some(failure) = mixer.local_failure(Instant::now()) {
        return Err(SelectOutputError::LocalDevice {
            reason: failure.reason,
            identity: failure.identity,
            source: anyhow::anyhow!(failure.detail),
        });
    }
    tracing::info!(identity = ?mixer.local_identity(), "opened the local audio device");
    Ok(OpenedOutput::new(mixer, None))
}

fn local_identity_hint() -> Option<LocalOutputIdentity> {
    #[cfg(target_os = "linux")]
    if let Ok(device) = std::env::var("PIPEWIRE_NODE") {
        return Some(LocalOutputIdentity {
            backend: "PipeWire".to_owned(),
            device: Some(device),
            sample_rate: None,
        });
    }
    None
}

impl AudioOutputs for DeviceOutputs {
    fn voice_availability(&self) -> VoiceAvailability {
        self.discord.voice_availability()
    }

    async fn open(
        &self,
        target: OutputTarget,
        volumes: MixerVolumes,
    ) -> Result<OpenedOutput, SelectOutputError> {
        match target {
            OutputTarget::Local => self.open_local(volumes).await,
            OutputTarget::DiscordVoice(voice_target) => {
                let (mixer, renderer_receiver) =
                    Mixer::open_pulled(DISCORD_SAMPLE_RATE, volumes)
                        .map_err(|error| SelectOutputError::VoiceConnection(error.into()))?;
                let mut session = self
                    .discord
                    .join(voice_target, renderer_receiver)
                    .await
                    .map_err(|error| match error {
                        JoinVoiceError::NotConfigured => SelectOutputError::DiscordNotConfigured,
                        JoinVoiceError::Disabled => SelectOutputError::DiscordDisabled,
                        JoinVoiceError::UnknownChannel => SelectOutputError::UnknownVoiceChannel,
                        JoinVoiceError::NotReady
                        | JoinVoiceError::TimedOut
                        | JoinVoiceError::Join(_)
                        | JoinVoiceError::VoiceInput(_) => {
                            SelectOutputError::VoiceConnection(error.into())
                        }
                    })?;
                Ok(OpenedOutput {
                    mixer,
                    voice_lost: session.take_lost_signal(),
                    session: Some(session),
                })
            }
        }
    }

    async fn close(&self, output: OpenedOutput) {
        let OpenedOutput { mixer, session, .. } = output;
        if let Some(session) = session {
            session.leave().await;
        }
        if let Err(error) = tokio::task::spawn_blocking(move || drop(mixer)).await {
            tracing::warn!(%error, "closing the audio mixer failed");
        }
    }

    fn voice_pacing(&self) -> Option<VoicePacing> {
        self.discord.voice_pacing()
    }

    fn local_callbacks(&self) -> Option<LocalCallbackDiagnostics> {
        Some(self.local_callbacks.snapshot())
    }
}
