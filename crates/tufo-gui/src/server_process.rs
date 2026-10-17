//! Starting `tufo-server` when its endpoint isn't live, detached so it can outlive the window.

/// Starts the server next to this executable, or the one on `PATH`, in its own process group so
/// the window's terminal can't stop it. It inherits the environment, including
/// `TUFO_DISCORD_CREDENTIALS`.
pub(crate) fn spawn_server() -> std::io::Result<()> {
    let mut child = tufo_client::process::Executable::Server.command().spawn()?;
    // Collects the exit status whenever the server stops, without blocking the window.
    std::thread::Builder::new()
        .name("tufo-server-reaper".to_owned())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}
