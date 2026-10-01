//! `~/Applications/Tufo.app` holds the three executables, which Spotlight, Launchpad, and the
//! Dock find, with an icon drawn from the Linux icons' sources. `~/.local/bin/tufo` links to the
//! command inside it.

use std::{
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};

use super::{BINARY_NAMES, remove_files, replace_file};
use crate::app_icon::{self, ICONS_DIR, SMALL_ICON_SVG};

/// The GUI's application ID, which also names its launch agent for starting at login.
const APP_ID: &str = "dev.tufo.Tufo";
const BUNDLE_NAME: &str = "Tufo.app";
const BUNDLE_EXECUTABLE: &str = "tufo-gui";
const BUNDLE_VERSION_PACKAGE: &str = "tufo-gui";
const APP_ICON_SVG: &str = "tufo.svg";
/// The bundle's icon in `Contents/Resources`; `Info.plist` names it without the extension.
const ICON_NAME: &str = "tufo";
/// The server drains for up to 5 s after SIGTERM before it exits.
const EXIT_WAIT_MAX: Duration = Duration::from_secs(15);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

struct Places {
    applications: PathBuf,
    command_links: PathBuf,
    launch_agent: PathBuf,
}

impl Places {
    fn for_current_user() -> Result<Self> {
        let base =
            directories::BaseDirs::new().context("the current user has no home directory")?;
        let home = base.home_dir();
        Ok(Self {
            applications: home.join("Applications"),
            command_links: home.join(".local").join("bin"),
            launch_agent: home
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{APP_ID}.plist")),
        })
    }

    fn bundle(&self) -> PathBuf {
        self.applications.join(BUNDLE_NAME)
    }

    fn info_plist(&self) -> PathBuf {
        self.bundle().join("Contents").join("Info.plist")
    }

    fn executables(&self) -> PathBuf {
        self.bundle().join("Contents").join("MacOS")
    }

    fn executable(&self, binary_name: &str) -> PathBuf {
        self.executables().join(binary_name)
    }

    fn icon(&self) -> PathBuf {
        self.bundle()
            .join("Contents")
            .join("Resources")
            .join(format!("{ICON_NAME}.icns"))
    }

    fn command_link(&self) -> PathBuf {
        self.command_links.join("tufo")
    }
}

pub(super) fn install(workspace_root: &Path, release_dir: &Path) -> Result<PathBuf> {
    let places = Places::for_current_user()?;
    let version = bundle_version(workspace_root)?;
    install_into(
        &places,
        release_dir,
        &workspace_root.join(ICONS_DIR),
        &version,
    )?;
    sign(&places.bundle())?;
    println!(
        "installed {} and the command {}",
        places.bundle().display(),
        places.command_link().display()
    );
    if !is_on_path(&places.command_links) {
        println!(
            "add {} to PATH to run `tufo` from a terminal",
            places.command_links.display()
        );
    }
    Ok(places.executable("tufo"))
}

/// Removes the bundle, the command's link, and the launch agent for starting at login.
pub(super) fn uninstall() -> Result<()> {
    let places = Places::for_current_user()?;
    remove_files(&[&places.command_link(), &places.launch_agent])?;
    let bundle = places.bundle();
    if !bundle.exists() {
        return Ok(());
    }
    ensure_is_tufo_bundle(&places)?;
    fs::remove_dir_all(&bundle).with_context(|| format!("remove {}", bundle.display()))?;
    println!("removed {}", bundle.display());
    Ok(())
}

/// Stops every GUI and server of this user, GUI first so it can't start the server again.
/// macOS shows no cheap way to read another process's environment, so isolated instances stop
/// too.
pub(super) fn stop_session() -> Result<()> {
    let user_id = current_user_id()?;
    for name in ["tufo-gui", "tufo-server"] {
        if !is_running(&user_id, name)? {
            continue;
        }
        signal(&user_id, name, "TERM")?;
        let deadline = Instant::now() + EXIT_WAIT_MAX;
        while is_running(&user_id, name)? {
            if Instant::now() >= deadline {
                eprintln!("killing {name}, which ignored SIGTERM");
                signal(&user_id, name, "KILL")?;
                break;
            }
            thread::sleep(EXIT_POLL_INTERVAL);
        }
        println!("stopped {name}");
    }
    Ok(())
}

fn install_into(places: &Places, release_dir: &Path, icons: &Path, version: &str) -> Result<()> {
    if places.bundle().exists() {
        ensure_is_tufo_bundle(places)?;
    }
    let executables = places.executables();
    fs::create_dir_all(&executables)
        .with_context(|| format!("create {}", executables.display()))?;
    for name in BINARY_NAMES {
        replace_file(&release_dir.join(name), &places.executable(name))?;
    }
    write_creating_directories(&places.info_plist(), info_plist(version).as_bytes())?;
    let bundle_icon = app_icon::icns(
        &read(&icons.join(APP_ICON_SVG))?,
        &read(&icons.join(SMALL_ICON_SVG))?,
    )?;
    write_creating_directories(&places.icon(), &bundle_icon)?;
    link_command(&places.executable("tufo"), &places.command_link())
}

/// Refuses to change a `Tufo.app` that another app's bundle occupies.
fn ensure_is_tufo_bundle(places: &Places) -> Result<()> {
    let info = fs::read_to_string(places.info_plist()).unwrap_or_default();
    if !info.contains(&format!("<string>{APP_ID}</string>")) {
        bail!(
            "{} belongs to another app; move it away first",
            places.bundle().display()
        );
    }
    Ok(())
}

fn info_plist(version: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>CFBundleDevelopmentRegion</key>\n\
         \x20 <string>en</string>\n\
         \x20 <key>CFBundleDisplayName</key>\n\
         \x20 <string>Tufo</string>\n\
         \x20 <key>CFBundleExecutable</key>\n\
         \x20 <string>{BUNDLE_EXECUTABLE}</string>\n\
         \x20 <key>CFBundleIconFile</key>\n\
         \x20 <string>{ICON_NAME}</string>\n\
         \x20 <key>CFBundleIdentifier</key>\n\
         \x20 <string>{APP_ID}</string>\n\
         \x20 <key>CFBundleInfoDictionaryVersion</key>\n\
         \x20 <string>6.0</string>\n\
         \x20 <key>CFBundleName</key>\n\
         \x20 <string>Tufo</string>\n\
         \x20 <key>CFBundlePackageType</key>\n\
         \x20 <string>APPL</string>\n\
         \x20 <key>CFBundleShortVersionString</key>\n\
         \x20 <string>{version}</string>\n\
         \x20 <key>CFBundleVersion</key>\n\
         \x20 <string>{version}</string>\n\
         \x20 <key>LSApplicationCategoryType</key>\n\
         \x20 <string>public.app-category.music</string>\n\
         \x20 <key>NSHighResolutionCapable</key>\n\
         \x20 <true/>\n\
         </dict>\n\
         </plist>\n"
    )
}

fn bundle_version(workspace_root: &Path) -> Result<String> {
    Ok(crate::workspace_metadata(workspace_root)?
        .packages
        .iter()
        .find(|package| package.name.as_str() == BUNDLE_VERSION_PACKAGE)
        .with_context(|| format!("the workspace has no {BUNDLE_VERSION_PACKAGE} package"))?
        .version
        .to_string())
}

/// Links next to the destination, then renames over it, so `tufo` never goes missing.
fn link_command(target: &Path, link: &Path) -> Result<()> {
    if let Some(directory) = link.parent() {
        fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;
    }
    let mut staged = link.as_os_str().to_owned();
    staged.push(".installing");
    let _ = fs::remove_file(&staged);
    symlink(target, &staged).with_context(|| format!("link {}", link.display()))?;
    fs::rename(&staged, link).with_context(|| format!("replace {}", link.display()))
}

/// Signs the bundle ad hoc. Apple silicon runs only signed code, and the linker signs each
/// executable alone; this seals the executables, `Info.plist`, and the icon into one app.
fn sign(bundle: &Path) -> Result<()> {
    let status = Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(bundle)
        .status()
        .context("run codesign")?;
    ensure!(
        status.success(),
        "codesign failed to sign {}: {status}",
        bundle.display()
    );
    Ok(())
}

fn is_on_path(directory: &Path) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|entry| entry == directory))
}

fn current_user_id() -> Result<String> {
    let output = Command::new("id").arg("-u").output().context("run id")?;
    ensure!(output.status.success(), "id -u failed: {}", output.status);
    Ok(String::from_utf8(output.stdout)
        .context("id -u printed a non-UTF-8 user ID")?
        .trim()
        .to_owned())
}

fn is_running(user_id: &str, process_name: &str) -> Result<bool> {
    /// pgrep's exit code when no process matched.
    const NO_MATCHING_PROCESS: i32 = 1;
    let status = Command::new("pgrep")
        .args(["-x", "-U", user_id, process_name])
        .stdout(std::process::Stdio::null())
        .status()
        .context("run pgrep")?;
    ensure!(
        status.success() || status.code() == Some(NO_MATCHING_PROCESS),
        "pgrep failed to look for {process_name}: {status}"
    );
    Ok(status.success())
}

fn signal(user_id: &str, process_name: &str, signal_name: &str) -> Result<()> {
    /// pkill's exit code when no process matched, as when it exited since the last look.
    const NO_MATCHING_PROCESS: i32 = 1;
    let status = Command::new("pkill")
        .arg(format!("-{signal_name}"))
        .args(["-x", "-U", user_id, process_name])
        .status()
        .context("run pkill")?;
    ensure!(
        status.success() || status.code() == Some(NO_MATCHING_PROCESS),
        "pkill -{signal_name} failed to stop {process_name}: {status}"
    );
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("read {}", path.display()))
}

fn write_creating_directories(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn places(root: &Path) -> Places {
        Places {
            applications: root.join("Applications"),
            command_links: root.join("bin"),
            launch_agent: root.join("LaunchAgents").join(format!("{APP_ID}.plist")),
        }
    }

    fn release_dir(root: &Path) -> PathBuf {
        let release = root.join("release");
        fs::create_dir_all(&release).unwrap();
        for name in BINARY_NAMES {
            let binary = release.join(name);
            fs::write(&binary, name).unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        }
        release
    }

    fn icons() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(ICONS_DIR)
    }

    #[test]
    fn install_builds_the_bundle_and_links_the_command() {
        let root = tempfile::tempdir().unwrap();
        let places = places(root.path());

        install_into(&places, &release_dir(root.path()), &icons(), "1.2.3").unwrap();

        for name in BINARY_NAMES {
            assert_eq!(fs::read_to_string(places.executable(name)).unwrap(), name);
        }
        let info = fs::read_to_string(places.info_plist()).unwrap();
        assert!(info.contains("<key>CFBundleExecutable</key>\n  <string>tufo-gui</string>\n"));
        assert!(info.contains("<key>CFBundleShortVersionString</key>\n  <string>1.2.3</string>\n"));
        assert!(fs::read(places.icon()).unwrap().starts_with(b"icns"));
        assert_eq!(
            fs::read_link(places.command_link()).unwrap(),
            places.executable("tufo")
        );
    }

    #[test]
    fn installing_again_replaces_the_executables_and_the_link() {
        let root = tempfile::tempdir().unwrap();
        let places = places(root.path());
        let release = release_dir(root.path());
        install_into(&places, &release, &icons(), "1.2.3").unwrap();
        fs::write(release.join("tufo-gui"), "newer").unwrap();

        install_into(&places, &release, &icons(), "1.2.4").unwrap();

        assert_eq!(
            fs::read_to_string(places.executable("tufo-gui")).unwrap(),
            "newer"
        );
        assert_eq!(fs::read_dir(&places.command_links).unwrap().count(), 1);
    }

    #[test]
    fn install_leaves_another_apps_bundle_alone() {
        let root = tempfile::tempdir().unwrap();
        let places = places(root.path());
        write_creating_directories(
            &places.info_plist(),
            b"<plist><dict><key>CFBundleIdentifier</key><string>org.example.Other</string></dict></plist>",
        )
        .unwrap();

        let error = install_into(&places, &release_dir(root.path()), &icons(), "1.2.3");

        assert!(error.is_err());
        assert!(!places.executables().exists());
    }
}
