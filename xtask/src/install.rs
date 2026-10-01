//! Installs the release build for the current user, with a launcher that desktop search finds: a
//! desktop entry and icon on Linux, an app bundle on macOS, a Start menu shortcut on Windows.
//! Uninstalling keeps the library, the settings, and the Discord credentials.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, ensure};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// Both clients start the server from their own directory, so all three install side by side.
pub(crate) const BINARY_NAMES: [&str; 3] = ["tufo-gui", "tufo-server", "tufo"];

pub(crate) fn install(workspace_root: &Path, release_dir: &Path) -> Result<()> {
    // A missing build must fail before closing the running application.
    for name in BINARY_NAMES {
        let binary = release_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        ensure!(
            binary.is_file(),
            "missing {}; run `just build` first",
            binary.display()
        );
    }
    let cli = place(workspace_root, release_dir)?;
    let status = Command::new(&cli)
        .args(["server", "start"])
        .status()
        .with_context(|| format!("start the installed server through {}", cli.display()))?;
    ensure!(
        status.success(),
        "the installed server did not become ready; run `tufo doctor` for recovery actions"
    );
    Ok(())
}

fn place(workspace_root: &Path, release_dir: &Path) -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    return {
        linux::stop_session()?;
        linux::install(workspace_root, release_dir)
    };
    #[cfg(target_os = "macos")]
    return {
        macos::stop_session()?;
        macos::install(workspace_root, release_dir)
    };
    #[cfg(windows)]
    return {
        let _ = workspace_root;
        windows::stop_session()?;
        windows::install(release_dir)
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    anyhow::bail!("installing supports Linux, macOS, and Windows")
}

/// Closes the session's Tufo and its server, then installs the release build over them.
pub(crate) fn update(workspace_root: &Path, release_dir: &Path) -> Result<()> {
    install(workspace_root, release_dir)?;
    println!("open Tufo again to use the new build");
    Ok(())
}

pub(crate) fn uninstall() -> Result<()> {
    #[cfg(target_os = "linux")]
    return linux::uninstall();
    #[cfg(target_os = "macos")]
    return macos::uninstall();
    #[cfg(windows)]
    return windows::uninstall();
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    anyhow::bail!("installing supports Linux, macOS, and Windows")
}

/// Copies next to the destination, then renames over it, so a running copy keeps its own file.
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    let mut staged = destination.as_os_str().to_owned();
    staged.push(".installing");
    fs::copy(source, &staged).with_context(|| format!("copy {}", source.display()))?;
    fs::rename(&staged, destination).with_context(|| {
        let _ = fs::remove_file(&staged);
        format!("replace {}", destination.display())
    })
}

/// Removes the files that exist, and names them.
fn remove_files(paths: &[&Path]) -> Result<()> {
    for path in paths {
        match fs::remove_file(path) {
            Ok(()) => println!("removed {}", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("remove {}", path.display()));
            }
        }
    }
    Ok(())
}
