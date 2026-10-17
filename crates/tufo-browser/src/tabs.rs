//! The tabs, owned by CEF's UI thread: their browsers, what each shows, and which one the view
//! shows. CEF can call back synchronously from its own calls, so nothing calls into CEF while the
//! state is borrowed.

use std::{cell::RefCell, num::NonZeroU32, path::PathBuf, sync::Arc, time::Duration};

use cef::{
    Browser, BrowserSettings, Client, ImplBrowser, ImplBrowserHost, ImplFrame, PaintElementType,
    Range, ThreadId, WindowInfo, browser_host_create_browser_sync, currently_on, quit_message_loop,
};
use tokio::sync::watch;
use tufo_models::browser::{BrowserTab, BrowserTabs, PageAddress, PageFailure, TabHistory, TabId};
use tufo_wire::{
    browser::{ViewInput, ViewSize},
    browser_host::HostEvent,
};

use crate::{
    Host,
    input::{InputTranslator, PageEvent},
    session::{Session, TABS_MAX},
    ui::run_on_ui_after,
    view::{BrowserNumber, ViewerId},
};

const BLANK_ADDRESS: &str = "about:blank";
/// Pages repaint at most this often while a view shows them.
const FRAMES_PER_SECOND: i32 = 30;
/// How long closing pages may take before the host quits regardless.
const CLOSE_GRACE: Duration = Duration::from_secs(3);
const OPAQUE_WHITE: u32 = 0xFFFF_FFFF;
#[cfg(target_os = "linux")]
const NO_PARENT_WINDOW: cef::sys::cef_window_handle_t = 0;
#[cfg(windows)]
const NO_PARENT_WINDOW: cef::sys::cef_window_handle_t = cef::sys::HWND(std::ptr::null_mut());
/// Inserts committed text at the caret; CEF drops a commit without a range.
const CARET_RANGE: Range = Range {
    from: u32::MAX,
    to: u32::MAX,
};

thread_local! {
    static TABS: RefCell<Option<Tabs>> = const { RefCell::new(None) };
}

pub(crate) struct Tabs {
    host: Arc<Host>,
    client: Client,
    profile: PathBuf,
    tabs: Vec<Tab>,
    selected: Option<TabId>,
    last_tab_number: u32,
    published: watch::Sender<BrowserTabs>,
    translator: InputTranslator,
    closing: bool,
}

struct Tab {
    browser: Browser,
    number: BrowserNumber,
    shown: BrowserTab,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum OpenTabError {
    #[error("the browser keeps at most {TABS_MAX} tabs")]
    TooMany,
    #[error("the browser is closing")]
    Closing,
    #[error("Chromium did not create the tab")]
    NotCreated,
}

#[derive(Debug, thiserror::Error)]
#[error("no such tab")]
pub(crate) struct TabNotFound;

impl Tabs {
    pub(crate) fn new(
        host: Arc<Host>,
        client: Client,
        profile: PathBuf,
        published: watch::Sender<BrowserTabs>,
    ) -> Self {
        Self {
            host,
            client,
            profile,
            tabs: Vec::new(),
            selected: None,
            last_tab_number: 0,
            published,
            translator: InputTranslator::default(),
            closing: false,
        }
    }

    fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.shown.id == id)
    }

    fn selected_browser(&self) -> Option<Browser> {
        self.selected
            .and_then(|id| self.tab(id))
            .map(|tab| tab.browser.clone())
    }

    fn publish(&self) {
        self.published.send_replace(BrowserTabs {
            tabs: self.tabs.iter().map(|tab| tab.shown.clone()).collect(),
            selected: self.selected,
        });
    }

    fn save(&self) {
        let session = Session {
            addresses: self
                .tabs
                .iter()
                .filter_map(|tab| PageAddress::parse(&tab.shown.address).ok())
                .collect(),
            selected: self
                .selected
                .and_then(|id| self.tabs.iter().position(|tab| tab.shown.id == id)),
        };
        if let Err(error) = session.save(&self.profile) {
            tracing::warn!(%error, "cannot save the open tabs");
        }
    }

    fn browsers(&self) -> Vec<Browser> {
        self.tabs.iter().map(|tab| tab.browser.clone()).collect()
    }
}

/// Makes `tabs` the UI thread's state; call once, on the UI thread.
pub(crate) fn install(tabs: Tabs) {
    TABS.with(|state| *state.borrow_mut() = Some(tabs));
}

/// Drops every browser handle, which CEF needs before it shuts down.
pub(crate) fn uninstall() {
    let tabs = TABS.with(|state| state.borrow_mut().take());
    drop(tabs);
}

fn with_tabs<T>(work: impl FnOnce(&mut Tabs) -> T) -> Option<T> {
    TABS.with(|state| state.borrow_mut().as_mut().map(work))
}

/// Applies a change reported by a CEF callback, at once or, when CEF called back from another
/// thread or from inside a change already under way, right after it on the UI thread.
pub(crate) fn update_tabs(work: impl FnOnce(&mut Tabs) + Send + 'static) {
    if currently_on(ThreadId::UI) == 0 {
        crate::ui::run_on_ui(move || update_tabs(work));
        return;
    }
    let applied = TABS.with(|state| match state.try_borrow_mut() {
        Ok(mut state) => {
            if let Some(tabs) = state.as_mut() {
                work(tabs);
            }
            None
        }
        Err(_) => Some(work),
    });
    if let Some(work) = applied {
        crate::ui::run_on_ui(move || {
            with_tabs(work);
        });
    }
}

/// Opens the saved tabs, selecting the one that was selected.
pub(crate) fn restore_session(profile: &std::path::Path) {
    let session = Session::load(profile);
    let mut opened = Vec::new();
    for address in &session.addresses {
        match open_tab_unselected(Some(address)) {
            Ok(id) => opened.push(id),
            Err(error) => tracing::warn!(%error, "cannot restore a tab"),
        }
    }
    let selected = session
        .selected
        .and_then(|index| opened.get(index))
        .or(opened.first());
    if let Some(id) = selected {
        let _ = select_tab(*id);
    }
}

/// Opens a tab after the others and selects it.
pub(crate) fn open_tab(address: Option<&PageAddress>) -> Result<TabId, OpenTabError> {
    let id = open_tab_unselected(address)?;
    let _ = select_tab(id);
    Ok(id)
}

fn open_tab_unselected(address: Option<&PageAddress>) -> Result<TabId, OpenTabError> {
    let (mut client, id) = with_tabs(|tabs| {
        if tabs.closing {
            return Err(OpenTabError::Closing);
        }
        if tabs.tabs.len() >= TABS_MAX {
            return Err(OpenTabError::TooMany);
        }
        tabs.last_tab_number += 1;
        let id = NonZeroU32::new(tabs.last_tab_number)
            .map(TabId::new)
            .ok_or(OpenTabError::TooMany)?;
        Ok((tabs.client.clone(), id))
    })
    .ok_or(OpenTabError::Closing)??;
    let address_text = address.map_or(BLANK_ADDRESS, PageAddress::as_str);
    let window = WindowInfo::default().set_as_windowless(NO_PARENT_WINDOW);
    let settings = BrowserSettings {
        windowless_frame_rate: FRAMES_PER_SECOND,
        background_color: OPAQUE_WHITE,
        ..BrowserSettings::default()
    };
    let browser = browser_host_create_browser_sync(
        Some(&window),
        Some(&mut client),
        Some(&address_text.into()),
        Some(&settings),
        None,
        None,
    )
    .ok_or(OpenTabError::NotCreated)?;
    if let Some(host) = browser.host() {
        host.was_hidden(1);
    }
    let number = browser.identifier();
    with_tabs(|tabs| {
        tabs.tabs.push(Tab {
            browser,
            number,
            shown: BrowserTab {
                id,
                address: address_text.to_owned(),
                title: String::new(),
                loading: address.is_some(),
                history: TabHistory::default(),
                audible: false,
                failure: None,
            },
        });
        tabs.publish();
        tabs.save();
    });
    Ok(id)
}

/// Shows a tab in the view, hiding the one shown before.
pub(crate) fn select_tab(id: TabId) -> Result<(), TabNotFound> {
    let (previous, next, host) = with_tabs(|tabs| {
        let next = tabs.tab(id).map(|tab| (tab.browser.clone(), tab.number))?;
        let previous = tabs.selected_browser();
        tabs.selected = Some(id);
        tabs.publish();
        tabs.save();
        Some((previous, next, Arc::clone(&tabs.host)))
    })
    .flatten()
    .ok_or(TabNotFound)?;
    let (next, number) = next;
    host.view.show_browser(Some(number));
    if let Some(previous) = previous.filter(|previous| previous.identifier() != number)
        && let Some(previous) = previous.host()
    {
        previous.was_hidden(1);
    }
    if host.view.viewer().is_some() {
        show(&next);
    }
    Ok(())
}

/// Closes a tab, selecting its neighbour if it was selected.
pub(crate) fn close_tab(id: TabId) {
    let closed = with_tabs(|tabs| {
        let index = tabs.tabs.iter().position(|tab| tab.shown.id == id)?;
        let closed = tabs.tabs.remove(index);
        let neighbour = if tabs.selected == Some(id) {
            let neighbour = tabs
                .tabs
                .get(index)
                .or_else(|| tabs.tabs.last())
                .map(|tab| tab.shown.id);
            tabs.selected = None;
            neighbour
        } else {
            None
        };
        tabs.publish();
        tabs.save();
        Some((closed.browser, neighbour, Arc::clone(&tabs.host)))
    })
    .flatten();
    let Some((browser, neighbour, host)) = closed else {
        return;
    };
    if let Some(neighbour) = neighbour {
        let _ = select_tab(neighbour);
    } else if with_tabs(|tabs| tabs.selected.is_none()).unwrap_or(true) {
        host.view.show_browser(None);
    }
    if let Some(browser_host) = browser.host() {
        browser_host.close_browser(1);
    }
}

/// Runs `action` on a tab's browser.
pub(crate) fn with_browser(id: TabId, action: impl FnOnce(&Browser)) -> Result<(), TabNotFound> {
    let browser = with_tabs(|tabs| tabs.tab(id).map(|tab| tab.browser.clone()))
        .flatten()
        .ok_or(TabNotFound)?;
    action(&browser);
    Ok(())
}

pub(crate) fn navigate_tab(id: TabId, address: &PageAddress) -> Result<(), TabNotFound> {
    with_browser(id, |browser| {
        if let Some(frame) = browser.main_frame() {
            frame.load_url(Some(&address.as_str().into()));
        }
    })
}

/// Attaches a view: the selected tab paints for it from now on.
pub(crate) fn attach_view(viewer: ViewerId, size: ViewSize) {
    let Some((host, selected, all)) = with_tabs(|tabs| {
        tabs.translator = InputTranslator::default();
        (
            Arc::clone(&tabs.host),
            tabs.selected_browser(),
            tabs.browsers(),
        )
    }) else {
        return;
    };
    host.view.attach(viewer, size);
    for browser in &all {
        if let Some(browser_host) = browser.host() {
            browser_host.notify_screen_info_changed();
            browser_host.was_resized();
        }
    }
    if let Some(selected) = selected {
        show(&selected);
    }
    host.events.send(HostEvent::ViewChanged { attached: true });
}

pub(crate) fn resize_view(viewer: ViewerId, size: ViewSize) {
    let Some((host, all)) = with_tabs(|tabs| (Arc::clone(&tabs.host), tabs.browsers())) else {
        return;
    };
    if !host.view.resize(viewer, size) {
        return;
    }
    for browser in &all {
        if let Some(browser_host) = browser.host() {
            browser_host.notify_screen_info_changed();
            browser_host.was_resized();
        }
    }
}

/// Detaches a view; pages stop painting, and playing ones keep playing.
pub(crate) fn detach_view(viewer: ViewerId) {
    let Some((host, all)) = with_tabs(|tabs| (Arc::clone(&tabs.host), tabs.browsers())) else {
        return;
    };
    if !host.view.detach(viewer) {
        return;
    }
    for browser in &all {
        if let Some(browser_host) = browser.host() {
            browser_host.was_hidden(1);
        }
    }
    host.events.send(HostEvent::ViewChanged { attached: false });
}

/// Sends the view's input to the selected tab.
pub(crate) fn apply_input(viewer: ViewerId, input: ViewInput) {
    let Some((events, selected)) = with_tabs(|tabs| {
        if tabs.host.view.viewer() != Some(viewer) {
            tracing::debug!(viewer, "input from a replaced view");
            return None;
        }
        Some((tabs.translator.translate(input), tabs.selected_browser()?))
    })
    .flatten() else {
        return;
    };
    let Some(browser_host) = selected.host() else {
        return;
    };
    for event in events {
        match event {
            PageEvent::MouseMove { event, leaves } => {
                browser_host.send_mouse_move_event(Some(&event), i32::from(leaves));
            }
            PageEvent::MouseClick {
                event,
                button,
                released,
                click_count,
            } => browser_host.send_mouse_click_event(
                Some(&event),
                button,
                i32::from(released),
                click_count,
            ),
            PageEvent::MouseWheel {
                event,
                delta_x,
                delta_y,
            } => browser_host.send_mouse_wheel_event(Some(&event), delta_x, delta_y),
            PageEvent::Key(event) => browser_host.send_key_event(Some(&event)),
            PageEvent::Focus(focused) => browser_host.set_focus(i32::from(focused)),
            PageEvent::CommitText(text) => {
                browser_host.ime_commit_text(Some(&text.as_str().into()), Some(&CARET_RANGE), 0);
            }
        }
    }
}

fn show(browser: &Browser) {
    if let Some(browser_host) = browser.host() {
        browser_host.was_hidden(0);
        browser_host.was_resized();
        browser_host.invalidate(PaintElementType::VIEW);
    }
}

/// Changes what a tab shows, by its browser's number.
pub(crate) fn update_tab(
    number: BrowserNumber,
    change: impl FnOnce(&mut BrowserTab) + Send + 'static,
) {
    update_tabs(move |tabs| {
        let Some(tab) = tabs.tabs.iter_mut().find(|tab| tab.number == number) else {
            return;
        };
        let address_before = tab.shown.address.clone();
        change(&mut tab.shown);
        let address_changed = tab.shown.address != address_before;
        tabs.publish();
        if address_changed {
            tabs.save();
        }
    });
}

pub(crate) fn mark_crashed(number: BrowserNumber) {
    update_tab(number, |tab| {
        tab.loading = false;
        tab.audible = false;
        tab.failure = Some(PageFailure::Crashed);
    });
}

/// Forgets a browser CEF closed, such as a page that closed itself, and quits once the last one
/// closes while the host is closing.
pub(crate) fn browser_closed(number: BrowserNumber) {
    update_tabs(move |tabs| {
        if let Some(index) = tabs.tabs.iter().position(|tab| tab.number == number) {
            let id = tabs.tabs.remove(index).shown.id;
            if tabs.selected == Some(id) {
                tabs.selected = tabs.tabs.first().map(|tab| tab.shown.id);
            }
            tabs.publish();
            if !tabs.closing {
                tabs.save();
            }
        }
        if tabs.closing && tabs.tabs.is_empty() {
            quit_message_loop();
        }
    });
}

/// Saves the tabs and closes every browser; the message loop quits once they close, or after
/// a grace period.
pub(crate) fn begin_shutdown() {
    let browsers = with_tabs(|tabs| {
        if tabs.closing {
            return None;
        }
        tabs.closing = true;
        tabs.save();
        Some(tabs.browsers())
    });
    let browsers = match browsers {
        Some(Some(browsers)) => browsers,
        Some(None) => return,
        // CEF never became ready, so there is nothing to close.
        None => Vec::new(),
    };
    tracing::info!("closing the browser");
    if browsers.is_empty() {
        quit_message_loop();
        return;
    }
    for browser in browsers {
        if let Some(browser_host) = browser.host() {
            browser_host.close_browser(1);
        }
    }
    run_on_ui_after(CLOSE_GRACE, quit_message_loop);
}
