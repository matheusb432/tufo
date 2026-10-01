//! Shows the window when the Dock or Finder opens the app while it runs: macOS then sends the
//! running app a reopen Apple Event instead of starting another process. winit owns the
//! application delegate that would hear it, so a handler of its own answers the event.

use std::time::Duration;

use objc2::{
    DefinedClass as _, MainThreadMarker, MainThreadOnly, class, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject},
    sel,
};

use crate::{WindowRequests, tray::TrayCommand};

/// `kCoreEventClass`.
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
/// `kAEReopenApplication`.
const REOPEN_APPLICATION_EVENT_ID: u32 = u32::from_be_bytes(*b"rapp");

struct ReopenHandlerIvars {
    on_reopen: Box<dyn Fn()>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class implements no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ReopenHandlerIvars]
    struct ReopenHandler;

    impl ReopenHandler {
        #[unsafe(method(handleReopen:withReplyEvent:))]
        fn handle_reopen(&self, _event: &AnyObject, _reply: &AnyObject) {
            (self.ivars().on_reopen)();
        }
    }
);

/// Shows the window at each reopen, from when the event loop runs and `AppKit` has installed its
/// own handler.
pub(crate) fn show_window_on_reopen(requests: WindowRequests) {
    slint::Timer::single_shot(Duration::ZERO, move || {
        install(move || requests.send(TrayCommand::ShowWindow));
    });
}

fn install(on_reopen: impl Fn() + 'static) {
    let Some(main_thread) = MainThreadMarker::new() else {
        tracing::warn!("reopening the app won't show its window: the UI isn't on the main thread");
        return;
    };
    let handler = ReopenHandler::alloc(main_thread).set_ivars(ReopenHandlerIvars {
        on_reopen: Box::new(on_reopen),
    });
    // SAFETY: NSObject's init takes no arguments.
    let handler: Retained<ReopenHandler> = unsafe { msg_send![super(handler), init] };
    // SAFETY: the class method takes no arguments and returns the shared manager.
    let manager: Retained<AnyObject> =
        unsafe { msg_send![class!(NSAppleEventManager), sharedAppleEventManager] };
    // SAFETY: the selector names the handler's method, which takes the event and its reply.
    unsafe {
        let () = msg_send![
            &manager,
            setEventHandler: &*handler,
            andSelector: sel!(handleReopen:withReplyEvent:),
            forEventClass: CORE_EVENT_CLASS,
            andEventID: REOPEN_APPLICATION_EVENT_ID,
        ];
    }
    // The manager doesn't retain its handlers; this one serves until the process exits.
    std::mem::forget(handler);
}
