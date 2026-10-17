//! `%LOCALAPPDATA%\Programs\Tufo` for the executables, and a Start menu shortcut, which Windows
//! search finds. The shortcut shows the icon embedded in `tufo-gui.exe`. A built browser host goes
//! to a `browser` folder there with Chromium's runtime.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, ensure};

use super::{BINARY_NAMES, remove_files, replace_file};
use crate::windows_browser;

/// Creates or replaces the shortcut named by `TUFO_SHORTCUT`, which opens `TUFO_TARGET`.
const CREATE_SHORTCUT_SCRIPT: &str = "\
    $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($env:TUFO_SHORTCUT); \
    $shortcut.TargetPath = $env:TUFO_TARGET; \
    $shortcut.WorkingDirectory = Split-Path -Parent $env:TUFO_TARGET; \
    $shortcut.Description = 'Music and sound effects for tabletop sessions'; \
    $shortcut.Save()";

struct Places {
    programs: PathBuf,
    start_menu_shortcut: PathBuf,
}

impl Places {
    fn for_current_user() -> Result<Self> {
        let base = directories::BaseDirs::new().context("the current user has no profile")?;
        Ok(Self {
            programs: base.data_local_dir().join("Programs").join("Tufo"),
            start_menu_shortcut: base
                .data_dir()
                .join(r"Microsoft\Windows\Start Menu\Programs\Tufo.lnk"),
        })
    }

    fn executable(&self, binary_name: &str) -> PathBuf {
        self.programs.join(format!("{binary_name}.exe"))
    }

    fn browser(&self) -> PathBuf {
        self.programs.join(windows_browser::FOLDER_NAME)
    }
}

pub(super) fn install(workspace_root: &Path, release_dir: &Path) -> Result<PathBuf> {
    let places = Places::for_current_user()?;
    let browser = windows_browser::find_source(workspace_root, release_dir)?;
    fs::create_dir_all(&places.programs)
        .with_context(|| format!("create {}", places.programs.display()))?;
    for name in BINARY_NAMES {
        let file_name = format!("{name}.exe");
        replace_file(&release_dir.join(&file_name), &places.executable(name))
            .context("quit Tufo and stop tufo-server before installing over them")?;
    }
    match browser {
        Some(source) => {
            windows_browser::place(&source, &places.browser())?;
            windows_browser::grant_app_container_access(&places.browser())?;
            println!("installed the browser in {}", places.browser().display());
        }
        None => println!("no tufo-browser build; Tufo installs without its browser"),
    }
    create_shortcut(&places.start_menu_shortcut, &places.executable("tufo-gui"))?;
    println!(
        "installed Tufo: {} and the Start menu shortcut {}",
        places.executable("tufo").display(),
        places.start_menu_shortcut.display()
    );
    Ok(places.executable("tufo"))
}

pub(super) fn uninstall() -> Result<()> {
    let places = Places::for_current_user()?;
    let browser = places.browser();
    let mut paths = BINARY_NAMES.map(|name| places.executable(name)).to_vec();
    paths.push(places.start_menu_shortcut);
    remove_files(&paths.iter().map(PathBuf::as_path).collect::<Vec<_>>())?;
    windows_browser::remove(&browser)?;
    // Only an empty directory goes; anything else in it isn't the installer's.
    let _ = fs::remove_dir(&places.programs);
    Ok(())
}

/// Force-stops every running Tufo GUI, server, and browser of this user, GUI first so it can't
/// start the server again. Windows exposes no cheap way to read another process's environment, so
/// isolated instances stop too.
pub(super) fn stop_session() -> Result<()> {
    /// taskkill's exit code when no process matched.
    const NO_MATCHING_PROCESS: i32 = 128;
    for name in ["tufo-gui", "tufo-server", "tufo-browser"] {
        let status = Command::new("taskkill")
            .args(["/F", "/IM", &format!("{name}.exe")])
            .status()
            .context("run taskkill")?;
        ensure!(
            status.success() || status.code() == Some(NO_MATCHING_PROCESS),
            "taskkill failed to stop {name}: {status}"
        );
    }
    Ok(())
}

fn create_shortcut(shortcut: &Path, target: &Path) -> Result<()> {
    let status = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            CREATE_SHORTCUT_SCRIPT,
        ])
        .env("TUFO_SHORTCUT", shortcut)
        .env("TUFO_TARGET", target)
        .status()
        .context("run PowerShell to create the Start menu shortcut")?;
    ensure!(
        status.success(),
        "PowerShell failed to create {}: {status}",
        shortcut.display()
    );
    Ok(())
}
