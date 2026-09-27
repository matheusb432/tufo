//! An XDG autostart desktop entry in `$XDG_CONFIG_HOME/autostart`.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::AT_LOGIN_FLAG;
use crate::APP_ID;

const AUTOSTART_FOLDER_NAME: &str = "autostart";

fn entry_path() -> io::Result<PathBuf> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    Ok(base
        .config_dir()
        .join(AUTOSTART_FOLDER_NAME)
        .join(format!("{APP_ID}.desktop")))
}

pub(crate) fn is_enabled() -> bool {
    entry_path().is_ok_and(|path| path.exists())
}

pub(crate) fn set_enabled(enabled: bool) -> io::Result<()> {
    let path = entry_path()?;
    if enabled {
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)?;
        }
        fs::write(&path, entry_text(&std::env::current_exe()?))
    } else {
        match fs::remove_file(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            removed => removed,
        }
    }
}

fn entry_text(gui: &Path) -> String {
    let exec = exec_argument(gui);
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Tufo\n\
         Exec={exec} {AT_LOGIN_FLAG}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// Quotes a path for `Exec=` when the desktop entry specification requires it.
fn exec_argument(path: &Path) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_launches_this_executable_at_login() {
        let text = entry_text(Path::new("/home/gm/My Tools/tufo-gui"));

        assert!(text.contains("\nExec=\"/home/gm/My Tools/tufo-gui\" --at-login\n"));
        assert!(text.contains("\nX-GNOME-Autostart-enabled=true\n"));
    }
}
