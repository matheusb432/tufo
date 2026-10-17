//! Runs a host process: Chromium's own processes return once CEF ran them, and the browser
//! process serves views until the server closes its input.

use std::{
    io::Read as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Context as _;
use cef::{CefString, ImplCommandLine as _, LogSeverity, Settings, args::Args};
use clap::Parser;
use tokio::sync::{oneshot, watch};
use tufo_local_transport::{LocalEndpoint, LocalListener};
use tufo_models::browser::BrowserTabs;
use tufo_wire::{
    browser::{PIXEL_BYTES, VIEW_PHYSICAL_PIXELS_MAX},
    browser_host::HostEvent,
    pb::browser::browser_service_server::BrowserServiceServer,
};

use crate::{app, client, events, service::BrowserGrpcService, tabs, ui, view::ViewHub};

const ENDPOINT_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// A frame covering the largest view, with room for its message.
const VIEW_RESPONSE_BYTES_MAX: usize =
    VIEW_PHYSICAL_PIXELS_MAX as usize * VIEW_PHYSICAL_PIXELS_MAX as usize * PIXEL_BYTES + 64 * 1024;
const VIEW_REQUEST_BYTES_MAX: usize = 1024 * 1024;
/// Chromium's ICU data, which marks a directory holding CEF's runtime files.
const RUNTIME_MARKER_FILE_NAME: &str = "icudtl.dat";
const LOCALES_FOLDER_NAME: &str = "locales";
/// Chromium's log on Windows, which hands it to Chromium's other processes by handle.
#[cfg(windows)]
const CHROMIUM_LOG_FILE_NAME: &str = "chromium.log";
const SERVER_THREADS: usize = 2;
const EXIT_SUCCESS: i32 = 0;
const EXIT_FAILURE: i32 = 1;

/// What every thread shares: the attached view and the server's event pipe.
pub(crate) struct Host {
    pub(crate) view: ViewHub,
    pub(crate) events: events::HostEvents,
}

/// The browser host; tufo-server starts it.
#[derive(Parser)]
#[command(version, about)]
struct Arguments {
    /// The folder for Chromium's profile and the saved tabs.
    #[arg(long, value_name = "PATH")]
    profile: PathBuf,
}

/// Runs this process as CEF asks and returns its exit code. `windows_sandbox_info` is what
/// Windows' bootstrap executable set up for Chromium's sandbox, and null elsewhere.
pub(crate) fn run(windows_sandbox_info: *mut u8) -> i32 {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = Args::new();
    let is_subprocess = args
        .as_cmd_line()
        .is_some_and(|line| line.has_switch(Some(&CefString::from("type"))) == 1);
    let on_context_ready = app::OnContextReady::default();
    let mut cef_app = app::app(Arc::clone(&on_context_ready));
    let subprocess_exit = cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut cef_app),
        windows_sandbox_info,
    );
    if is_subprocess {
        return subprocess_exit;
    }
    let arguments = Arguments::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match serve(
        &arguments,
        &args,
        &mut cef_app,
        &on_context_ready,
        windows_sandbox_info,
    ) {
        Ok(()) => EXIT_SUCCESS,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "the browser host failed");
            EXIT_FAILURE
        }
    }
}

fn serve(
    arguments: &Arguments,
    args: &Args,
    cef_app: &mut cef::App,
    on_context_ready: &app::OnContextReady,
    windows_sandbox_info: *mut u8,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&arguments.profile).context("creating the browser profile")?;
    let profile = resolve_profile(&arguments.profile).context("resolving the browser profile")?;
    let events = events::start().context("taking standard output for the events")?;
    let runtime = runtime_directory().context("finding CEF's runtime files")?;
    let settings = Settings {
        windowless_rendering_enabled: 1,
        root_cache_path: path_text(&profile).as_str().into(),
        cache_path: path_text(&profile).as_str().into(),
        persist_session_cookies: 1,
        resources_dir_path: path_text(&runtime).as_str().into(),
        locales_dir_path: path_text(&runtime.join(LOCALES_FOLDER_NAME))
            .as_str()
            .into(),
        log_severity: LogSeverity::WARNING,
        ..Settings::default()
    };
    #[cfg(windows)]
    let settings = Settings {
        log_file: path_text(&profile.join(CHROMIUM_LOG_FILE_NAME))
            .as_str()
            .into(),
        ..settings
    };
    let host = Arc::new(Host {
        view: ViewHub::default(),
        events,
    });
    let (published, tabs_receiver) = watch::channel(BrowserTabs::default());
    let (context_ready, context_ready_receiver) = oneshot::channel();
    *on_context_ready
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new({
        let host = Arc::clone(&host);
        let profile = profile.clone();
        move || {
            tabs::install(tabs::Tabs::new(
                Arc::clone(&host),
                client::client(&host),
                profile.clone(),
                published,
            ));
            tabs::restore_session(&profile);
            let _ = context_ready.send(());
        }
    }));
    // CEF can report its context ready from inside initialize, so the hook comes first.
    if cef::initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(cef_app),
        windows_sandbox_info,
    ) != 1
    {
        anyhow::bail!("CEF did not initialize");
    }
    let failed = Arc::new(AtomicBool::new(false));
    let server = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(SERVER_THREADS)
        .thread_name("tufo-browser-server")
        .enable_all()
        .build()
        .context("starting the server threads")?;
    server.spawn(serve_views(
        Arc::clone(&host),
        tabs_receiver,
        context_ready_receiver,
        Arc::clone(&failed),
    ));
    close_when_input_closes()?;
    cef::run_message_loop();
    tabs::uninstall();
    server.shutdown_timeout(Duration::from_secs(1));
    cef::shutdown();
    tracing::info!("the browser closed");
    if failed.load(Ordering::Relaxed) {
        anyhow::bail!("the browser stopped serving views");
    }
    Ok(())
}

#[cfg(not(windows))]
fn resolve_profile(profile: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(profile)
}

/// Windows' canonical paths carry the `\\?\` prefix; Chromium takes plain absolute ones.
#[cfg(windows)]
fn resolve_profile(profile: &Path) -> std::io::Result<PathBuf> {
    std::path::absolute(profile)
}

/// Beside this executable once installed; else the unpacked CEF directory it was built with.
fn runtime_directory() -> anyhow::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let beside = executable
        .parent()
        .context("the executable has no folder")?
        .to_path_buf();
    if beside.join(RUNTIME_MARKER_FILE_NAME).is_file() {
        return Ok(beside);
    }
    option_env!("TUFO_CEF_BUILD_DIRECTORY")
        .map(PathBuf::from)
        .filter(|directory| directory.join(RUNTIME_MARKER_FILE_NAME).is_file())
        .context("CEF's runtime files are neither beside the host nor where it was built")
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The server closes the host's input to end it, or by exiting.
fn close_when_input_closes() -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("tufo-browser-input".to_owned())
        .spawn(|| {
            let mut ignored = [0; 64];
            let mut input = std::io::stdin().lock();
            while input.read(&mut ignored).is_ok_and(|read| read > 0) {}
            ui::run_on_ui(tabs::begin_shutdown);
        })
        .context("watching standard input")?;
    Ok(())
}

async fn serve_views(
    host: Arc<Host>,
    tabs_receiver: watch::Receiver<BrowserTabs>,
    context_ready: oneshot::Receiver<()>,
    failed: Arc<AtomicBool>,
) {
    let served = async {
        let endpoint = LocalEndpoint::browser_from_environment()
            .context("resolving the browser's endpoint")?;
        let listener = LocalListener::bind(&endpoint, ENDPOINT_PROBE_TIMEOUT)
            .await
            .context("binding the browser's endpoint")?;
        let (incoming, _ownership) = listener.into_parts();
        context_ready
            .await
            .context("CEF never became ready to create pages")?;
        host.events.send(HostEvent::Ready);
        tracing::info!(path = %endpoint.path().display(), "the browser accepts views");
        let service =
            BrowserServiceServer::new(BrowserGrpcService::new(Arc::clone(&host), tabs_receiver))
                .max_encoding_message_size(VIEW_RESPONSE_BYTES_MAX)
                .max_decoding_message_size(VIEW_REQUEST_BYTES_MAX);
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(incoming)
            .await
            .context("serving views")
    };
    if let Err(error) = served.await {
        tracing::error!(
            error = format!("{error:#}"),
            "the browser cannot serve views"
        );
        failed.store(true, Ordering::Relaxed);
        ui::run_on_ui(tabs::begin_shutdown);
    }
}
