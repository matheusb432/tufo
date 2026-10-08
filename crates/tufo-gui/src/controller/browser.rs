//! The browser page's connection to the browser host: its tabs while the server reports it open,
//! and a view of the selected tab while the page shows on a visible window.

use std::{num::NonZeroU32, time::Instant};

use slint::{Rgba8Pixel, SharedPixelBuffer};
use tokio::{sync::mpsc, task::JoinHandle};
use tufo_client::{
    LocalEndpoint,
    browser::{
        BrowserClient, KeyModifiers, MouseButton, PIXEL_BYTES, ScaleFactor, ViewFrame, ViewInput,
        ViewOutput, ViewPoint, ViewSize,
    },
    player::UpdatePlayerSettings,
};
use tufo_models::{
    browser::{BrowserTabs, PageAddress, TabId},
    playback::browser::BrowserStatus,
};

use super::{Controller, VolumeTarget};
use crate::{
    browser_input::{ClickCounter, key_input},
    i18n::tr,
    presentation::{self, PageKind},
    render,
};

/// Input waiting for the view; a full queue drops the newest, such as a burst of moves.
const VIEW_INPUTS_QUEUED_MAX: usize = 128;

pub(super) struct BrowserSession {
    client: Option<BrowserClient>,
    tabs: BrowserTabs,
    watcher: Option<TabWatcher>,
    view: Option<AttachedView>,
    /// The pane's logical size and the window's scale, once the pane reported them.
    pane: Option<PaneGeometry>,
    window_visible: bool,
    clicks: ClickCounter,
}

/// The pane's size in logical pixels and the window's scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PaneGeometry {
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) scale_factor: f32,
}

struct AttachedView {
    inputs: mpsc::Sender<ViewInput>,
    size: ViewSize,
    task: JoinHandle<()>,
}

impl Drop for AttachedView {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Default for BrowserSession {
    fn default() -> Self {
        Self {
            client: None,
            tabs: BrowserTabs::default(),
            watcher: None,
            view: None,
            pane: None,
            window_visible: true,
            clicks: ClickCounter::default(),
        }
    }
}

impl BrowserSession {
    /// Forgets the browser that closed, keeping what the window reported about the pane.
    fn disconnect(&mut self) {
        self.client = None;
        self.tabs = BrowserTabs::default();
        self.watcher = None;
        self.view = None;
    }
}

/// Follows the tabs until dropped.
struct TabWatcher(JoinHandle<()>);

impl Drop for TabWatcher {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What the pane's pointer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanePointer {
    Moved,
    Pressed(MouseButton),
    Released(MouseButton),
    Left,
}

impl Controller {
    /// Follows the browser the server reports: connects once it is open and lets go once it
    /// isn't. Runs after every player state.
    pub(super) fn sync_browser(&self) {
        let open = self.lock().player.browser.status == BrowserStatus::Open;
        let connect = {
            let mut shared = self.lock();
            let session = &mut shared.browser;
            match (open, session.client.is_some()) {
                (true, false) => true,
                (false, true) => {
                    session.disconnect();
                    false
                }
                _ => false,
            }
        };
        if !open {
            render::clear_browser_frame(&self.window);
        }
        if connect {
            self.connect_browser();
        }
        self.render_browser();
        self.sync_browser_view();
    }

    fn connect_browser(&self) {
        let client = match LocalEndpoint::browser_from_environment()
            .map_err(tufo_client::ConnectError::from)
            .and_then(|endpoint| BrowserClient::connect_lazy(&endpoint))
        {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(%error, "cannot reach the browser");
                return;
            }
        };
        let watcher = self
            .runtime
            .spawn(self.clone().follow_browser_tabs(client.clone()));
        let mut shared = self.lock();
        shared.browser.client = Some(client);
        shared.browser.watcher = Some(TabWatcher(watcher));
    }

    async fn follow_browser_tabs(self, client: BrowserClient) {
        let mut states = match client.watch_browser().await {
            Ok(states) => states,
            Err(error) => {
                tracing::warn!(%error, "cannot follow the browser's tabs");
                return;
            }
        };
        while let Some(Ok(tabs)) = states.next().await {
            self.lock().browser.tabs = tabs;
            self.render_browser();
        }
    }

    pub(crate) fn render_browser(&self) {
        let view = {
            let shared = self.lock();
            presentation::browser_page(shared.player.browser, &shared.browser.tabs)
        };
        render::browser_page(&self.window, view);
    }

    /// Attaches a view while the page shows on a visible window with a size, and detaches it
    /// otherwise, so hidden pages stop painting and keep playing.
    pub(super) fn sync_browser_view(&self) {
        let mut shared = self.lock();
        let showing = shared.navigation.page == PageKind::Browser && shared.browser.window_visible;
        let session = &mut shared.browser;
        let size = session.pane.and_then(view_size);
        let (Some(client), true, Some(size)) = (session.client.clone(), showing, size) else {
            if session.view.take().is_some() {
                tracing::debug!("detached the browser view");
            }
            return;
        };
        if let Some(view) = &mut session.view {
            if view.size != size && view.inputs.try_send(ViewInput::Resize(size)).is_ok() {
                view.size = size;
            }
            return;
        }
        let (inputs, receiver) = mpsc::channel(VIEW_INPUTS_QUEUED_MAX);
        let _ = inputs.try_send(ViewInput::Resize(size));
        let task = self
            .runtime
            .spawn(self.clone().show_browser_view(client, receiver));
        tracing::debug!(?size, "attached the browser view");
        session.view = Some(AttachedView { inputs, size, task });
    }

    async fn show_browser_view(self, client: BrowserClient, inputs: mpsc::Receiver<ViewInput>) {
        let mut view = match client.view_browser(inputs).await {
            Ok(view) => view,
            Err(error) => {
                tracing::warn!(%error, "cannot view the browser");
                return;
            }
        };
        let mut canvas = PaneCanvas::default();
        while let Some(Ok(output)) = view.next().await {
            self.show_view_output(&mut canvas, output);
        }
    }

    fn show_view_output(&self, canvas: &mut PaneCanvas, output: ViewOutput) {
        match output {
            ViewOutput::Frame(frame) => {
                if let Some(pixels) = canvas.apply(&frame) {
                    render::browser_frame(&self.window, pixels);
                }
            }
            ViewOutput::PointerShape(shape) => render::browser_pointer(&self.window, shape),
        }
    }

    fn send_view_input(&self, input: ViewInput) {
        let shared = self.lock();
        let Some(view) = &shared.browser.view else {
            return;
        };
        if let Err(error) = view.inputs.try_send(input) {
            tracing::debug!(%error, "the browser view dropped the input");
        }
    }

    // Window actions.

    pub(crate) fn browser_pane_resized(&self, geometry: PaneGeometry) {
        self.lock().browser.pane = Some(geometry);
        self.sync_browser_view();
    }

    /// The window moved to a display of another scale.
    pub(crate) fn set_scale_factor(&self, scale_factor: f32) {
        if let Some(pane) = &mut self.lock().browser.pane {
            pane.scale_factor = scale_factor;
        }
        self.sync_browser_view();
    }

    pub(crate) fn set_window_visible(&self, visible: bool) {
        self.lock().browser.window_visible = visible;
        self.sync_browser_view();
    }

    pub(crate) fn open_browser(&self) {
        self.run(|client| async move { client.open_browser().await });
    }

    pub(crate) fn close_browser(&self) {
        self.run(|client| async move { client.close_browser().await });
    }

    pub(crate) fn browser_pointer(
        &self,
        pointer: PanePointer,
        x: f32,
        y: f32,
        modifiers: KeyModifiers,
    ) {
        let position = view_point(x, y);
        let input = match pointer {
            PanePointer::Moved => ViewInput::PointerMoved {
                position,
                modifiers,
            },
            PanePointer::Left => ViewInput::PointerLeft,
            PanePointer::Pressed(button) => ViewInput::PointerButton {
                position,
                button,
                pressed: true,
                click_count: self
                    .lock()
                    .browser
                    .clicks
                    .press(button, position, Instant::now()),
                modifiers,
            },
            PanePointer::Released(button) => ViewInput::PointerButton {
                position,
                button,
                pressed: false,
                click_count: self.lock().browser.clicks.release(button),
                modifiers,
            },
        };
        self.send_view_input(input);
    }

    pub(crate) fn browser_scroll(
        &self,
        position: (f32, f32),
        delta: (f32, f32),
        modifiers: KeyModifiers,
    ) {
        self.send_view_input(ViewInput::PointerWheel {
            position: view_point(position.0, position.1),
            delta_x: delta.0,
            delta_y: delta.1,
            modifiers,
        });
    }

    pub(crate) fn browser_key(
        &self,
        text: &str,
        pressed: bool,
        repeat: bool,
        modifiers: KeyModifiers,
    ) {
        if let Some(key) = key_input(text, pressed, repeat, modifiers) {
            self.send_view_input(ViewInput::Key(key));
        }
    }

    pub(crate) fn browser_focus(&self, focused: bool) {
        self.send_view_input(ViewInput::Focus(focused));
    }

    pub(crate) fn browser_paste(&self, text: &str) {
        if !text.is_empty() {
            self.send_view_input(ViewInput::InsertText(text.to_owned()));
        }
    }

    /// Loads what was typed in the selected tab, or in a new one without tabs.
    pub(crate) fn browser_navigate(&self, typed: &str) {
        let Some(address) = PageAddress::from_typed(typed) else {
            return;
        };
        let selected = self.lock().browser.tabs.selected;
        self.run_on_browser(move |browser| async move {
            match selected {
                Some(tab) => browser.navigate_tab(tab, &address).await,
                None => browser.open_tab(Some(&address)).await.map(drop),
            }
        });
    }

    pub(crate) fn browser_new_tab(&self) {
        self.run_on_browser(|browser| async move { browser.open_tab(None).await.map(drop) });
    }

    pub(crate) fn browser_select_tab(&self, id: i32) {
        if let Some(tab) = tab_id(id) {
            self.run_on_browser(move |browser| async move { browser.select_tab(tab).await });
        }
    }

    pub(crate) fn browser_close_tab(&self, id: i32) {
        if let Some(tab) = tab_id(id) {
            self.run_on_browser(move |browser| async move { browser.close_tab(tab).await });
        }
    }

    pub(crate) fn browser_back(&self) {
        self.on_selected_tab(|browser, tab| async move { browser.go_back(tab).await });
    }

    pub(crate) fn browser_forward(&self) {
        self.on_selected_tab(|browser, tab| async move { browser.go_forward(tab).await });
    }

    pub(crate) fn browser_reload(&self) {
        self.on_selected_tab(|browser, tab| async move { browser.reload_tab(tab).await });
    }

    pub(crate) fn browser_stop(&self) {
        self.on_selected_tab(|browser, tab| async move { browser.stop_loading_tab(tab).await });
    }

    pub(crate) fn set_browser_volume(&self, volume: f32) {
        self.queue_volume(VolumeTarget::Browser, volume);
    }

    pub(crate) fn toggle_browser_mute(&self) {
        let muted = !self.lock().player.browser.muted;
        self.update_settings(UpdatePlayerSettings {
            browser_muted: Some(muted),
            ..UpdatePlayerSettings::default()
        });
    }

    fn on_selected_tab<Command, Request>(&self, command: Command)
    where
        Command: FnOnce(BrowserClient, TabId) -> Request + Send + 'static,
        Request: Future<Output = Result<(), tufo_client::ClientError>> + Send,
    {
        let Some(tab) = self.lock().browser.tabs.selected else {
            return;
        };
        self.run_on_browser(move |browser| command(browser, tab));
    }

    /// Runs a browser call, explaining a failure in a toast.
    fn run_on_browser<Command, Request>(&self, command: Command)
    where
        Command: FnOnce(BrowserClient) -> Request + Send + 'static,
        Request: Future<Output = Result<(), tufo_client::ClientError>> + Send,
    {
        let Some(browser) = self.lock().browser.client.clone() else {
            self.toast_error(tr("Open the browser first.").to_owned());
            return;
        };
        let controller = self.clone();
        self.runtime.spawn(async move {
            if let Err(error) = command(browser).await {
                tracing::warn!(%error, "the browser refused a request");
                controller.toast_error(tr("The browser could not do that. Try again.").to_owned());
            }
        });
    }
}

fn tab_id(id: i32) -> Option<TabId> {
    u32::try_from(id)
        .ok()
        .and_then(NonZeroU32::new)
        .map(TabId::new)
}

fn view_point(x: f32, y: f32) -> ViewPoint {
    // Positions are logical pixels within a window, far inside the i32 range.
    #[expect(clippy::cast_possible_truncation, reason = "positions fit an i32")]
    let point = ViewPoint {
        x: x.round() as i32,
        y: y.round() as i32,
    };
    point
}

fn view_size(pane: PaneGeometry) -> Option<ViewSize> {
    let whole = |length: f32| {
        // A pane's size is a few thousand logical pixels at most.
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pixels = length.floor().max(0.0) as u32;
        NonZeroU32::new(pixels)
    };
    ViewSize::try_new(
        whole(pane.width)?,
        whole(pane.height)?,
        ScaleFactor::try_new(pane.scale_factor).ok()?,
    )
    .ok()
}

/// The pane's latest frame, updated by each frame's changed rectangle.
#[derive(Default)]
struct PaneCanvas {
    pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
}

impl PaneCanvas {
    /// Applies a frame, returning the pixels to show; a partial frame without a full one first
    /// shows nothing.
    fn apply(&mut self, frame: &ViewFrame) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
        let size_matches = self.pixels.as_ref().is_some_and(|pixels| {
            (pixels.width(), pixels.height()) == (frame.width(), frame.height())
        });
        if !size_matches {
            if !frame.covers_frame() {
                return None;
            }
            self.pixels = Some(SharedPixelBuffer::new(frame.width(), frame.height()));
        }
        let pixels = self.pixels.as_mut()?;
        let width = frame.width() as usize;
        let rectangle = frame.rectangle();
        let row_bytes = rectangle.width as usize * PIXEL_BYTES;
        let target = pixels.make_mut_bytes();
        for (row, source) in frame.pixels().chunks_exact(row_bytes.max(1)).enumerate() {
            let start = ((rectangle.y as usize + row) * width + rectangle.x as usize) * PIXEL_BYTES;
            target[start..start + row_bytes].copy_from_slice(source);
        }
        Some(pixels.clone())
    }
}

#[cfg(test)]
mod tests {
    use tufo_client::browser::PixelRectangle;

    use super::*;

    fn frame(width: u32, height: u32, rectangle: PixelRectangle, value: u8) -> ViewFrame {
        let length = rectangle.width as usize * rectangle.height as usize * PIXEL_BYTES;
        ViewFrame::try_new(width, height, rectangle, vec![value; length]).unwrap()
    }

    #[test]
    fn partial_frames_update_the_full_one_they_follow() {
        let mut canvas = PaneCanvas::default();
        let whole = PixelRectangle {
            x: 0,
            y: 0,
            width: 3,
            height: 2,
        };
        let corner = PixelRectangle {
            x: 2,
            y: 1,
            width: 1,
            height: 1,
        };

        assert!(canvas.apply(&frame(3, 2, corner, 9)).is_none());
        canvas.apply(&frame(3, 2, whole, 1));
        let pixels = canvas.apply(&frame(3, 2, corner, 9)).unwrap();

        let bytes = pixels.as_bytes();
        assert_eq!(&bytes[..4], &[1, 1, 1, 1]);
        assert_eq!(&bytes[bytes.len() - 4..], &[9, 9, 9, 9]);
    }

    #[test]
    fn view_sizes_need_a_whole_logical_pixel_each_way() {
        let pane = |width, height| PaneGeometry {
            width,
            height,
            scale_factor: 1.25,
        };
        assert!(view_size(pane(0.5, 300.0)).is_none());
        let size = view_size(pane(640.7, 360.2)).unwrap();
        assert_eq!((size.width().get(), size.height().get()), (640, 360));
    }
}
