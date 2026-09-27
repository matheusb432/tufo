//! File drops on a native Wayland window, which winit 0.30 doesn't report.
//!
//! Tufo adds its own `wl_data_device` to winit's Wayland connection, on its own event queue.
//! winit keeps reading the socket, which fills that queue as well, and [`WaylandDrops::dispatch`]
//! empties it on the UI thread whenever winit's loop is about to wait: nothing here reads the
//! socket or blocks. Only a dropped file list arrives separately, over a pipe that a short-lived
//! thread reads.
//!
//! Delete this module once Slint moves to winit 0.31, whose Wayland backend reports drops.

use std::{
    ffi::OsString,
    io::{self, PipeReader, Read as _},
    os::{
        fd::{AsFd as _, AsRawFd as _},
        unix::ffi::OsStringExt as _,
    },
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

use slint::winit_030::winit::{
    event_loop::ActiveEventLoop,
    raw_window_handle::{HandleError, HasDisplayHandle as _, RawDisplayHandle},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy as _, QueueHandle,
    backend::{Backend, WaylandError},
    event_created_child,
    protocol::{
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager::{DndAction, WlDataDeviceManager},
        wl_data_offer::{self, WlDataOffer},
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
    },
};

use super::DropEvent;

const URI_LIST_MIME_TYPE: &str = "text/uri-list";
/// The newest `wl_data_device_manager` this module speaks: version 3 adds drop actions.
const DATA_DEVICE_MANAGER_VERSION_MAX: u32 = 3;
/// How long a drop's source gets to send its file list.
const FILE_LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest file list Tufo reads, far past any real drop.
const FILE_LIST_BYTES_MAX: usize = 1024 * 1024;

type Deliver = fn(DropEvent);

/// Listens for drops on winit's Wayland connection, once winit's event loop is running.
#[derive(Default)]
pub(crate) enum WaylandDrops {
    #[default]
    NotStarted,
    /// Leaked on purpose: it borrows winit's display, which lives until the process exits, and
    /// dropping it after that display would touch freed memory.
    Listening(&'static mut Listener),
    /// Not a Wayland session, or listening failed.
    Unavailable,
}

impl WaylandDrops {
    /// Starts listening on the first call, then hands the drops that arrived to `deliver`.
    pub(crate) fn dispatch(&mut self, event_loop: &ActiveEventLoop, deliver: Deliver) {
        if matches!(self, Self::NotStarted) {
            *self = match Listener::connect(event_loop, deliver) {
                Ok(Some(listener)) => {
                    tracing::info!("listening for file drops on Wayland");
                    Self::Listening(Box::leak(Box::new(listener)))
                }
                Ok(None) => Self::Unavailable,
                Err(error) => {
                    tracing::warn!(%error, "cannot listen for file drops on Wayland");
                    Self::Unavailable
                }
            };
        }
        if let Self::Listening(listener) = self
            && let Err(error) = listener.dispatch()
        {
            tracing::warn!(%error, "stopped listening for file drops on Wayland");
            *self = Self::Unavailable;
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ListenError {
    #[error("winit has no display handle: {0}")]
    DisplayHandle(#[from] HandleError),
    #[error(transparent)]
    Wayland(#[from] WaylandError),
    #[error(transparent)]
    Dispatch(#[from] wayland_client::DispatchError),
}

pub(crate) struct Listener {
    connection: Connection,
    queue: EventQueue<Drops>,
    drops: Drops,
}

impl Listener {
    /// Asks for the registry on winit's connection, or returns `None` outside Wayland.
    fn connect(
        event_loop: &ActiveEventLoop,
        deliver: Deliver,
    ) -> Result<Option<Self>, ListenError> {
        let RawDisplayHandle::Wayland(display) = event_loop.display_handle()?.as_raw() else {
            return Ok(None);
        };
        // SAFETY: the pointer is winit's connected `wl_display`, which stays alive until the
        // process exits, and the listener is leaked so it never outlives it.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        connection.flush()?;
        Ok(Some(Self {
            drops: Drops {
                connection: connection.clone(),
                deliver,
                manager: None,
                seats: Vec::new(),
                devices: Vec::new(),
                offer: None,
            },
            connection,
            queue,
        }))
    }

    /// Handles the events winit read for this queue since the last call.
    fn dispatch(&mut self) -> Result<(), ListenError> {
        self.queue.dispatch_pending(&mut self.drops)?;
        self.connection.flush()?;
        Ok(())
    }
}

/// Which mime types an offer carries, learned from its `offer` events.
#[derive(Default)]
struct OfferedTypes {
    file_list: Mutex<bool>,
}

impl OfferedTypes {
    fn has_file_list(&self) -> bool {
        *self
            .file_list
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct Drops {
    connection: Connection,
    deliver: Deliver,
    manager: Option<WlDataDeviceManager>,
    /// Seats announced before the manager, waiting for their data devices.
    seats: Vec<WlSeat>,
    devices: Vec<WlDataDevice>,
    /// The offer over the window, and whether it carries files.
    offer: Option<(WlDataOffer, bool)>,
}

impl Drops {
    fn create_devices(&mut self, queue: &QueueHandle<Self>) {
        let Some(manager) = &self.manager else {
            return;
        };
        for seat in self.seats.drain(..) {
            tracing::debug!("listening for drops on a seat");
            self.devices.push(manager.get_data_device(&seat, queue, ()));
        }
    }

    fn enter(&mut self, serial: u32, offer: Option<WlDataOffer>) {
        self.leave();
        let Some(offer) = offer else {
            return;
        };
        let files = offer
            .data::<OfferedTypes>()
            .is_some_and(OfferedTypes::has_file_list);
        tracing::debug!(files, "a drag entered the window");
        if files {
            offer.accept(serial, Some(URI_LIST_MIME_TYPE.to_owned()));
            if offer.version() >= 3 {
                offer.set_actions(DndAction::Copy, DndAction::Copy);
            }
            (self.deliver)(DropEvent::Hovering(true));
        } else {
            offer.accept(serial, None);
        }
        self.offer = Some((offer, files));
    }

    fn leave(&mut self) {
        if let Some((offer, files)) = self.offer.take() {
            offer.destroy();
            if files {
                (self.deliver)(DropEvent::Hovering(false));
            }
        }
    }

    fn drop_offer(&mut self) {
        match self.offer.take() {
            Some((offer, true)) => self.receive_files(offer),
            Some((offer, false)) => offer.destroy(),
            None => {}
        }
    }

    /// Reads the dropped file list on a helper thread, then delivers it on the UI thread.
    fn receive_files(&self, offer: WlDataOffer) {
        let (reader, writer) = match io::pipe() {
            Ok(pipe) => pipe,
            Err(error) => {
                tracing::warn!(%error, "cannot receive dropped files");
                finish(&self.connection, &offer);
                (self.deliver)(DropEvent::Hovering(false));
                return;
            }
        };
        offer.receive(URI_LIST_MIME_TYPE.to_owned(), writer.as_fd());
        drop(writer);
        let _ = self.connection.flush();
        let (connection, deliver) = (self.connection.clone(), self.deliver);
        let spawned = std::thread::Builder::new()
            .name("wayland-drop".to_owned())
            .spawn(move || {
                let paths = read_file_list(reader, FILE_LIST_TIMEOUT);
                let _ = slint::invoke_from_event_loop(move || {
                    finish(&connection, &offer);
                    deliver_files(deliver, paths);
                });
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "cannot receive dropped files");
            (self.deliver)(DropEvent::Hovering(false));
        }
    }
}

fn deliver_files(deliver: Deliver, paths: io::Result<Vec<PathBuf>>) {
    match paths {
        Ok(paths) => deliver(DropEvent::Dropped(paths)),
        Err(error) => {
            tracing::warn!(%error, "cannot read the dropped files");
            deliver(DropEvent::Hovering(false));
        }
    }
}

/// Tells the source the drop is done with, and forgets the offer.
fn finish(connection: &Connection, offer: &WlDataOffer) {
    if offer.version() >= 3 {
        offer.finish();
    }
    offer.destroy();
    let _ = connection.flush();
}

/// Reads the whole file list, giving up after `timeout` or past [`FILE_LIST_BYTES_MAX`].
fn read_file_list(mut reader: PipeReader, timeout: Duration) -> io::Result<Vec<PathBuf>> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut poll = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = libc::c_int::try_from(remaining.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: `poll` points to one pollfd for the reader's open descriptor.
        let ready = unsafe { libc::poll(&raw mut poll, 1, timeout_ms) };
        match ready {
            ..0 => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            0 => return Err(io::ErrorKind::TimedOut.into()),
            _ => {
                let read = reader.read(&mut chunk)?;
                if read == 0 {
                    return Ok(file_paths(&String::from_utf8_lossy(&bytes)));
                }
                bytes.extend_from_slice(&chunk[..read]);
                if bytes.len() > FILE_LIST_BYTES_MAX {
                    return Err(io::ErrorKind::InvalidData.into());
                }
            }
        }
    }
}

/// The local files a `text/uri-list` names, skipping its comments and every URI that isn't a
/// file on this machine.
fn file_paths(uri_list: &str) -> Vec<PathBuf> {
    uri_list
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(file_path)
        .collect()
}

fn file_path(uri: &str) -> Option<PathBuf> {
    let after_scheme = uri.strip_prefix("file://")?;
    let path = match after_scheme.find('/')? {
        0 => after_scheme,
        host_end if &after_scheme[..host_end] == "localhost" => &after_scheme[host_end..],
        _ => return None,
    };
    Some(PathBuf::from(OsString::from_vec(percent_decoded(path)?)))
}

fn percent_decoded(text: &str) -> Option<Vec<u8>> {
    let mut bytes = text.bytes();
    let mut decoded = Vec::with_capacity(text.len());
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            decoded.push(u8::try_from(high * 16 + low).ok()?);
        } else {
            decoded.push(byte);
        }
    }
    Some(decoded)
}

impl Dispatch<WlRegistry, ()> for Drops {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == WlDataDeviceManager::interface().name {
                state.manager = Some(registry.bind(
                    name,
                    version.min(DATA_DEVICE_MANAGER_VERSION_MAX),
                    queue,
                    (),
                ));
            } else if interface == WlSeat::interface().name {
                state.seats.push(registry.bind(name, 1, queue, ()));
            }
            state.create_devices(queue);
        }
    }
}

impl Dispatch<WlSeat, ()> for Drops {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDeviceManager, ()> for Drops {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for Drops {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::Enter { serial, id, .. } => state.enter(serial, id),
            wl_data_device::Event::Leave => state.leave(),
            wl_data_device::Event::Drop => state.drop_offer(),
            // Tufo doesn't read the clipboard, so its offers go at once.
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }

    event_created_child!(Drops, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferedTypes::default()),
    ]);
}

impl Dispatch<WlDataOffer, OfferedTypes> for Drops {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        types: &OfferedTypes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && mime_type == URI_LIST_MIME_TYPE
        {
            *types
                .file_list
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn a_uri_list_names_its_local_files() {
        let uri_list = "# dropped from Files\r\n\
            file:///home/gm/Music/Tavern%20night/01%20-%20Ale.flac\r\n\
            file://localhost/home/gm/sfx/door.wav\r\n\
            file://nas/share/storm.ogg\r\n\
            https://example.com/thunder.mp3\r\n";

        assert_eq!(
            file_paths(uri_list),
            [
                PathBuf::from("/home/gm/Music/Tavern night/01 - Ale.flac"),
                PathBuf::from("/home/gm/sfx/door.wav"),
            ]
        );
    }

    #[test]
    fn a_malformed_escape_skips_its_uri() {
        assert_eq!(
            file_paths("file:///home/gm/%zz.flac\nfile:///ok.flac"),
            [PathBuf::from("/ok.flac")]
        );
    }

    #[test]
    fn the_file_list_arrives_when_the_source_closes_its_end() {
        let (reader, mut writer) = io::pipe().unwrap();
        writer.write_all(b"file:///home/gm/rain.ogg\r\n").unwrap();
        drop(writer);

        let paths = read_file_list(reader, Duration::from_secs(1)).unwrap();

        assert_eq!(paths, [PathBuf::from("/home/gm/rain.ogg")]);
    }

    #[test]
    fn a_source_that_never_finishes_times_out() {
        let (reader, _writer) = io::pipe().unwrap();

        let error = read_file_list(reader, Duration::from_millis(50)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
