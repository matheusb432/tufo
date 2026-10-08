use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use kira::backend::Renderer;
use tufo_application::{library::Library, playback::select_output::SelectOutputError};
use tufo_infra::{
    audio::Mixer,
    database::open_database,
    discord::{SongbirdDiscordClient, VoiceAvailability},
    library_files::DiskLibraryFiles,
    player::{
        EnginePlayer, EngineTimings,
        outputs::{AudioOutputs, OpenedOutput},
    },
};
use tufo_local_transport::{LocalEndpoint, LocalListener};
use tufo_models::{
    library::Volume,
    playback::{OutputTarget, diagnostics::VoicePacing},
};

use super::support::TEST_TIMEOUT;

const SAMPLE_RATE: u32 = 8_000;
const RENDER_PERIOD: Duration = Duration::from_millis(10);

pub(super) struct PulledServer {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    renderer: PacedRenderer,
}

impl PulledServer {
    pub(super) async fn start(
        runtime: &Path,
        database: &Path,
        credentials: PathBuf,
    ) -> anyhow::Result<Self> {
        let endpoint = LocalEndpoint::from_root(runtime)?;
        let listener = LocalListener::bind(&endpoint, TEST_TIMEOUT).await?;
        let library =
            Arc::new(Library::open(open_database(database).await?, DiskLibraryFiles).await?);
        let (renderers, receiver) = mpsc::sync_channel(8);
        let renderer = PacedRenderer::start(receiver);
        let player = EnginePlayer::with_outputs(
            PulledOutputs { renderers },
            EngineTimings::default(),
            library.saved_song_loops(),
            tufo_infra::browser::UnavailableBrowser,
        );
        let state = tufo_server::AppState::new(
            player,
            SongbirdDiscordClient::not_configured(),
            library,
            credentials,
        );
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(tufo_server::serve(
            listener,
            async {
                let _ = stopped.await;
            },
            TEST_TIMEOUT,
            state,
        ));
        Ok(Self {
            shutdown: Some(shutdown),
            task,
            renderer,
        })
    }

    pub(super) async fn stop(mut self) -> anyhow::Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        tokio::time::timeout(TEST_TIMEOUT, &mut self.task).await???;
        self.renderer.stop()?;
        Ok(())
    }
}

impl Drop for PulledServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

struct PulledOutputs {
    renderers: mpsc::SyncSender<Renderer>,
}

impl PulledOutputs {
    fn open_local(&self, effects_volume: Volume) -> Result<OpenedOutput, SelectOutputError> {
        let (mixer, receiver) = Mixer::open_pulled(SAMPLE_RATE, effects_volume)
            .map_err(|error| SelectOutputError::LocalDevice(error.into()))?;
        let renderer = receiver
            .recv_timeout(Duration::from_secs(1))
            .map_err(|error| SelectOutputError::LocalDevice(error.into()))?;
        self.renderers
            .try_send(renderer)
            .map_err(|error| SelectOutputError::LocalDevice(anyhow::anyhow!(error.to_string())))?;
        Ok(OpenedOutput::new(mixer, None))
    }
}

impl AudioOutputs for PulledOutputs {
    fn voice_availability(&self) -> VoiceAvailability {
        VoiceAvailability::NotConfigured
    }

    fn open(
        &self,
        target: OutputTarget,
        effects_volume: Volume,
    ) -> impl std::future::Future<Output = Result<OpenedOutput, SelectOutputError>> {
        let output = match target {
            OutputTarget::Local => self.open_local(effects_volume),
            OutputTarget::DiscordVoice(_) => Err(SelectOutputError::DiscordNotConfigured),
        };
        std::future::ready(output)
    }

    fn close(&self, output: OpenedOutput) -> impl std::future::Future<Output = ()> {
        drop(output);
        std::future::ready(())
    }

    fn voice_pacing(&self) -> Option<VoicePacing> {
        None
    }
}

struct PacedRenderer {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<anyhow::Result<()>>>,
}

impl PacedRenderer {
    fn start(receiver: mpsc::Receiver<Renderer>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let renderer_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || render_audio(&receiver, &renderer_stop));
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("test renderer panicked"))??;
        }
        Ok(())
    }
}

impl Drop for PacedRenderer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn render_audio(receiver: &mpsc::Receiver<Renderer>, stop: &AtomicBool) -> anyhow::Result<()> {
    let mut renderers = Vec::new();
    let mut samples = [0.0_f32; 160];
    while !stop.load(Ordering::Relaxed) {
        for renderer in receiver.try_iter() {
            anyhow::ensure!(renderers.len() < 16, "too many test outputs opened");
            renderers.push(renderer);
        }
        for renderer in &mut renderers {
            renderer.on_start_processing();
            renderer.process(&mut samples, 2);
        }
        thread::sleep(RENDER_PERIOD);
    }
    Ok(())
}
