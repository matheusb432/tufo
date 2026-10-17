//! The browser host: a separate executable the engine starts on demand, whose pages play into
//! the mixer. Chromium never loads into the server, and closing the host ends every process it
//! started.
//!
//! Two threads serve each host. One reads its pipe, routing audio packets into the mixer and
//! reporting readiness, views, and audibility. The other supervises the process: it asks the host
//! to exit, kills what remains after a grace period, and reports when the host is gone.

mod process;
mod router;

use std::{
    io::Read,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

pub use process::ProcessBrowserLauncher;
pub(crate) use router::RouterControl;
use router::{Audible, AudioRouter};
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use tufo_wire::browser_host::{HostEvent, read_host_event};

use crate::audio::browser_input::{BrowserAudioCounters, BrowserInputHandle};

/// How often the supervisor checks whether the host exited.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub trait BrowserLauncher: Send + Sync + 'static {
    /// Whether this installation has a host to launch.
    fn is_available(&self) -> bool {
        true
    }

    fn launch(&self) -> Result<LaunchedBrowser, LaunchBrowserError>;
}

/// A launcher for installations without a browser host.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableBrowser;

impl BrowserLauncher for UnavailableBrowser {
    fn is_available(&self) -> bool {
        false
    }

    fn launch(&self) -> Result<LaunchedBrowser, LaunchBrowserError> {
        Err(LaunchBrowserError::Unavailable)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchBrowserError {
    #[error("this installation has no browser host")]
    Unavailable,
    #[error("the browser host cannot start")]
    Spawn(#[source] std::io::Error),
}

/// A started host: the pipe it reports on, and its processes.
pub struct LaunchedBrowser {
    pub events: Box<dyn Read + Send>,
    pub process: Box<dyn BrowserProcess>,
}

/// The host's processes, as the supervisor ends them.
pub trait BrowserProcess: Send {
    /// Asks the host to save its tabs and exit.
    fn request_exit(&mut self);

    /// Whether the host exited, without letting the system reuse its process id yet.
    fn has_exited(&mut self) -> bool;

    /// Kills the host and every process it started, and releases its process id.
    fn kill(&mut self);
}

/// What a host reported, tagged with the launch it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BrowserNotice {
    pub(crate) launch: u64,
    pub(crate) kind: BrowserNoticeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserNoticeKind {
    Ready,
    ViewAttached(bool),
    Audible(bool),
    /// The host is gone with every process it started; `requested` when the engine closed it.
    Exited {
        requested: bool,
    },
}

/// A running host, as the engine controls it.
pub(crate) struct BrowserHost {
    close_requests: mpsc::Sender<()>,
    router: mpsc::Sender<RouterControl>,
    exited: oneshot::Receiver<()>,
}

impl BrowserHost {
    /// Serves `launched` on two threads, reporting to `notices` under `launch`.
    pub(crate) fn serve(
        launched: LaunchedBrowser,
        launch: u64,
        notices: &UnboundedSender<BrowserNotice>,
        counters: Arc<BrowserAudioCounters>,
        exit_grace: Duration,
    ) -> std::io::Result<Self> {
        let LaunchedBrowser {
            mut events,
            mut process,
        } = launched;
        let (router_sender, router_receiver) = mpsc::channel();
        let (close_sender, close_receiver) = mpsc::channel();
        let (exited_sender, exited_receiver) = oneshot::channel();
        let notify = {
            let notices = notices.clone();
            move |kind| {
                let _ = notices.send(BrowserNotice { launch, kind });
            }
        };
        let pipe_notify = notify.clone();
        let spawned = thread::Builder::new()
            .name("tufo-browser-pipe".to_owned())
            .spawn(move || {
                read_pipe(
                    &mut events,
                    &router_receiver,
                    AudioRouter::new(counters),
                    &pipe_notify,
                );
            });
        if let Err(error) = spawned {
            process.kill();
            return Err(error);
        }
        thread::Builder::new()
            .name("tufo-browser-supervisor".to_owned())
            .spawn(move || {
                let requested = supervise(process.as_mut(), &close_receiver, exit_grace);
                notify(BrowserNoticeKind::Exited { requested });
                let _ = exited_sender.send(());
            })?;
        Ok(Self {
            close_requests: close_sender,
            router: router_sender,
            exited: exited_receiver,
        })
    }

    /// Plays the pages on `input` from now on.
    pub(crate) fn attach(&self, input: BrowserInputHandle) {
        let _ = self.router.send(RouterControl::Attach(input));
    }

    pub(crate) fn detach(&self) {
        let _ = self.router.send(RouterControl::Detach);
    }

    /// Asks the host to exit; the receiver resolves once every process it started is gone.
    pub(crate) fn close(self) -> oneshot::Receiver<()> {
        let _ = self.close_requests.send(());
        self.exited
    }
}

fn read_pipe(
    events: &mut dyn Read,
    controls: &mpsc::Receiver<RouterControl>,
    mut router: AudioRouter,
    notify: &impl Fn(BrowserNoticeKind),
) {
    let report_audible = |audible| {
        if let Audible::Changed(audible) = audible {
            notify(BrowserNoticeKind::Audible(audible));
        }
    };
    loop {
        let event = match read_host_event(events) {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, "stopped reading the browser host's pipe");
                break;
            }
        };
        while let Ok(control) = controls.try_recv() {
            router.control(control);
        }
        match event {
            HostEvent::Ready => notify(BrowserNoticeKind::Ready),
            HostEvent::ViewChanged { attached } => {
                notify(BrowserNoticeKind::ViewAttached(attached));
            }
            HostEvent::AudioStarted {
                stream,
                sample_rate,
                channels,
            } => report_audible(router.start_stream(
                stream,
                sample_rate.get(),
                usize::try_from(channels.get()).unwrap_or(usize::MAX),
            )),
            HostEvent::AudioPacket { stream, samples } => router.route_packet(stream, &samples),
            HostEvent::AudioStopped { stream } => report_audible(router.stop_stream(stream)),
        }
    }
    if router.is_audible() {
        notify(BrowserNoticeKind::Audible(false));
    }
}

/// Waits for a close request or the host's own exit, then makes sure nothing it started remains.
/// Returns whether the engine asked it to close.
fn supervise(
    process: &mut dyn BrowserProcess,
    close_requests: &mpsc::Receiver<()>,
    exit_grace: Duration,
) -> bool {
    let requested = loop {
        match close_requests.recv_timeout(EXIT_POLL_INTERVAL) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break true,
            Err(mpsc::RecvTimeoutError::Timeout) if process.has_exited() => break false,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    if requested {
        process.request_exit();
        let deadline = Instant::now() + exit_grace;
        while !process.has_exited() && Instant::now() < deadline {
            thread::sleep(EXIT_POLL_INTERVAL);
        }
        if !process.has_exited() {
            tracing::warn!("the browser host did not exit in time; killing it");
        }
    }
    process.kill();
    requested
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroU32,
        sync::atomic::{AtomicBool, Ordering},
    };

    use tufo_wire::browser_host::{AudioStreamId, write_host_event};

    use super::*;

    #[derive(Clone, Default)]
    struct FakeProcess {
        exits_on_request: bool,
        exit_requested: Arc<AtomicBool>,
        exited: Arc<AtomicBool>,
        killed: Arc<AtomicBool>,
    }

    impl BrowserProcess for FakeProcess {
        fn request_exit(&mut self) {
            self.exit_requested.store(true, Ordering::SeqCst);
            if self.exits_on_request {
                self.exited.store(true, Ordering::SeqCst);
            }
        }

        fn has_exited(&mut self) -> bool {
            self.exited.load(Ordering::SeqCst)
        }

        fn kill(&mut self) {
            self.killed.store(true, Ordering::SeqCst);
            self.exited.store(true, Ordering::SeqCst);
        }
    }

    fn stream(id: u32) -> AudioStreamId {
        AudioStreamId(NonZeroU32::new(id).unwrap())
    }

    fn pipe(events: &[HostEvent]) -> Box<dyn Read + Send> {
        let mut bytes = Vec::new();
        for event in events {
            write_host_event(&mut bytes, event).unwrap();
        }
        Box::new(std::io::Cursor::new(bytes))
    }

    async fn notices_until_exit(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<BrowserNotice>,
    ) -> Vec<BrowserNoticeKind> {
        let mut kinds = Vec::new();
        while let Some(notice) = receiver.recv().await {
            assert_eq!(notice.launch, 7);
            kinds.push(notice.kind);
            if matches!(notice.kind, BrowserNoticeKind::Exited { .. }) {
                break;
            }
        }
        kinds
    }

    #[tokio::test]
    async fn a_host_that_dies_reports_its_audio_ending_then_an_unrequested_exit() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let process = FakeProcess::default();
        let events = pipe(&[
            HostEvent::Ready,
            HostEvent::ViewChanged { attached: true },
            HostEvent::AudioStarted {
                stream: stream(1),
                sample_rate: NonZeroU32::new(48_000).unwrap(),
                channels: NonZeroU32::new(2).unwrap(),
            },
        ]);
        let host = BrowserHost::serve(
            LaunchedBrowser {
                events,
                process: Box::new(process.clone()),
            },
            7,
            &sender,
            Arc::default(),
            Duration::from_secs(1),
        )
        .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        process.exited.store(true, Ordering::SeqCst);
        let kinds = notices_until_exit(&mut receiver).await;

        assert_eq!(
            kinds,
            [
                BrowserNoticeKind::Ready,
                BrowserNoticeKind::ViewAttached(true),
                BrowserNoticeKind::Audible(true),
                BrowserNoticeKind::Audible(false),
                BrowserNoticeKind::Exited { requested: false },
            ]
        );
        assert!(process.killed.load(Ordering::SeqCst));
        assert!(!process.exit_requested.load(Ordering::SeqCst));
        drop(host);
    }

    #[tokio::test]
    async fn closing_asks_first_and_kills_what_outlives_the_grace() {
        for exits_on_request in [true, false] {
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let process = FakeProcess {
                exits_on_request,
                ..FakeProcess::default()
            };
            let host = BrowserHost::serve(
                LaunchedBrowser {
                    events: pipe(&[]),
                    process: Box::new(process.clone()),
                },
                7,
                &sender,
                Arc::default(),
                Duration::from_millis(200),
            )
            .unwrap();

            host.close().await.unwrap();

            assert!(process.exit_requested.load(Ordering::SeqCst));
            assert!(process.killed.load(Ordering::SeqCst));
            assert_eq!(
                notices_until_exit(&mut receiver).await,
                [BrowserNoticeKind::Exited { requested: true }]
            );
        }
    }
}
