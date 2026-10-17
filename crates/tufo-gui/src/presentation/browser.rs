//! The browser page: whether the browser runs, its tabs, and the selected tab's controls.

use tufo_models::{
    browser::{BrowserTab, BrowserTabs, PageFailure, PageLoadError},
    playback::browser::{BrowserPlayback, BrowserStatus, BrowserStopReason},
};

use crate::i18n::tr;

const BLANK_ADDRESS: &str = "about:blank";

/// Mirrors the window's `BrowserState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BrowserStateView {
    #[default]
    Closed,
    Starting,
    Open,
    StoppedIdle,
    Crashed,
    StartFailed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserTabView {
    pub(crate) id: u32,
    pub(crate) title: String,
    pub(crate) loading: bool,
    pub(crate) audible: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "it mirrors the window's browser properties one to one"
)]
pub(crate) struct BrowserPageView {
    pub(crate) state: BrowserStateView,
    pub(crate) tabs: Vec<BrowserTabView>,
    pub(crate) selected: Option<u32>,
    /// The selected tab's address, empty for a blank tab.
    pub(crate) address: String,
    pub(crate) loading: bool,
    pub(crate) can_go_back: bool,
    pub(crate) can_go_forward: bool,
    /// Why the selected tab shows no page; empty while it shows one.
    pub(crate) failure: String,
    pub(crate) volume: f32,
    pub(crate) muted: bool,
    pub(crate) audible: bool,
}

pub(crate) fn browser_page(playback: BrowserPlayback, tabs: &BrowserTabs) -> BrowserPageView {
    let state = match playback.status {
        BrowserStatus::Closed => BrowserStateView::Closed,
        BrowserStatus::Starting => BrowserStateView::Starting,
        BrowserStatus::Open => BrowserStateView::Open,
        BrowserStatus::Stopped(BrowserStopReason::Idle) => BrowserStateView::StoppedIdle,
        BrowserStatus::Stopped(BrowserStopReason::Crashed) => BrowserStateView::Crashed,
        BrowserStatus::Stopped(BrowserStopReason::StartFailed) => BrowserStateView::StartFailed,
        BrowserStatus::Stopped(BrowserStopReason::Unavailable) => BrowserStateView::Unavailable,
    };
    let mut view = BrowserPageView {
        state,
        volume: playback.volume.into_inner(),
        muted: playback.muted,
        audible: playback.audible,
        ..BrowserPageView::default()
    };
    if state != BrowserStateView::Open {
        return view;
    }
    view.tabs = tabs.tabs.iter().map(tab_view).collect();
    let Some(selected) = tabs
        .selected
        .and_then(|id| tabs.tabs.iter().find(|tab| tab.id == id))
    else {
        return view;
    };
    view.selected = Some(selected.id.into_inner().get());
    view.address = if selected.address == BLANK_ADDRESS {
        String::new()
    } else {
        selected.address.clone()
    };
    view.loading = selected.loading;
    view.can_go_back = selected.history.can_go_back;
    view.can_go_forward = selected.history.can_go_forward;
    view.failure = selected
        .failure
        .map(|failure| failure_text(failure).to_owned())
        .unwrap_or_default();
    view
}

fn tab_view(tab: &BrowserTab) -> BrowserTabView {
    let title = if !tab.title.is_empty() && tab.title != BLANK_ADDRESS {
        tab.title.clone()
    } else if tab.address.is_empty() || tab.address == BLANK_ADDRESS {
        tr("New tab").to_owned()
    } else {
        tab.address.clone()
    };
    BrowserTabView {
        id: tab.id.into_inner().get(),
        title,
        loading: tab.loading,
        audible: tab.audible,
    }
}

fn failure_text(failure: PageFailure) -> &'static str {
    match failure {
        PageFailure::Crashed => tr("This page stopped working. Reload it to try again."),
        PageFailure::LoadFailed(error) => match error {
            PageLoadError::NameNotResolved => tr("Couldn’t find that site. Check the address."),
            PageLoadError::ConnectionFailed => tr("The site didn’t respond. Try again later."),
            PageLoadError::Offline => tr("This computer is offline."),
            PageLoadError::InvalidCertificate => {
                tr("The site’s security certificate isn’t valid, so it wasn’t opened.")
            }
            PageLoadError::Blocked => tr("The site refused to open here."),
            PageLoadError::Other => tr("This page couldn’t load."),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use tufo_models::{
        browser::{TabHistory, TabId},
        library::Volume,
    };

    use super::*;

    fn tab(id: u32, address: &str, title: &str) -> BrowserTab {
        BrowserTab {
            id: TabId::new(NonZeroU32::new(id).unwrap()),
            address: address.to_owned(),
            title: title.to_owned(),
            loading: false,
            history: TabHistory {
                can_go_back: true,
                can_go_forward: false,
            },
            audible: false,
            failure: None,
        }
    }

    fn open() -> BrowserPlayback {
        BrowserPlayback {
            status: BrowserStatus::Open,
            volume: Volume::try_new(0.5).unwrap(),
            muted: false,
            audible: true,
        }
    }

    #[test]
    fn tabs_show_their_title_their_address_or_new_tab() {
        let tabs = BrowserTabs {
            tabs: vec![
                tab(1, "https://ambience.example/", "Forest ambience"),
                tab(2, "https://music.example/", ""),
                tab(3, "about:blank", "about:blank"),
            ],
            selected: Some(TabId::new(NonZeroU32::new(3).unwrap())),
        };

        let view = browser_page(open(), &tabs);

        let titles = view
            .tabs
            .iter()
            .map(|tab| tab.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            ["Forest ambience", "https://music.example/", "New tab"]
        );
        assert_eq!(view.selected, Some(3));
        assert_eq!(view.address, "");
        assert!(view.can_go_back && !view.can_go_forward);
        assert_eq!(view.volume, 0.5);
    }

    #[test]
    fn the_selected_tabs_failure_explains_itself() {
        let mut failed = tab(1, "https://gone.example/", "");
        failed.failure = Some(PageFailure::LoadFailed(PageLoadError::NameNotResolved));
        let tabs = BrowserTabs {
            tabs: vec![failed],
            selected: Some(TabId::new(NonZeroU32::MIN)),
        };

        let view = browser_page(open(), &tabs);

        assert_eq!(view.failure, "Couldn’t find that site. Check the address.");
    }

    #[test]
    fn a_stopped_browser_shows_no_tabs() {
        let tabs = BrowserTabs {
            tabs: vec![tab(1, "https://ambience.example/", "Forest")],
            selected: Some(TabId::new(NonZeroU32::MIN)),
        };
        let playback = BrowserPlayback {
            status: BrowserStatus::Stopped(BrowserStopReason::Crashed),
            ..open()
        };

        let view = browser_page(playback, &tabs);

        assert_eq!(view.state, BrowserStateView::Crashed);
        assert!(view.tabs.is_empty() && view.selected.is_none());
    }
}
