//! The CEF application: the switches every process starts with, and the hook that runs once the
//! browser process is ready to create pages.

#![allow(
    clippy::transmute_ptr_to_ptr,
    reason = "cef-rs's wrap macros transmute the reference to each object's base"
)]

use std::sync::{Arc, Mutex, PoisonError};

use cef::{
    App, BrowserProcessHandler, CefStringUtf16, CommandLine, ImplApp, ImplBrowserProcessHandler,
    ImplCommandLine, WrapApp, WrapBrowserProcessHandler, rc::Rc as _, wrap_app,
    wrap_browser_process_handler,
};

/// Runs once, on the UI thread, when CEF can create browsers.
pub(crate) type OnContextReady = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

/// Switches without values, then switches with them. Pages render in software without a display
/// connection; their audio reaches only the audio handler; Chromium stores cookies without the
/// desktop keyring, which could prompt for a password; and nothing updates in the background.
const SWITCHES: [&str; 4] = [
    "disable-audio-output",
    "disable-gpu",
    "disable-gpu-compositing",
    "disable-component-update",
];
#[cfg(target_os = "linux")]
const VALUED_SWITCHES: [(&str, &str); 3] = [
    ("ozone-platform", "headless"),
    ("password-store", "basic"),
    ("enable-logging", "stderr"),
];
/// Windows has no display connection to skip and protects cookies without prompting. Logging to
/// standard error there opens a console for each of Chromium's processes without one, so
/// Chromium logs to a file instead.
#[cfg(windows)]
const VALUED_SWITCHES: [(&str, &str); 0] = [];

pub(crate) fn app(on_context_ready: OnContextReady) -> App {
    HostApp::new(HostProcess::new(on_context_ready))
}

wrap_app! {
    struct HostApp {
        browser_process: BrowserProcessHandler,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefStringUtf16>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else {
                return;
            };
            for switch in SWITCHES {
                command_line.append_switch(Some(&switch.into()));
            }
            for (switch, value) in VALUED_SWITCHES {
                command_line.append_switch_with_value(Some(&switch.into()), Some(&value.into()));
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(self.browser_process.clone())
        }
    }
}

wrap_browser_process_handler! {
    struct HostProcess {
        on_context_ready: OnContextReady,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            let ready = self
                .on_context_ready
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(ready) = ready {
                ready();
            }
        }
    }
}
