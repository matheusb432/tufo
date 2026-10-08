//! `BrowserService` on the host's private endpoint: the GUI watches the tabs, drives them, and
//! attaches its view. Every change to a browser runs on CEF's UI thread.

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tufo_models::browser::BrowserTabs;
use tufo_wire::{
    browser::ViewInput,
    pb::browser::{
        CloseTabRequest, CloseTabResponse, GoBackRequest, GoBackResponse, GoForwardRequest,
        GoForwardResponse, NavigateTabRequest, NavigateTabResponse, OpenTabRequest,
        OpenTabResponse, ReloadTabRequest, ReloadTabResponse, SelectTabRequest, SelectTabResponse,
        StopLoadingTabRequest, StopLoadingTabResponse, ViewBrowserRequest, ViewBrowserResponse,
        WatchBrowserRequest, WatchBrowserResponse, browser_service_server::BrowserService,
    },
    proto,
};

use crate::{
    Host,
    tabs::{self, OpenTabError, TabNotFound},
    ui::{call_on_ui, run_on_ui},
    view::ViewerId,
};

pub(crate) struct BrowserGrpcService {
    host: Arc<Host>,
    tabs: watch::Receiver<BrowserTabs>,
    last_viewer: AtomicU64,
}

impl BrowserGrpcService {
    pub(crate) fn new(host: Arc<Host>, tabs: watch::Receiver<BrowserTabs>) -> Self {
        Self {
            host,
            tabs,
            last_viewer: AtomicU64::new(0),
        }
    }
}

fn closing(_: crate::ui::BrowserClosing) -> Status {
    Status::unavailable("the browser is closing")
}

fn tab_not_found(_: TabNotFound) -> Status {
    Status::not_found("no such tab")
}

/// Runs a tab action on the UI thread.
async fn on_tab(
    action: impl FnOnce() -> Result<(), TabNotFound> + Send + 'static,
) -> Result<(), Status> {
    call_on_ui(action)
        .await
        .map_err(closing)?
        .map_err(tab_not_found)
}

#[tonic::async_trait]
impl BrowserService for BrowserGrpcService {
    type WatchBrowserStream = ReceiverStream<Result<WatchBrowserResponse, Status>>;
    type ViewBrowserStream = ReceiverStream<Result<ViewBrowserResponse, Status>>;

    async fn watch_browser(
        &self,
        _request: Request<WatchBrowserRequest>,
    ) -> Result<Response<Self::WatchBrowserStream>, Status> {
        // One slot: a slow reader holds the forwarder, which then sends the latest tabs.
        let (sender, receiver) = mpsc::channel(1);
        tokio::spawn(forward_tabs(self.tabs.clone(), sender));
        Ok(Response::new(ReceiverStream::new(receiver)))
    }

    async fn view_browser(
        &self,
        request: Request<Streaming<ViewBrowserRequest>>,
    ) -> Result<Response<Self::ViewBrowserStream>, Status> {
        let mut inputs = request.into_inner();
        let first = inputs
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("the view sent nothing"))?;
        let ViewInput::Resize(size) = proto::browser::view_browser_request(first)? else {
            return Err(Status::invalid_argument(
                "the first request must give the view's size",
            ));
        };
        let viewer = self.last_viewer.fetch_add(1, Ordering::Relaxed) + 1;
        call_on_ui(move || tabs::attach_view(viewer, size))
            .await
            .map_err(closing)?;
        let (sender, receiver) = mpsc::channel(2);
        tokio::spawn(forward_inputs(inputs, viewer));
        tokio::spawn(forward_view(Arc::clone(&self.host), viewer, sender));
        Ok(Response::new(ReceiverStream::new(receiver)))
    }

    async fn open_tab(
        &self,
        request: Request<OpenTabRequest>,
    ) -> Result<Response<OpenTabResponse>, Status> {
        let address = proto::browser::open_tab_request(request.get_ref())?;
        let opened = call_on_ui(move || tabs::open_tab(address.as_ref()))
            .await
            .map_err(closing)?;
        let tab = opened.map_err(|error| match error {
            OpenTabError::TooMany => Status::failed_precondition(error.to_string()),
            OpenTabError::Closing | OpenTabError::NotCreated => {
                Status::unavailable(error.to_string())
            }
        })?;
        Ok(Response::new(OpenTabResponse {
            tab_id: tab.into_inner().get(),
        }))
    }

    async fn close_tab(
        &self,
        request: Request<CloseTabRequest>,
    ) -> Result<Response<CloseTabResponse>, Status> {
        let tab = proto::browser::tab_id(request.get_ref().tab_id)?;
        call_on_ui(move || tabs::close_tab(tab))
            .await
            .map_err(closing)?;
        Ok(Response::new(CloseTabResponse {}))
    }

    async fn select_tab(
        &self,
        request: Request<SelectTabRequest>,
    ) -> Result<Response<SelectTabResponse>, Status> {
        let tab = proto::browser::tab_id(request.get_ref().tab_id)?;
        on_tab(move || tabs::select_tab(tab)).await?;
        Ok(Response::new(SelectTabResponse {}))
    }

    async fn navigate_tab(
        &self,
        request: Request<NavigateTabRequest>,
    ) -> Result<Response<NavigateTabResponse>, Status> {
        let (tab, address) = proto::browser::navigate_tab_request(request.get_ref())?;
        on_tab(move || tabs::navigate_tab(tab, &address)).await?;
        Ok(Response::new(NavigateTabResponse {}))
    }

    async fn go_back(
        &self,
        request: Request<GoBackRequest>,
    ) -> Result<Response<GoBackResponse>, Status> {
        let tab = proto::browser::tab_id(request.get_ref().tab_id)?;
        on_tab(move || tabs::with_browser(tab, cef::ImplBrowser::go_back)).await?;
        Ok(Response::new(GoBackResponse {}))
    }

    async fn go_forward(
        &self,
        request: Request<GoForwardRequest>,
    ) -> Result<Response<GoForwardResponse>, Status> {
        let tab = proto::browser::tab_id(request.get_ref().tab_id)?;
        on_tab(move || tabs::with_browser(tab, cef::ImplBrowser::go_forward)).await?;
        Ok(Response::new(GoForwardResponse {}))
    }

    async fn reload_tab(
        &self,
        request: Request<ReloadTabRequest>,
    ) -> Result<Response<ReloadTabResponse>, Status> {
        let tab = proto::browser::tab_id(request.get_ref().tab_id)?;
        on_tab(move || tabs::with_browser(tab, cef::ImplBrowser::reload)).await?;
        Ok(Response::new(ReloadTabResponse {}))
    }

    async fn stop_loading_tab(
        &self,
        request: Request<StopLoadingTabRequest>,
    ) -> Result<Response<StopLoadingTabResponse>, Status> {
        let tab = proto::browser::tab_id(request.get_ref().tab_id)?;
        on_tab(move || tabs::with_browser(tab, cef::ImplBrowser::stop_load)).await?;
        Ok(Response::new(StopLoadingTabResponse {}))
    }
}

async fn forward_tabs(
    mut tabs: watch::Receiver<BrowserTabs>,
    sender: mpsc::Sender<Result<WatchBrowserResponse, Status>>,
) {
    loop {
        let response = proto::browser::watch_browser_response(&tabs.borrow_and_update());
        if !send_or_closed(&sender, response, tabs.changed()).await {
            return;
        }
    }
}

/// Sends `response`, then waits for `next`; false once the reader or the source is gone.
async fn send_or_closed<T, E>(
    sender: &mpsc::Sender<Result<T, Status>>,
    response: T,
    next: impl Future<Output = Result<(), E>>,
) -> bool {
    if sender.send(Ok(response)).await.is_err() {
        return false;
    }
    tokio::select! {
        () = sender.closed() => false,
        changed = next => changed.is_ok(),
    }
}

/// Hands the view's input to the UI thread, and detaches the view once its stream ends.
async fn forward_inputs(mut inputs: Streaming<ViewBrowserRequest>, viewer: ViewerId) {
    while let Ok(Some(request)) = inputs.message().await {
        match proto::browser::view_browser_request(request) {
            Ok(ViewInput::Resize(size)) => run_on_ui(move || tabs::resize_view(viewer, size)),
            Ok(input) => run_on_ui(move || tabs::apply_input(viewer, input)),
            Err(status) => {
                tracing::warn!(%status, "the view sent invalid input");
                break;
            }
        }
    }
    run_on_ui(move || tabs::detach_view(viewer));
}

/// Sends the view what changed, until a newer view replaces it or it disconnects.
async fn forward_view(
    host: Arc<Host>,
    viewer: ViewerId,
    sender: mpsc::Sender<Result<ViewBrowserResponse, Status>>,
) {
    let mut changes = host.view.subscribe();
    loop {
        let Ok(outputs) = host.view.take_output(viewer) else {
            return;
        };
        for output in outputs {
            let response = proto::browser::view_browser_response(output);
            if sender.send(Ok(response)).await.is_err() {
                return;
            }
        }
        tokio::select! {
            () = sender.closed() => return,
            next_change = changes.changed() => if next_change.is_err() { return },
        }
    }
}
