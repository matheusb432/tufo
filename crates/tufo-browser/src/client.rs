//! What Chromium asks of the host for every tab: where and how big the view is, its paints, the
//! pointer's shape, navigation, permissions, dialogs, and the audio pages play.

#![allow(
    clippy::transmute_ptr_to_ptr,
    reason = "cef-rs's wrap macros transmute the reference to each object's base"
)]
#![allow(
    clippy::too_many_arguments,
    reason = "the client macro takes every handler as an argument"
)]

use std::{
    collections::HashMap,
    num::NonZeroU32,
    sync::{Arc, Mutex, PoisonError},
};

use cef::{
    AudioHandler, AudioParameters, Browser, BrowserSettings, CefString, ChannelLayout, Client,
    ContextMenuHandler, ContextMenuParams, CursorInfo, CursorType, DialogHandler, DictionaryValue,
    DisplayHandler, DownloadHandler, Errorcode, FileDialogCallback, FileDialogMode, Frame,
    ImplAudioHandler, ImplBrowser, ImplClient, ImplContextMenuHandler, ImplDialogHandler,
    ImplDisplayHandler, ImplDownloadHandler, ImplFileDialogCallback, ImplFrame,
    ImplJsdialogCallback, ImplJsdialogHandler, ImplLifeSpanHandler, ImplLoadHandler,
    ImplMediaAccessCallback, ImplMenuModel, ImplPermissionHandler, ImplPermissionPromptCallback,
    ImplRenderHandler, ImplRequest, ImplRequestHandler, JsdialogCallback, JsdialogHandler,
    JsdialogType, LifeSpanHandler, LoadHandler, MediaAccessCallback, MenuModel, PaintElementType,
    PermissionHandler, PermissionPromptCallback, PermissionRequestResult, PopupFeatures, Rect,
    RenderHandler, Request, RequestHandler, ScreenInfo, TerminationStatus, TransitionType,
    WindowInfo, WindowOpenDisposition, WrapAudioHandler, WrapClient, WrapContextMenuHandler,
    WrapDialogHandler, WrapDisplayHandler, WrapDownloadHandler, WrapJsdialogHandler,
    WrapLifeSpanHandler, WrapLoadHandler, WrapPermissionHandler, WrapRenderHandler,
    WrapRequestHandler, rc::Rc as _, wrap_audio_handler, wrap_client, wrap_context_menu_handler,
    wrap_dialog_handler, wrap_display_handler, wrap_download_handler, wrap_jsdialog_handler,
    wrap_life_span_handler, wrap_load_handler, wrap_permission_handler, wrap_render_handler,
    wrap_request_handler,
};
use tufo_models::browser::{PageAddress, PageFailure, PageLoadError, TabHistory};
use tufo_wire::{
    browser::{PixelRectangle, PointerShape},
    browser_host::{AudioStreamId, HostEvent},
};

use crate::{Host, tabs, ui::run_on_ui, view::BrowserNumber};

/// The audio format pages capture to: the mixer's usual rate, in 10 ms packets.
const CAPTURE_SAMPLE_RATE: i32 = 48_000;
const CAPTURE_PACKET_FRAMES: i32 = 480;
/// Schemes a page may navigate a tab's main frame to; Chromium already keeps pages from
/// navigating to the rest, and this keeps redirects and links from doing so too.
const NAVIGABLE_SCHEMES: [&str; 5] = ["http", "https", "about", "blob", "data"];
/// Chromium's certificate errors.
const CERTIFICATE_ERROR_CODES: std::ops::RangeInclusive<i32> = -299..=-200;
const HANDLED: i32 = 1;
const NOT_HANDLED: i32 = 0;

/// Creates the client every tab's browser shares.
pub(crate) fn client(host: &Arc<Host>) -> Client {
    HostClient::new(
        HostRender::new(Arc::clone(host)),
        HostDisplay::new(Arc::clone(host)),
        HostLoad::new(),
        HostRequest::new(),
        HostLifeSpan::new(),
        HostPermission::new(),
        HostDialogs::new(),
        HostFileDialogs::new(),
        HostDownloads::new(),
        HostContextMenu::new(),
        HostAudio::new(Arc::clone(host), Arc::default()),
    )
}

wrap_client! {
    struct HostClient {
        render: RenderHandler,
        display: DisplayHandler,
        load: LoadHandler,
        request: RequestHandler,
        life_span: LifeSpanHandler,
        permission: PermissionHandler,
        dialogs: JsdialogHandler,
        file_dialogs: DialogHandler,
        downloads: DownloadHandler,
        context_menu: ContextMenuHandler,
        audio: AudioHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn permission_handler(&self) -> Option<PermissionHandler> {
            Some(self.permission.clone())
        }

        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(self.dialogs.clone())
        }

        fn dialog_handler(&self) -> Option<DialogHandler> {
            Some(self.file_dialogs.clone())
        }

        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(self.downloads.clone())
        }

        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            Some(self.context_menu.clone())
        }

        fn audio_handler(&self) -> Option<AudioHandler> {
            Some(self.audio.clone())
        }
    }
}

fn number(browser: Option<&mut Browser>) -> Option<BrowserNumber> {
    browser.map(|browser| browser.identifier())
}

fn is_main_frame(frame: Option<&mut Frame>) -> bool {
    frame.is_some_and(|frame| frame.is_main() == 1)
}

fn text(value: Option<&CefString>) -> String {
    value.map(ToString::to_string).unwrap_or_default()
}

wrap_render_handler! {
    struct HostRender {
        host: Arc<Host>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            let Some(rect) = rect else {
                return;
            };
            let geometry = self.host.view.geometry();
            *rect = Rect {
                x: 0,
                y: 0,
                width: geometry.width,
                height: geometry.height,
            };
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            let Some(screen_info) = screen_info else {
                return NOT_HANDLED;
            };
            let geometry = self.host.view.geometry();
            let rect = Rect {
                x: 0,
                y: 0,
                width: geometry.width,
                height: geometry.height,
            };
            screen_info.device_scale_factor = geometry.scale_factor;
            screen_info.rect = rect.clone();
            screen_info.available_rect = rect;
            HANDLED
        }

        fn on_popup_show(&self, browser: Option<&mut Browser>, show: ::std::os::raw::c_int) {
            if show == 0
                && let Some(number) = number(browser)
            {
                self.host.view.place_popup(number, None);
            }
        }

        fn on_popup_size(&self, browser: Option<&mut Browser>, rect: Option<&Rect>) {
            let (Some(number), Some(rect)) = (number(browser), rect) else {
                return;
            };
            self.host.view.place_popup(number, pixel_rectangle(rect));
        }

        fn on_paint(
            &self,
            browser: Option<&mut Browser>,
            type_: PaintElementType,
            dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            let (Some(number), Ok(width), Ok(height)) =
                (number(browser), u32::try_from(width), u32::try_from(height))
            else {
                return;
            };
            if buffer.is_null() {
                return;
            }
            let length = width as usize * height as usize * tufo_wire::browser::PIXEL_BYTES;
            // SAFETY: CEF paints `width` × `height` BGRA pixels into `buffer` for this call.
            let pixels = unsafe { std::slice::from_raw_parts(buffer, length) };
            if type_ == PaintElementType::POPUP {
                self.host.view.paint_popup(number, width, height, pixels);
                return;
            }
            let changed = dirty_rects
                .unwrap_or_default()
                .iter()
                .filter_map(pixel_rectangle)
                .collect::<Vec<_>>();
            self.host
                .view
                .paint_page(number, width, height, pixels, &changed);
        }
    }
}

fn pixel_rectangle(rect: &Rect) -> Option<PixelRectangle> {
    Some(PixelRectangle {
        x: u32::try_from(rect.x).ok()?,
        y: u32::try_from(rect.y).ok()?,
        width: u32::try_from(rect.width).ok()?,
        height: u32::try_from(rect.height).ok()?,
    })
}

wrap_display_handler! {
    struct HostDisplay {
        host: Arc<Host>,
    }

    impl DisplayHandler {
        fn on_address_change(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            let Some(number) = number(browser) else {
                return;
            };
            if !is_main_frame(frame) {
                return;
            }
            let address = text(url);
            tabs::update_tab(number, move |tab| tab.address = address);
        }

        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let Some(number) = number(browser) else {
                return;
            };
            let title = text(title);
            tabs::update_tab(number, move |tab| tab.title = title);
        }

        fn on_cursor_change(
            &self,
            browser: Option<&mut Browser>,
            _cursor: ::std::os::raw::c_ulong,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> ::std::os::raw::c_int {
            if let Some(number) = number(browser) {
                self.host.view.set_pointer_shape(number, pointer_shape(type_));
            }
            HANDLED
        }
    }
}

fn pointer_shape(cursor: CursorType) -> PointerShape {
    let shapes = [
        (CursorType::HAND, PointerShape::Hand),
        (CursorType::IBEAM, PointerShape::Text),
        (CursorType::VERTICALTEXT, PointerShape::Text),
        (CursorType::WAIT, PointerShape::Wait),
        (CursorType::PROGRESS, PointerShape::Progress),
        (CursorType::CROSS, PointerShape::Crosshair),
        (CursorType::CELL, PointerShape::Crosshair),
        (CursorType::MOVE, PointerShape::Move),
        (CursorType::MIDDLEPANNING, PointerShape::Move),
        (CursorType::NOTALLOWED, PointerShape::NotAllowed),
        (CursorType::NODROP, PointerShape::NotAllowed),
        (CursorType::GRAB, PointerShape::Grab),
        (CursorType::GRABBING, PointerShape::Grabbing),
        (CursorType::HELP, PointerShape::Help),
        (CursorType::EASTRESIZE, PointerShape::ColumnResize),
        (CursorType::WESTRESIZE, PointerShape::ColumnResize),
        (CursorType::EASTWESTRESIZE, PointerShape::ColumnResize),
        (CursorType::COLUMNRESIZE, PointerShape::ColumnResize),
        (CursorType::NORTHRESIZE, PointerShape::RowResize),
        (CursorType::SOUTHRESIZE, PointerShape::RowResize),
        (CursorType::NORTHSOUTHRESIZE, PointerShape::RowResize),
        (CursorType::ROWRESIZE, PointerShape::RowResize),
        (CursorType::NORTHEASTRESIZE, PointerShape::NeswResize),
        (CursorType::SOUTHWESTRESIZE, PointerShape::NeswResize),
        (
            CursorType::NORTHEASTSOUTHWESTRESIZE,
            PointerShape::NeswResize,
        ),
        (CursorType::NORTHWESTRESIZE, PointerShape::NwseResize),
        (CursorType::SOUTHEASTRESIZE, PointerShape::NwseResize),
        (
            CursorType::NORTHWESTSOUTHEASTRESIZE,
            PointerShape::NwseResize,
        ),
        (CursorType::NONE, PointerShape::None),
    ];
    shapes
        .into_iter()
        .find(|(candidate, _)| *candidate == cursor)
        .map_or(PointerShape::Default, |(_, shape)| shape)
}

wrap_load_handler! {
    struct HostLoad;

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            let Some(number) = number(browser) else {
                return;
            };
            tabs::update_tab(number, move |tab| {
                tab.loading = is_loading != 0;
                tab.history = TabHistory {
                    can_go_back: can_go_back != 0,
                    can_go_forward: can_go_forward != 0,
                };
            });
        }

        fn on_load_start(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _transition_type: TransitionType,
        ) {
            let Some(number) = number(browser) else {
                return;
            };
            if is_main_frame(frame) {
                tabs::update_tab(number, |tab| tab.failure = None);
            }
        }

        fn on_load_error(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            _error_text: Option<&CefString>,
            _failed_url: Option<&CefString>,
        ) {
            let Some(number) = number(browser) else {
                return;
            };
            if !is_main_frame(frame) || error_code == Errorcode::ABORTED {
                return;
            }
            let error = load_error(error_code);
            tabs::update_tab(number, move |tab| {
                tab.failure = Some(PageFailure::LoadFailed(error));
            });
        }
    }
}

fn load_error(code: Errorcode) -> PageLoadError {
    if code == Errorcode::NAME_NOT_RESOLVED {
        PageLoadError::NameNotResolved
    } else if code == Errorcode::INTERNET_DISCONNECTED {
        PageLoadError::Offline
    } else if [
        Errorcode::CONNECTION_REFUSED,
        Errorcode::CONNECTION_RESET,
        Errorcode::CONNECTION_CLOSED,
        Errorcode::CONNECTION_TIMED_OUT,
        Errorcode::TIMED_OUT,
        Errorcode::ADDRESS_UNREACHABLE,
    ]
    .contains(&code)
    {
        PageLoadError::ConnectionFailed
    } else if [Errorcode::BLOCKED_BY_CLIENT, Errorcode::BLOCKED_BY_RESPONSE].contains(&code) {
        PageLoadError::Blocked
    } else if CERTIFICATE_ERROR_CODES.contains(&(*code.as_ref() as i32)) {
        PageLoadError::InvalidCertificate
    } else {
        PageLoadError::Other
    }
}

wrap_request_handler! {
    struct HostRequest;

    impl RequestHandler {
        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: ::std::os::raw::c_int,
            _is_redirect: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            if !is_main_frame(frame) {
                return NOT_HANDLED;
            }
            let address = request.map(|request| CefString::from(&request.url()).to_string());
            let navigable = address.as_deref().is_some_and(|address| {
                address
                    .split_once(':')
                    .is_some_and(|(scheme, _)| NAVIGABLE_SCHEMES.contains(&scheme))
            });
            if navigable { NOT_HANDLED } else { HANDLED }
        }

        fn on_render_process_terminated(
            &self,
            browser: Option<&mut Browser>,
            _status: TerminationStatus,
            _error_code: ::std::os::raw::c_int,
            _error_string: Option<&CefString>,
        ) {
            if let Some(number) = number(browser) {
                tracing::warn!(number, "a page's renderer stopped");
                tabs::mark_crashed(number);
            }
        }
    }
}

wrap_life_span_handler! {
    struct HostLifeSpan;

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            if let Ok(address) = PageAddress::parse(&text(target_url)) {
                run_on_ui(move || {
                    if let Err(error) = tabs::open_tab(Some(&address)) {
                        tracing::warn!(%error, "cannot open a page's new window as a tab");
                    }
                });
            }
            HANDLED
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(number) = number(browser) {
                tabs::browser_closed(number);
            }
        }
    }
}

wrap_permission_handler! {
    struct HostPermission;

    impl PermissionHandler {
        fn on_request_media_access_permission(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _requesting_origin: Option<&CefString>,
            _requested_permissions: u32,
            callback: Option<&mut MediaAccessCallback>,
        ) -> ::std::os::raw::c_int {
            if let Some(callback) = callback {
                callback.cancel();
            }
            HANDLED
        }

        fn on_show_permission_prompt(
            &self,
            _browser: Option<&mut Browser>,
            _prompt_id: u64,
            _requesting_origin: Option<&CefString>,
            _requested_permissions: u32,
            callback: Option<&mut PermissionPromptCallback>,
        ) -> ::std::os::raw::c_int {
            if let Some(callback) = callback {
                callback.cont(PermissionRequestResult::DENY);
            }
            HANDLED
        }
    }
}

wrap_jsdialog_handler! {
    struct HostDialogs;

    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            _origin_url: Option<&CefString>,
            _dialog_type: JsdialogType,
            _message_text: Option<&CefString>,
            _default_prompt_text: Option<&CefString>,
            _callback: Option<&mut JsdialogCallback>,
            suppress_message: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            if let Some(suppress_message) = suppress_message {
                *suppress_message = 1;
            }
            NOT_HANDLED
        }

        fn on_before_unload_dialog(
            &self,
            _browser: Option<&mut Browser>,
            _message_text: Option<&CefString>,
            _is_reload: ::std::os::raw::c_int,
            callback: Option<&mut JsdialogCallback>,
        ) -> ::std::os::raw::c_int {
            if let Some(callback) = callback {
                callback.cont(1, None);
            }
            HANDLED
        }
    }
}

wrap_dialog_handler! {
    struct HostFileDialogs;

    impl DialogHandler {
        fn on_file_dialog(
            &self,
            _browser: Option<&mut Browser>,
            _mode: FileDialogMode,
            _title: Option<&CefString>,
            _default_file_path: Option<&CefString>,
            _accept_filters: Option<&mut cef::CefStringList>,
            _accept_extensions: Option<&mut cef::CefStringList>,
            _accept_descriptions: Option<&mut cef::CefStringList>,
            callback: Option<&mut FileDialogCallback>,
        ) -> ::std::os::raw::c_int {
            if let Some(callback) = callback {
                callback.cancel();
            }
            HANDLED
        }
    }
}

wrap_download_handler! {
    struct HostDownloads;

    impl DownloadHandler {
        fn can_download(
            &self,
            _browser: Option<&mut Browser>,
            _url: Option<&CefString>,
            _request_method: Option<&CefString>,
        ) -> ::std::os::raw::c_int {
            0
        }
    }
}

wrap_context_menu_handler! {
    struct HostContextMenu;

    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            if let Some(model) = model {
                model.clear();
            }
        }
    }
}

/// The channel count of each playing stream, by browser.
type StreamChannels = Arc<Mutex<HashMap<BrowserNumber, usize>>>;

wrap_audio_handler! {
    struct HostAudio {
        host: Arc<Host>,
        channels: StreamChannels,
    }

    impl AudioHandler {
        fn audio_parameters(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&mut AudioParameters>,
        ) -> ::std::os::raw::c_int {
            if let Some(params) = params {
                params.sample_rate = CAPTURE_SAMPLE_RATE;
                params.channel_layout = ChannelLayout::LAYOUT_STEREO;
                params.frames_per_buffer = CAPTURE_PACKET_FRAMES;
            }
            1
        }

        fn on_audio_stream_started(
            &self,
            browser: Option<&mut Browser>,
            params: Option<&AudioParameters>,
            channels: ::std::os::raw::c_int,
        ) {
            let (Some(number), Some(params)) = (number(browser), params) else {
                return;
            };
            let (Some(stream), Some(sample_rate), Some(channel_count)) = (
                stream_id(number),
                u32::try_from(params.sample_rate).ok().and_then(NonZeroU32::new),
                u32::try_from(channels).ok().and_then(NonZeroU32::new),
            ) else {
                return;
            };
            self.channels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(number, channel_count.get() as usize);
            self.host.events.send(HostEvent::AudioStarted {
                stream,
                sample_rate,
                channels: channel_count,
            });
            tabs::update_tab(number, |tab| tab.audible = true);
        }

        fn on_audio_stream_packet(
            &self,
            browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: ::std::os::raw::c_int,
            _pts: i64,
        ) {
            let (Some(number), Ok(frames)) = (number(browser), usize::try_from(frames)) else {
                return;
            };
            let channels = self
                .channels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&number)
                .copied();
            let (Some(stream), Some(channels)) = (stream_id(number), channels) else {
                return;
            };
            if data.is_null() {
                return;
            }
            // SAFETY: CEF passes one pointer per channel of the stream it started.
            let planes = unsafe { std::slice::from_raw_parts(data.cast_const(), channels) };
            let mut samples = Vec::with_capacity(frames * channels);
            for frame in 0..frames {
                for plane in planes {
                    // SAFETY: each plane holds `frames` samples for this call.
                    samples.push(unsafe { *plane.add(frame) });
                }
            }
            self.host
                .events
                .send(HostEvent::AudioPacket { stream, samples });
        }

        fn on_audio_stream_stopped(&self, browser: Option<&mut Browser>) {
            if let Some(number) = number(browser) {
                self.stop(number);
            }
        }

        fn on_audio_stream_error(&self, browser: Option<&mut Browser>, message: Option<&CefString>) {
            if let Some(number) = number(browser) {
                tracing::warn!(number, message = text(message), "a page's audio capture failed");
                self.stop(number);
            }
        }
    }
}

impl HostAudio {
    fn stop(&self, number: BrowserNumber) {
        let started = self
            .channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&number)
            .is_some();
        if let (true, Some(stream)) = (started, stream_id(number)) {
            self.host.events.send(HostEvent::AudioStopped { stream });
            tabs::update_tab(number, |tab| tab.audible = false);
        }
    }
}

fn stream_id(number: BrowserNumber) -> Option<AudioStreamId> {
    u32::try_from(number)
        .ok()
        .and_then(NonZeroU32::new)
        .map(AudioStreamId)
}
