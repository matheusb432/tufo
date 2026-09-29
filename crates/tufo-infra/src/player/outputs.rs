//! Where the engine's mixers come from: the sound card, a Discord voice channel, or, in tests,
//! a mixer the test pulls itself.

use std::future::Future;

use tokio::sync::oneshot;
use tufo_application::playback::select_output::SelectOutputError;
use tufo_models::playback::{OutputTarget, diagnostics::VoicePacing};

use crate::{
    audio::Mixer,
    discord::{
        DISCORD_SAMPLE_RATE, JoinVoiceError, SongbirdDiscordClient, VoiceAvailability, VoiceSession,
    },
};

/// Opens and closes the mixers the engine plays on.
pub trait AudioOutputs: Send + Sync + 'static {
    fn voice_availability(&self) -> VoiceAvailability;

    /// Opens a mixer for `target`, joining its voice channel when it is one.
    fn open(
        &self,
        target: OutputTarget,
    ) -> impl Future<Output = Result<OpenedOutput, SelectOutputError>> + Send;

    /// Closes an output the engine no longer uses, leaving its voice channel.
    fn close(&self, output: OpenedOutput) -> impl Future<Output = ()> + Send;

    /// Voice tick lateness since the server started, when voice is configured.
    fn voice_pacing(&self) -> Option<VoicePacing>;
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
}

impl DeviceOutputs {
    #[must_use]
    pub fn new(discord: SongbirdDiscordClient) -> Self {
        Self { discord }
    }
}

impl AudioOutputs for DeviceOutputs {
    fn voice_availability(&self) -> VoiceAvailability {
        self.discord.voice_availability()
    }

    async fn open(&self, target: OutputTarget) -> Result<OpenedOutput, SelectOutputError> {
        match target {
            OutputTarget::Local => {
                let mixer = tokio::task::spawn_blocking(Mixer::open_local)
                    .await
                    .map_err(|error| SelectOutputError::LocalDevice(error.into()))?
                    .map_err(|error| SelectOutputError::LocalDevice(error.into()))?;
                tracing::info!(
                    sample_rate = mixer.sample_rate(),
                    "opened the local audio device"
                );
                Ok(OpenedOutput::new(mixer, None))
            }
            OutputTarget::DiscordVoice(voice_target) => {
                let (mixer, renderer_receiver) = Mixer::open_pulled(DISCORD_SAMPLE_RATE)
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
                        | JoinVoiceError::Join(_) => {
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
        if let Some(session) = output.session {
            session.leave().await;
        }
    }

    fn voice_pacing(&self) -> Option<VoicePacing> {
        self.discord.voice_pacing()
    }
}
