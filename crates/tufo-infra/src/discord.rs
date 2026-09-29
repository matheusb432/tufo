//! The music client: a Discord bot that joins one voice channel and streams a mixer into it.

pub mod bot_credentials;
mod credentials;
mod pacing_totals;
mod voice_channel_cache;
mod voice_source;

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock, PoisonError, RwLock, RwLockReadGuard,
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
    },
    time::Duration,
};

pub use credentials::{DiscordCredentials, LoadCredentialsError, SaveCredentialsError};
use kira::backend::Renderer;
use pacing_totals::PacingTotals;
use songbird::{
    CoreEvent, Event, EventContext, EventHandler, Songbird,
    input::{Input, RawAdapter},
    shards::TwilightMap,
    tracks::TrackHandle,
};
use tokio::sync::oneshot;
use tufo_application::discord::discord_client::{DiscordClient, DiscordClientError};
use tufo_models::{
    discord::{GuildId, VoiceChannel, VoiceChannelTarget},
    playback::diagnostics::VoicePacing,
};
use twilight_gateway::{EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::error::ErrorType;
use twilight_model::id::{Id, marker::UserMarker};
use voice_channel_cache::VoiceChannelCache;
use voice_source::{KiraVoiceSource, VoicePacingStats};

/// Discord voice carries 48 kHz stereo.
pub const DISCORD_SAMPLE_RATE: u32 = 48_000;
const DISCORD_CHANNELS: u32 = 2;
const JOIN_TIMEOUT: Duration = Duration::from_secs(15);
const PACING_REPORT_INTERVAL: Duration = Duration::from_secs(30);
const LOGIN_RETRY_DELAY_MIN: Duration = Duration::from_secs(2);
const LOGIN_RETRY_DELAY_MAX: Duration = Duration::from_secs(60);
const HTTP_UNAUTHORIZED: u16 = 401;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceAvailability {
    NotConfigured,
    Disabled,
    Enabled,
}

/// The server's Discord bot: its gateway connection and the voice channels it joins. Clones
/// share one connection.
#[derive(Clone)]
pub struct SongbirdDiscordClient {
    connection: Arc<RwLock<Connection>>,
    pacing_totals: Arc<PacingTotals>,
}

enum Connection {
    NotConfigured,
    Disabled(DiscordCredentials),
    Enabled(Gateway),
}

impl SongbirdDiscordClient {
    #[must_use]
    pub fn not_configured() -> Self {
        Self::with_connection(Connection::NotConfigured)
    }

    /// Starts logging the bot in and following its gateway in the background, so the server
    /// starts even while Discord is unreachable. Must run inside a tokio runtime.
    #[must_use]
    pub fn start(credentials: DiscordCredentials) -> Self {
        Self::with_connection(Connection::Enabled(Gateway::start(credentials)))
    }

    fn with_connection(connection: Connection) -> Self {
        Self {
            connection: Arc::new(RwLock::new(connection)),
            pacing_totals: Arc::default(),
        }
    }

    #[must_use]
    pub fn voice_availability(&self) -> VoiceAvailability {
        match &*self.read_connection() {
            Connection::NotConfigured => VoiceAvailability::NotConfigured,
            Connection::Disabled(_) => VoiceAvailability::Disabled,
            Connection::Enabled(_) => VoiceAvailability::Enabled,
        }
    }

    /// How late songbird ran its voice ticks, over every session since the server started;
    /// `None` when not configured.
    #[must_use]
    pub fn voice_pacing(&self) -> Option<VoicePacing> {
        match self.voice_availability() {
            VoiceAvailability::NotConfigured => None,
            VoiceAvailability::Disabled | VoiceAvailability::Enabled => {
                Some(self.pacing_totals.summary())
            }
        }
    }

    /// Joins `target` self-deafened and streams whatever the renderer from `renderer_receiver`
    /// produces. The session's [`VoiceSession::take_lost_signal`] reports when the connection
    /// drops without Tufo leaving.
    pub async fn join(
        &self,
        target: VoiceChannelTarget,
        renderer_receiver: Receiver<Renderer>,
    ) -> Result<VoiceSession, JoinVoiceError> {
        let songbird = self.songbird_seeing(target)?;
        let call = tokio::time::timeout(
            JOIN_TIMEOUT,
            songbird.join(target.guild_id.into_inner(), target.channel_id.into_inner()),
        )
        .await
        .map_err(|_| JoinVoiceError::TimedOut)?
        .map_err(JoinVoiceError::Join)?;

        let stats = Arc::new(VoicePacingStats::default());
        let leaving = Arc::new(AtomicBool::new(false));
        let (lost_sender, lost_receiver) = oneshot::channel();
        let track = {
            let mut call = call.lock().await;
            if let Err(error) = call.deafen(true).await {
                tracing::warn!(%error, "self-deafening in the voice channel failed");
            }
            call.remove_all_global_events();
            call.add_global_event(
                Event::Core(CoreEvent::DriverDisconnect),
                DisconnectWatcher {
                    leaving: Arc::clone(&leaving),
                    lost_sender: Mutex::new(Some(lost_sender)),
                },
            );
            let source = KiraVoiceSource::new(
                renderer_receiver,
                Arc::clone(&stats),
                Arc::clone(&self.pacing_totals),
            );
            call.play_input(Input::from(RawAdapter::new(
                source,
                DISCORD_SAMPLE_RATE,
                DISCORD_CHANNELS,
            )))
        };
        let reporter = tokio::spawn(report_pacing(stats));
        tracing::info!(?target, "joined the Discord voice channel");
        Ok(VoiceSession {
            songbird,
            guild_id: target.guild_id,
            track,
            reporter,
            leaving,
            lost: Some(lost_receiver),
        })
    }

    fn songbird_seeing(&self, target: VoiceChannelTarget) -> Result<Arc<Songbird>, JoinVoiceError> {
        let connection = self.read_connection();
        let gateway = match &*connection {
            Connection::NotConfigured => return Err(JoinVoiceError::NotConfigured),
            Connection::Disabled(_) => return Err(JoinVoiceError::Disabled),
            Connection::Enabled(gateway) => gateway,
        };
        let songbird = gateway.songbird.get().ok_or(JoinVoiceError::NotReady)?;
        let visible = gateway
            .channels
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(target);
        if !visible {
            return Err(JoinVoiceError::UnknownChannel);
        }
        Ok(Arc::clone(songbird))
    }

    fn read_connection(&self) -> RwLockReadGuard<'_, Connection> {
        self.connection
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl DiscordClient for SongbirdDiscordClient {
    fn voice_channels(&self) -> Result<Vec<VoiceChannel>, DiscordClientError> {
        match &*self.read_connection() {
            Connection::NotConfigured => Err(DiscordClientError::NotConfigured),
            Connection::Disabled(_) => Err(DiscordClientError::Disabled),
            Connection::Enabled(gateway) => gateway
                .channels
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .voice_channels()
                .ok_or(DiscordClientError::NotReady),
        }
    }

    fn disable_connection(&self) {
        let mut connection = self
            .connection
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if let Connection::Enabled(gateway) = &*connection {
            *connection = Connection::Disabled(gateway.credentials.clone());
            tracing::info!("disabled the Discord connection");
        }
    }

    fn enable_connection(&self) {
        let mut connection = self
            .connection
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if let Connection::Disabled(credentials) = &*connection {
            *connection = Connection::Enabled(Gateway::start(credentials.clone()));
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JoinVoiceError {
    #[error("Discord is not configured on this server")]
    NotConfigured,
    #[error("the Discord connection is disabled on this server")]
    Disabled,
    #[error("the Discord bot has not logged in yet")]
    NotReady,
    #[error("the bot cannot see that voice channel")]
    UnknownChannel,
    #[error("joining the voice channel timed out")]
    TimedOut,
    #[error("joining the voice channel failed")]
    Join(#[source] songbird::error::JoinError),
}

/// A gateway session following Discord in the background; dropping it logs the bot out.
struct Gateway {
    credentials: DiscordCredentials,
    songbird: Arc<OnceLock<Arc<Songbird>>>,
    channels: Arc<RwLock<VoiceChannelCache>>,
    task: tokio::task::JoinHandle<()>,
}

impl Gateway {
    fn start(credentials: DiscordCredentials) -> Self {
        let songbird = Arc::new(OnceLock::new());
        let channels = Arc::new(RwLock::new(VoiceChannelCache::default()));
        let task = tokio::spawn(run_gateway(
            credentials.clone(),
            Arc::clone(&songbird),
            Arc::clone(&channels),
        ));
        Self {
            credentials,
            songbird,
            channels,
            task,
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A joined voice channel receiving the mixer's output.
pub struct VoiceSession {
    songbird: Arc<Songbird>,
    guild_id: GuildId,
    track: TrackHandle,
    reporter: tokio::task::JoinHandle<()>,
    /// Set before Tufo leaves, so its own disconnect isn't reported as lost.
    leaving: Arc<AtomicBool>,
    lost: Option<oneshot::Receiver<String>>,
}

impl VoiceSession {
    /// Resolves with a reason when the connection drops without Tufo leaving. Available once.
    pub fn take_lost_signal(&mut self) -> Option<oneshot::Receiver<String>> {
        self.lost.take()
    }

    /// Stops streaming and leaves the voice channel.
    pub async fn leave(self) {
        self.leaving.store(true, Ordering::Relaxed);
        if let Err(error) = self.track.stop() {
            tracing::debug!(%error, "the voice track had already ended");
        }
        if let Err(error) = self.songbird.remove(self.guild_id.into_inner()).await {
            tracing::warn!(%error, "leaving the Discord voice channel failed");
        }
    }
}

impl Drop for VoiceSession {
    fn drop(&mut self) {
        self.reporter.abort();
    }
}

/// Reports the first disconnect of a call Tufo didn't leave.
struct DisconnectWatcher {
    leaving: Arc<AtomicBool>,
    lost_sender: Mutex<Option<oneshot::Sender<String>>>,
}

#[async_trait::async_trait]
impl EventHandler for DisconnectWatcher {
    async fn act(&self, context: &EventContext<'_>) -> Option<Event> {
        let EventContext::DriverDisconnect(disconnect) = context else {
            return None;
        };
        if self.leaving.load(Ordering::Relaxed) {
            return Some(Event::Cancel);
        }
        let reason = format!(
            "the voice connection closed ({:?}, {:?})",
            disconnect.kind, disconnect.reason
        );
        let sender = self
            .lost_sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(reason);
        }
        Some(Event::Cancel)
    }
}

async fn run_gateway(
    credentials: DiscordCredentials,
    songbird_slot: Arc<OnceLock<Arc<Songbird>>>,
    channels: Arc<RwLock<VoiceChannelCache>>,
) {
    let token = credentials.bot_token().to_owned();
    let Some(user_id) = log_in(&twilight_http::Client::new(token.clone())).await else {
        return;
    };
    let mut shard = Shard::new(
        ShardId::ONE,
        token,
        Intents::GUILDS | Intents::GUILD_VOICE_STATES,
    );
    let senders = TwilightMap::new(HashMap::from([(shard.id().number(), shard.sender())]));
    let songbird = Arc::new(Songbird::twilight(Arc::new(senders), user_id));
    let _ = songbird_slot.set(Arc::clone(&songbird));
    tracing::info!("the Discord bot logged in");

    while let Some(item) = shard.next_event(gateway_events()).await {
        match item {
            Ok(event) => {
                channels
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
                    .apply(&event);
                songbird.process(&event).await;
            }
            Err(error) => tracing::warn!(%error, "receiving a Discord gateway event failed"),
        }
    }
    tracing::error!(
        "the Discord gateway closed for good; disable and enable the connection to reconnect"
    );
}

fn gateway_events() -> EventTypeFlags {
    EventTypeFlags::READY
        | EventTypeFlags::GUILD_CREATE
        | EventTypeFlags::GUILD_UPDATE
        | EventTypeFlags::GUILD_DELETE
        | EventTypeFlags::CHANNEL_CREATE
        | EventTypeFlags::CHANNEL_UPDATE
        | EventTypeFlags::CHANNEL_DELETE
        | EventTypeFlags::VOICE_STATE_UPDATE
        | EventTypeFlags::VOICE_SERVER_UPDATE
}

/// Resolves the bot's user id, retrying failures with capped backoff until the server stops. A
/// rejected token ends the attempt.
async fn log_in(http: &twilight_http::Client) -> Option<Id<UserMarker>> {
    let mut delay = LOGIN_RETRY_DELAY_MIN;
    loop {
        let error = match http.current_user().await {
            Ok(response) => match response.model().await {
                Ok(user) => return Some(user.id),
                Err(error) => anyhow::Error::new(error),
            },
            Err(error) => {
                if let ErrorType::Response { status, .. } = error.kind()
                    && status.get() == HTTP_UNAUTHORIZED
                {
                    tracing::error!("Discord rejected the bot token");
                    return None;
                }
                anyhow::Error::new(error)
            }
        };
        tracing::warn!(error = format!("{error:#}"), retry_in = ?delay, "logging the Discord bot in failed");
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(LOGIN_RETRY_DELAY_MAX);
    }
}

async fn report_pacing(stats: Arc<VoicePacingStats>) {
    let mut interval = tokio::time::interval(PACING_REPORT_INTERVAL);
    interval.tick().await;
    let mut promotion_reported = false;
    loop {
        interval.tick().await;
        if !promotion_reported && let Some(outcome) = stats.realtime_promotion() {
            match outcome {
                Ok(()) => tracing::info!("songbird's mixer thread runs at real-time priority"),
                Err(error) => {
                    tracing::warn!(%error, "songbird's mixer thread runs without real-time priority");
                }
            }
            promotion_reported = true;
        }
        let window = stats.take_window();
        if window.very_late_ticks > 0 {
            tracing::warn!(?window, "songbird sent late voice packets in bursts");
        } else {
            tracing::info!(?window, "voice packet pacing");
        }
    }
}
