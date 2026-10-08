//! Typed calls to the browser host, which the server starts on demand and which serves its own
//! private endpoint while it runs.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_stream::{StreamExt as _, wrappers::ReceiverStream};
use tonic::{Streaming, transport::Channel};
use tufo_local_transport::{ConnectError, LocalEndpoint};
use tufo_models::browser::{BrowserTabs, PageAddress, TabId};
use tufo_wire::{
    browser::{PIXEL_BYTES, VIEW_PHYSICAL_PIXELS_MAX, ViewInput, ViewOutput},
    pb::browser::{
        CloseTabRequest, GoBackRequest, GoForwardRequest, ReloadTabRequest, SelectTabRequest,
        StopLoadingTabRequest, ViewBrowserResponse, WatchBrowserRequest, WatchBrowserResponse,
        browser_service_client::BrowserServiceClient,
    },
    proto,
};

use crate::{ClientError, with_timeout};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RESPONSE_START_TIMEOUT: Duration = Duration::from_secs(10);
const QUICK_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// A frame covering the largest view, with room for its message.
const VIEW_RESPONSE_SIZE_MAX: usize =
    VIEW_PHYSICAL_PIXELS_MAX as usize * VIEW_PHYSICAL_PIXELS_MAX as usize * PIXEL_BYTES + 64 * 1024;

#[derive(Debug, Clone)]
pub struct BrowserClient {
    browser: BrowserServiceClient<Channel>,
}

impl BrowserClient {
    /// Connects on first use; calls fail while no browser runs.
    pub fn connect_lazy(endpoint: &LocalEndpoint) -> Result<Self, ConnectError> {
        let channel = endpoint.connect_lazy(CONNECT_TIMEOUT, RESPONSE_START_TIMEOUT)?;
        Ok(Self {
            browser: BrowserServiceClient::new(channel)
                .max_decoding_message_size(VIEW_RESPONSE_SIZE_MAX),
        })
    }

    /// Streams the tabs, then each change, until the browser closes.
    pub async fn watch_browser(&self) -> Result<BrowserTabStates, ClientError> {
        let stream = self
            .browser
            .clone()
            .watch_browser(WatchBrowserRequest {})
            .await?
            .into_inner();
        Ok(BrowserTabStates { stream })
    }

    /// Shows the selected tab until the returned view drops or `inputs` closes. The first input
    /// must be the view's size.
    pub async fn view_browser(
        &self,
        inputs: mpsc::Receiver<ViewInput>,
    ) -> Result<BrowserView, ClientError> {
        let requests =
            ReceiverStream::new(inputs).map(|input| proto::browser::view_browser_message(&input));
        let stream = self
            .browser
            .clone()
            .view_browser(requests)
            .await?
            .into_inner();
        Ok(BrowserView { stream })
    }

    /// Opens a tab after the others, blank without an address, and selects it.
    pub async fn open_tab(&self, address: Option<&PageAddress>) -> Result<TabId, ClientError> {
        let request = with_timeout(
            proto::browser::open_tab_message(address),
            QUICK_REQUEST_TIMEOUT,
        );
        let response = self.browser.clone().open_tab(request).await?.into_inner();
        Ok(proto::browser::opened_tab(response)?)
    }

    pub async fn close_tab(&self, tab: TabId) -> Result<(), ClientError> {
        let request = with_timeout(
            CloseTabRequest {
                tab_id: tab_number(tab),
            },
            QUICK_REQUEST_TIMEOUT,
        );
        self.browser.clone().close_tab(request).await?;
        Ok(())
    }

    pub async fn select_tab(&self, tab: TabId) -> Result<(), ClientError> {
        let request = with_timeout(
            SelectTabRequest {
                tab_id: tab_number(tab),
            },
            QUICK_REQUEST_TIMEOUT,
        );
        self.browser.clone().select_tab(request).await?;
        Ok(())
    }

    pub async fn navigate_tab(&self, tab: TabId, address: &PageAddress) -> Result<(), ClientError> {
        let request = with_timeout(
            proto::browser::navigate_tab_message(tab, address),
            QUICK_REQUEST_TIMEOUT,
        );
        self.browser.clone().navigate_tab(request).await?;
        Ok(())
    }

    pub async fn go_back(&self, tab: TabId) -> Result<(), ClientError> {
        let request = with_timeout(
            GoBackRequest {
                tab_id: tab_number(tab),
            },
            QUICK_REQUEST_TIMEOUT,
        );
        self.browser.clone().go_back(request).await?;
        Ok(())
    }

    pub async fn go_forward(&self, tab: TabId) -> Result<(), ClientError> {
        let request = with_timeout(
            GoForwardRequest {
                tab_id: tab_number(tab),
            },
            QUICK_REQUEST_TIMEOUT,
        );
        self.browser.clone().go_forward(request).await?;
        Ok(())
    }

    pub async fn reload_tab(&self, tab: TabId) -> Result<(), ClientError> {
        let request = with_timeout(
            ReloadTabRequest {
                tab_id: tab_number(tab),
            },
            QUICK_REQUEST_TIMEOUT,
        );
        self.browser.clone().reload_tab(request).await?;
        Ok(())
    }

    pub async fn stop_loading_tab(&self, tab: TabId) -> Result<(), ClientError> {
        let request = with_timeout(
            StopLoadingTabRequest {
                tab_id: tab_number(tab),
            },
            QUICK_REQUEST_TIMEOUT,
        );
        self.browser.clone().stop_loading_tab(request).await?;
        Ok(())
    }
}

fn tab_number(tab: TabId) -> u32 {
    tab.into_inner().get()
}

/// The browser's tabs as the host publishes them.
pub struct BrowserTabStates {
    stream: Streaming<WatchBrowserResponse>,
}

impl BrowserTabStates {
    /// Waits for the next state; `None` once the browser ends the stream.
    pub async fn next(&mut self) -> Option<Result<BrowserTabs, ClientError>> {
        match self.stream.message().await {
            Ok(Some(response)) => {
                Some(proto::browser::browser_tabs(response).map_err(ClientError::from))
            }
            Ok(None) => None,
            Err(status) => Some(Err(ClientError::Status(status))),
        }
    }
}

/// Pixels and pointer shapes for an attached view.
pub struct BrowserView {
    stream: Streaming<ViewBrowserResponse>,
}

impl BrowserView {
    /// Waits for the next output; `None` once the browser ends the view.
    pub async fn next(&mut self) -> Option<Result<ViewOutput, ClientError>> {
        match self.stream.message().await {
            Ok(Some(response)) => {
                Some(proto::browser::view_output(response).map_err(ClientError::from))
            }
            Ok(None) => None,
            Err(status) => Some(Err(ClientError::Status(status))),
        }
    }
}
