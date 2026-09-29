use std::{sync::Arc, time::Duration};

use tufo_application::library::Library;
use tufo_client::TufoClient;
use tufo_infra::{
    database::open_database, discord::SongbirdDiscordClient, library_files::DiskLibraryFiles,
    player::EnginePlayer,
};
use tufo_local_transport::{LocalEndpoint, LocalListener};
use tufo_server::AppState;

pub const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// A real server on a private socket in a temporary directory, without Discord credentials.
pub struct TestServer {
    pub endpoint: LocalEndpoint,
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
    directory: tempfile::TempDir,
}

impl TestServer {
    pub async fn start() -> anyhow::Result<Self> {
        let directory = tempfile::tempdir()?;
        let endpoint = LocalEndpoint::from_root(directory.path().join("runtime"))?;
        let listener = LocalListener::bind(&endpoint, TEST_TIMEOUT).await?;
        let database = open_database(&directory.path().join("tufo.sqlite3")).await?;
        let library = Arc::new(Library::open(database, DiskLibraryFiles).await?);
        let discord = SongbirdDiscordClient::not_configured();
        let state = AppState::new(
            EnginePlayer::start(discord.clone()),
            discord,
            library,
            directory.path().join("discord.toml"),
        );
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(tufo_server::serve(
            listener,
            async {
                let _ = stopped.await;
            },
            TEST_TIMEOUT,
            state,
        ));
        Ok(Self {
            endpoint,
            shutdown,
            server,
            directory,
        })
    }

    #[allow(
        dead_code,
        reason = "each test binary compiles this harness but uses a subset"
    )]
    pub async fn channel(&self) -> anyhow::Result<tonic::transport::Channel> {
        Ok(self.endpoint.connect(TEST_TIMEOUT, TEST_TIMEOUT).await?)
    }

    #[allow(
        dead_code,
        reason = "each test binary compiles this harness but uses a subset"
    )]
    pub fn client(&self) -> anyhow::Result<TufoClient> {
        Ok(TufoClient::connect_lazy(&self.endpoint)?)
    }

    #[allow(
        dead_code,
        reason = "each test binary compiles this harness but uses a subset"
    )]
    pub fn discord_credentials_path(&self) -> std::path::PathBuf {
        self.directory.path().join("discord.toml")
    }

    /// Waits for a server that was asked to stop to finish draining.
    #[allow(
        dead_code,
        reason = "each test binary compiles this harness but uses a subset"
    )]
    pub async fn stopped(self) -> anyhow::Result<()> {
        tokio::time::timeout(TEST_TIMEOUT, self.server).await???;
        Ok(())
    }

    pub async fn stop(self) -> anyhow::Result<()> {
        let _ = self.shutdown.send(());
        tokio::time::timeout(TEST_TIMEOUT, self.server).await???;
        Ok(())
    }
}

/// Writes one second of a quiet mono 8 kHz tone as a 16-bit PCM WAV file.
#[allow(
    dead_code,
    reason = "each test binary compiles this harness but uses a subset"
)]
pub fn write_wav(path: &std::path::Path, frequency: f64) -> std::io::Result<()> {
    const SAMPLE_RATE: u32 = 8_000;
    let mut samples = Vec::new();
    for index in 0..SAMPLE_RATE {
        let phase = std::f64::consts::TAU * frequency * f64::from(index) / f64::from(SAMPLE_RATE);
        #[expect(clippy::cast_possible_truncation, reason = "the tone stays within i16")]
        samples.extend_from_slice(&((phase.sin() * 4_000.0) as i16).to_le_bytes());
    }
    let data_bytes = SAMPLE_RATE * 2;
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    for field in [16_u32.to_le_bytes().to_vec(), 1_u16.to_le_bytes().to_vec()] {
        wav.extend_from_slice(&field);
    }
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    wav.extend_from_slice(&samples);
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    std::fs::write(path, wav)
}
