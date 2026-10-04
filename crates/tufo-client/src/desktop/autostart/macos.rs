//! A launch agent in `~/Library/LaunchAgents`, which launchd starts at login.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::{APP_ID, AT_LOGIN_FLAG};

fn agent_path() -> io::Result<PathBuf> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    Ok(base
        .home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{APP_ID}.plist")))
}

pub(super) fn read_enabled() -> io::Result<bool> {
    agent_path()?.try_exists()
}

pub(super) fn enable(gui: &Path) -> io::Result<()> {
    let path = agent_path()?;
    super::super::files::write_atomically(&path, &agent_plist(gui))
}

pub(super) fn disable() -> io::Result<()> {
    match fs::remove_file(agent_path()?) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        removed => removed,
    }
}

fn agent_plist(gui: &Path) -> String {
    let gui = escape_xml(&gui.display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>Label</key>\n\
         \x20 <string>{APP_ID}</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>{gui}</string>\n\
         \x20   <string>{AT_LOGIN_FLAG}</string>\n\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key>\n\
         \x20 <true/>\n\
         \x20 <key>ProcessType</key>\n\
         \x20 <string>Interactive</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_launches_this_executable_at_login() {
        let plist = agent_plist(Path::new(
            "/Users/gm/Applications/Sound & Co/Tufo.app/Contents/MacOS/tufo-gui",
        ));

        assert!(plist.contains(
            "  <array>\n    \
             <string>/Users/gm/Applications/Sound &amp; Co/Tufo.app/Contents/MacOS/tufo-gui</string>\n    \
             <string>--at-login</string>\n  </array>\n"
        ));
        assert!(plist.contains("  <key>RunAtLoad</key>\n  <true/>\n"));
    }
}
