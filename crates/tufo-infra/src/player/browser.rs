//! The browser inside the engine: starting and ending its host, playing its pages on the current
//! output, and releasing it once nothing plays or shows it.
//!
//! Opens and closes run one at a time behind their own lock. A closed host may take its grace
//! period to exit; the next open waits for that, so two hosts never share the profile or the
//! endpoint.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::{mpsc::UnboundedReceiver, oneshot};
use tufo_application::playback::{control_browser::BrowserCommand, open_browser::OpenBrowserError};
use tufo_models::{
    library::Volume,
    playback::browser::{BrowserPlayback, BrowserStatus, BrowserStopReason},
};

use super::{Engine, EnginePlayer, outputs::AudioOutputs, sounds::volume_gain};
use crate::browser::{BrowserHost, BrowserNotice, BrowserNoticeKind, LaunchBrowserError};

/// How the engine paces the browser's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserTimings {
    /// How long the browser stays loaded with no page playing and no view showing it.
    pub idle_unload_delay: Duration,
    /// How long a starting host may take to accept views.
    pub ready_timeout: Duration,
    /// How long a closing host may take to save its tabs before it is killed.
    pub exit_grace: Duration,
}

impl Default for BrowserTimings {
    fn default() -> Self {
        Self {
            idle_unload_delay: Duration::from_mins(10),
            ready_timeout: Duration::from_secs(30),
            exit_grace: Duration::from_secs(5),
        }
    }
}

/// The browser's part of the engine's state.
#[derive(Default)]
pub(super) struct BrowserSession {
    status: BrowserStatus,
    volume: Volume,
    muted: bool,
    audible: bool,
    view_attached: bool,
    host: Option<BrowserHost>,
    last_launch: u64,
    started_at: Option<Instant>,
    /// Since when no page played and no view showed the browser.
    idle_since: Option<Instant>,
    /// Hosts asked to exit that may still be running.
    exiting: Vec<oneshot::Receiver<()>>,
}

impl BrowserSession {
    /// A session for an installation without a browser reports it unavailable from the start.
    pub(super) fn new(available: bool) -> Self {
        let status = if available {
            BrowserStatus::Closed
        } else {
            BrowserStatus::Stopped(BrowserStopReason::Unavailable)
        };
        Self {
            status,
            ..Self::default()
        }
    }

    pub(super) fn playback(&self) -> BrowserPlayback {
        BrowserPlayback {
            status: self.status,
            volume: self.volume,
            muted: self.muted,
            audible: self.audible,
        }
    }

    pub(super) fn gain(&self) -> f32 {
        volume_gain(self.volume, self.muted)
    }

    pub(super) fn is_audible(&self) -> bool {
        self.audible
    }

    pub(super) fn host(&self) -> Option<&BrowserHost> {
        self.host.as_ref()
    }

    /// Asks the host to exit and reports `status` at once; the host finishes exiting on its own.
    pub(super) fn retire(&mut self, status: BrowserStatus) {
        if let Some(host) = self.host.take() {
            self.exiting.push(host.close());
        }
        // Closing a browser this installation lacks leaves it unavailable.
        if self.status != BrowserStatus::Stopped(BrowserStopReason::Unavailable) {
            self.status = status;
        }
        self.audible = false;
        self.view_attached = false;
        self.started_at = None;
        self.idle_since = None;
    }

    fn stop_after_unrequested_exit(&mut self) {
        let reason = if self.status == BrowserStatus::Starting {
            BrowserStopReason::StartFailed
        } else {
            BrowserStopReason::Crashed
        };
        tracing::warn!(?reason, "the browser exited on its own");
        self.retire(BrowserStatus::Stopped(reason));
    }

    fn is_running(&self) -> bool {
        matches!(self.status, BrowserStatus::Starting | BrowserStatus::Open)
    }
}

impl<O: AudioOutputs> EnginePlayer<O> {
    pub(super) async fn open_browser_host(&self) -> Result<(), OpenBrowserError> {
        let _lifecycle = self.browser_lifecycle.lock().await;
        let exiting = {
            let mut engine = self.engine.lock().await;
            if engine.browser.is_running() {
                return Ok(());
            }
            std::mem::take(&mut engine.browser.exiting)
        };
        for exited in exiting {
            let _ = exited.await;
        }
        let browser_launcher = Arc::clone(&self.browser_launcher);
        let launch_result = tokio::task::spawn_blocking(move || browser_launcher.launch())
            .await
            .map_err(|error| OpenBrowserError::StartFailed(error.into()))?;
        let mut engine = self.engine.lock().await;
        let launch_id = engine.browser.last_launch + 1;
        engine.browser.last_launch = launch_id;
        let served = match launch_result {
            Ok(launched) => BrowserHost::serve(
                launched,
                launch_id,
                &self.browser_notices,
                Arc::clone(&self.browser_counters),
                self.timings.browser.exit_grace,
            )
            .map_err(|error| OpenBrowserError::StartFailed(error.into())),
            Err(LaunchBrowserError::Unavailable) => Err(OpenBrowserError::Unavailable),
            Err(LaunchBrowserError::Spawn(error)) => {
                Err(OpenBrowserError::StartFailed(error.into()))
            }
        };
        let host = match served {
            Ok(host) => host,
            Err(error) => {
                let reason = match error {
                    OpenBrowserError::Unavailable => BrowserStopReason::Unavailable,
                    OpenBrowserError::StartFailed(_) => BrowserStopReason::StartFailed,
                };
                tracing::warn!(%error, "the browser did not start");
                engine.browser.status = BrowserStatus::Stopped(reason);
                self.publish(&engine);
                return Err(error);
            }
        };
        let browser = &mut engine.browser;
        browser.host = Some(host);
        browser.status = BrowserStatus::Starting;
        browser.started_at = Some(Instant::now());
        browser.idle_since = None;
        self.attach_browser(&mut engine);
        self.publish(&engine);
        Ok(())
    }

    /// Closes the browser and waits until every process it started is gone.
    pub(super) async fn close_browser_host(&self) {
        let _lifecycle = self.browser_lifecycle.lock().await;
        let exiting = {
            let mut engine = self.engine.lock().await;
            engine.browser.retire(BrowserStatus::Closed);
            self.publish(&engine);
            std::mem::take(&mut engine.browser.exiting)
        };
        for exited in exiting {
            let _ = exited.await;
        }
    }

    pub(super) async fn control_browser_gain(&self, command: BrowserCommand) {
        let mut engine = self.engine.lock().await;
        match command {
            BrowserCommand::SetVolume(volume) => engine.browser.volume = volume,
            BrowserCommand::SetMuted(muted) => engine.browser.muted = muted,
        }
        let gain = engine.browser.gain();
        if let Some(output) = &mut engine.output {
            output.mixer.set_browser_volume(gain);
        }
        self.publish(&engine);
    }

    /// Gives the running host an input on the current mixer, at the browser's gain.
    pub(super) fn attach_browser(&self, engine: &mut Engine) {
        let gain = engine.browser.gain();
        let (Some(host), Some(output)) = (&engine.browser.host, &mut engine.output) else {
            return;
        };
        output.mixer.set_browser_volume(gain);
        match output
            .mixer
            .attach_browser_input(Arc::clone(&self.browser_counters))
        {
            Ok(input) => host.attach(input),
            Err(error) => tracing::warn!(%error, "the browser cannot play on this output"),
        }
    }

    async fn apply_browser_notice(&self, notice: BrowserNotice) {
        let mut engine = self.engine.lock().await;
        if notice.launch != engine.browser.last_launch || engine.browser.host.is_none() {
            return;
        }
        match notice.kind {
            BrowserNoticeKind::Ready => {
                if engine.browser.status == BrowserStatus::Starting {
                    engine.browser.status = BrowserStatus::Open;
                    engine.browser.started_at = None;
                    tracing::info!("the browser is open");
                }
            }
            BrowserNoticeKind::ViewAttached(attached) => engine.browser.view_attached = attached,
            BrowserNoticeKind::Audible(audible) => {
                self.follow_audibility(&mut engine, audible).await;
            }
            BrowserNoticeKind::Exited { requested } => {
                engine.browser.host = None;
                if !requested {
                    engine.browser.stop_after_unrequested_exit();
                }
            }
        }
        self.publish(&engine);
    }

    /// Opens the local device for a page that starts playing after the device was released.
    async fn follow_audibility(&self, engine: &mut Engine, audible: bool) {
        engine.browser.audible = audible;
        if !audible {
            return;
        }
        if let Err(error) = self.ensure_output(engine).await {
            tracing::warn!(%error, "the browser's audio has no output");
        }
    }

    /// Unloads a browser nothing used for the idle delay, and gives up on one that never became
    /// ready. Reports whether the browser's state changed.
    pub(super) fn release_browser_if_due(&self, engine: &mut Engine) -> bool {
        let now = Instant::now();
        let timings = self.timings.browser;
        let browser = &mut engine.browser;
        if browser.status == BrowserStatus::Starting
            && browser
                .started_at
                .is_some_and(|started_at| now - started_at >= timings.ready_timeout)
        {
            tracing::warn!("the browser did not become ready in time");
            browser.retire(BrowserStatus::Stopped(BrowserStopReason::StartFailed));
            return true;
        }
        let idle =
            browser.status == BrowserStatus::Open && !browser.audible && !browser.view_attached;
        if !idle {
            browser.idle_since = None;
            return false;
        }
        let idle_since = *browser.idle_since.get_or_insert(now);
        if now - idle_since < timings.idle_unload_delay {
            return false;
        }
        tracing::info!("unloading the browser, which nothing used for a while");
        browser.retire(BrowserStatus::Stopped(BrowserStopReason::Idle));
        true
    }
}

pub(super) async fn run_browser_notices<O: AudioOutputs>(
    player: std::sync::Weak<EnginePlayer<O>>,
    mut notices: UnboundedReceiver<BrowserNotice>,
) {
    while let Some(notice) = notices.recv().await {
        let Some(player) = player.upgrade() else {
            return;
        };
        player.apply_browser_notice(notice).await;
    }
}
