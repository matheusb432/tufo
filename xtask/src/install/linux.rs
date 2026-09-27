//! `~/.local/bin` for the binaries, and a desktop entry with its icon under `~/.local/share`,
//! which GNOME's search and dock read: the scalable art, and PNGs of the small-size art for 16 to
//! 32 px, which GNOME prefers over scaling the SVG down.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

use super::{BINARY_NAMES, remove_files, replace_file};
use crate::app_icon::{self, ICONS_DIR, SMALL_ICON_SVG};

mod session;

/// The GUI's application ID, which names its window for the desktop; `tufo-gui` sets the same.
const APP_ID: &str = "dev.tufo.Tufo";
const APP_ICON_SVG: &str = "tufo.svg";
/// The hicolor sizes the small-size art covers.
const FIXED_ICON_SIDES_PX: [u32; 4] = [16, 22, 24, 32];

/// Where an install goes: `$XDG_BIN_HOME` or `~/.local/bin`, and `$XDG_DATA_HOME` or
/// `~/.local/share`.
struct Homes {
    binaries: PathBuf,
    data: PathBuf,
}

impl Homes {
    fn for_current_user() -> Result<Self> {
        let base =
            directories::BaseDirs::new().context("the current user has no home directory")?;
        Ok(Self {
            binaries: base
                .executable_dir()
                .context("the current user has no binary directory")?
                .to_path_buf(),
            data: base.data_dir().to_path_buf(),
        })
    }

    fn desktop_entry(&self) -> PathBuf {
        self.data
            .join("applications")
            .join(format!("{APP_ID}.desktop"))
    }

    fn icon(&self) -> PathBuf {
        self.data
            .join("icons/hicolor/scalable/apps")
            .join(format!("{APP_ID}.svg"))
    }

    fn fixed_icon(&self, side: u32) -> PathBuf {
        self.data
            .join(format!("icons/hicolor/{side}x{side}/apps"))
            .join(format!("{APP_ID}.png"))
    }

    fn icons(&self) -> Vec<PathBuf> {
        let mut icons = vec![self.icon()];
        icons.extend(FIXED_ICON_SIDES_PX.map(|side| self.fixed_icon(side)));
        icons
    }

    fn binary(&self, name: &str) -> PathBuf {
        self.binaries.join(name)
    }
}

pub(super) fn install(workspace_root: &Path, release_dir: &Path) -> Result<PathBuf> {
    let homes = Homes::for_current_user()?;
    install_into(&homes, release_dir, &workspace_root.join(ICONS_DIR))?;
    println!(
        "installed {} and the launcher {}",
        homes.binary("tufo").display(),
        homes.desktop_entry().display()
    );
    Ok(homes.binary("tufo"))
}

/// Stops the session's Tufo GUI and server; isolated instances keep running.
pub(super) fn stop_session() -> Result<()> {
    session::stop()
}

pub(super) fn uninstall() -> Result<()> {
    let homes = Homes::for_current_user()?;
    let mut paths = BINARY_NAMES.map(|name| homes.binary(name)).to_vec();
    paths.push(homes.desktop_entry());
    paths.extend(homes.icons());
    remove_files(&paths.iter().map(PathBuf::as_path).collect::<Vec<_>>())?;
    refresh_icon_theme(&homes.data)
}

fn install_into(homes: &Homes, release_dir: &Path, icons: &Path) -> Result<()> {
    fs::create_dir_all(&homes.binaries)
        .with_context(|| format!("create {}", homes.binaries.display()))?;
    for name in BINARY_NAMES {
        replace_file(&release_dir.join(name), &homes.binary(name))?;
    }
    let desktop_entry = homes.desktop_entry();
    write_creating_directories(
        &desktop_entry,
        desktop_entry_text(&homes.binary("tufo-gui")).as_bytes(),
    )?;
    write_creating_directories(&homes.icon(), &read(&icons.join(APP_ICON_SVG))?)?;
    let small_icon = read(&icons.join(SMALL_ICON_SVG))?;
    for side in FIXED_ICON_SIDES_PX {
        write_creating_directories(&homes.fixed_icon(side), &app_icon::png(&small_icon, side)?)?;
    }
    refresh_icon_theme(&homes.data)
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("read {}", path.display()))
}

fn desktop_entry_text(gui: &Path) -> String {
    let exec = desktop_exec_argument(gui);
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Tufo\n\
         GenericName=Tabletop Soundboard\n\
         Comment=Play music and sound effects for tabletop sessions, here or into Discord\n\
         Exec={exec}\n\
         TryExec={exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=AudioVideo;Audio;Player;\n\
         Keywords=music;soundboard;sound;effects;rpg;tabletop;discord;kenku;\n\
         StartupNotify=false\n\
         StartupWMClass={APP_ID}\n\
         SingleMainWindow=true\n"
    )
}

/// Quotes a path for `Exec=` when the desktop entry specification requires it.
fn desktop_exec_argument(path: &Path) -> String {
    let raw = path.display().to_string();
    if raw
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "/._-".contains(character))
    {
        return raw;
    }
    let escaped = raw
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`");
    format!("\"{escaped}\"")
}

fn write_creating_directories(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

/// GTK notices a changed icon only when the theme directory's modification time moves.
fn refresh_icon_theme(data_home: &Path) -> Result<()> {
    let theme = data_home.join("icons/hicolor");
    if !theme.is_dir() {
        return Ok(());
    }
    let marker = theme.join(".tufo-icon-refresh");
    fs::write(&marker, b"").with_context(|| format!("write {}", marker.display()))?;
    fs::remove_file(&marker).with_context(|| format!("remove {}", marker.display()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn homes(root: &Path) -> Homes {
        Homes {
            binaries: root.join("bin"),
            data: root.join("share"),
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
    fn install_places_the_binaries_the_launcher_and_the_icons() {
        let root = tempfile::tempdir().unwrap();
        let homes = homes(root.path());

        install_into(&homes, &release_dir(root.path()), &icons()).unwrap();

        for name in BINARY_NAMES {
            let binary = homes.binary(name);
            assert_eq!(fs::read_to_string(&binary).unwrap(), name);
            assert_eq!(
                fs::metadata(&binary).unwrap().permissions().mode() & 0o111,
                0o111
            );
        }
        let entry = fs::read_to_string(homes.desktop_entry()).unwrap();
        assert!(entry.contains(&format!("Exec={}\n", homes.binary("tufo-gui").display())));
        assert!(entry.contains("StartupWMClass=dev.tufo.Tufo\n"));
        assert_eq!(
            fs::read(homes.icon()).unwrap(),
            fs::read(icons().join(APP_ICON_SVG)).unwrap()
        );
        for side in FIXED_ICON_SIDES_PX {
            assert!(
                fs::read(homes.fixed_icon(side))
                    .unwrap()
                    .starts_with(b"\x89PNG")
            );
        }
    }

    #[test]
    fn installing_again_replaces_the_binaries() {
        let root = tempfile::tempdir().unwrap();
        let homes = homes(root.path());
        let release = release_dir(root.path());
        install_into(&homes, &release, &icons()).unwrap();
        fs::write(release.join("tufo-gui"), "newer").unwrap();

        install_into(&homes, &release, &icons()).unwrap();

        assert_eq!(
            fs::read_to_string(homes.binary("tufo-gui")).unwrap(),
            "newer"
        );
        assert_eq!(
            fs::read_dir(&homes.binaries).unwrap().count(),
            BINARY_NAMES.len()
        );
    }

    #[test]
    fn exec_quotes_paths_with_spaces() {
        assert_eq!(
            desktop_exec_argument(Path::new("/home/me/.local/bin/tufo-gui")),
            "/home/me/.local/bin/tufo-gui"
        );
        assert_eq!(
            desktop_exec_argument(Path::new("/home/my $name/tufo-gui")),
            "\"/home/my \\$name/tufo-gui\""
        );
    }
}
