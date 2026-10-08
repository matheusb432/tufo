//! The events the host reports to the server over the pipe that was its standard output.

use std::{
    fs::File,
    io::{BufWriter, Write as _},
    os::fd::{FromRawFd as _, OwnedFd},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread,
};

use tufo_wire::browser_host::{HostEvent, write_host_event};

/// Events waiting for the pipe: two seconds of audio from two pages.
const EVENTS_QUEUED_MAX: usize = 400;

/// Hands events to the writer thread without waiting; a full queue drops the event.
#[derive(Clone)]
pub(crate) struct HostEvents {
    sender: SyncSender<HostEvent>,
    dropped: Arc<AtomicU64>,
}

impl HostEvents {
    pub(crate) fn send(&self, event: HostEvent) {
        match self.sender.try_send(event) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped.is_power_of_two() {
                    tracing::warn!(dropped, "the server reads the browser's events too slowly");
                }
            }
        }
    }
}

/// Moves standard output aside for the events, pointing it at standard error so nothing else
/// Chromium or its libraries print reaches the pipe, and starts the writer. Call before any
/// other thread starts.
pub(crate) fn start() -> std::io::Result<HostEvents> {
    // SAFETY: F_DUPFD_CLOEXEC duplicates standard output to a new descriptor that child
    // processes don't inherit.
    let duplicate = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a fresh descriptor this process owns.
    let pipe = File::from(unsafe { OwnedFd::from_raw_fd(duplicate) });
    // SAFETY: dup2 only replaces standard output with standard error.
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (sender, receiver) = sync_channel(EVENTS_QUEUED_MAX);
    thread::Builder::new()
        .name("tufo-browser-events".to_owned())
        .spawn(move || write_events(&receiver, pipe))?;
    Ok(HostEvents {
        sender,
        dropped: Arc::default(),
    })
}

/// Writes each event in one system call.
fn write_events(receiver: &Receiver<HostEvent>, pipe: File) {
    let mut pipe = BufWriter::new(pipe);
    while let Ok(event) = receiver.recv() {
        if let Err(error) = write_host_event(&mut pipe, &event).and_then(|()| pipe.flush()) {
            tracing::warn!(%error, "the server stopped reading the browser's events");
            return;
        }
    }
}
