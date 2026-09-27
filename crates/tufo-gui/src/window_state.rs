//! The window's layout between launches, in `window.toml` in the state directory.

use std::{io, path::PathBuf};

use slint::ComponentHandle as _;
use tokio::{runtime::Handle, sync::watch};

use crate::{
    files,
    ui::{Actions, AppWindow, Session},
};

const WINDOW_STATE_FILE_NAME: &str = "window.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct WindowState {
    pub(crate) drawer_pinned: bool,
    /// The player shows as one slim row in a narrow window.
    pub(crate) player_collapsed: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            drawer_pinned: true,
            player_collapsed: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WindowStateFile {
    path: PathBuf,
}

impl WindowStateFile {
    pub(crate) fn from_environment() -> Option<Self> {
        let directories = directories::ProjectDirs::from("", "", "tufo")?;
        let folder = directories
            .state_dir()
            .unwrap_or_else(|| directories.data_local_dir());
        Some(Self::at(folder.join(WINDOW_STATE_FILE_NAME)))
    }

    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn load(&self) -> WindowState {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) => {
                if error.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(%error, path = %self.path.display(), "cannot read the window's layout");
                }
                return WindowState::default();
            }
        };
        toml::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!(%error, path = %self.path.display(), "the window's layout is not valid; using the defaults");
            WindowState::default()
        })
    }

    pub(crate) fn save(&self, state: WindowState) -> io::Result<()> {
        files::write_atomically(&self.path, &render(state))
    }
}

fn render(state: WindowState) -> String {
    format!(
        "drawer_pinned = {}\nplayer_collapsed = {}\n",
        state.drawer_pinned, state.player_collapsed
    )
}

/// Restores the layout before the window first shows, and saves each change off the UI thread.
pub(crate) fn install(window: &AppWindow, runtime: &Handle, file: WindowStateFile) {
    let state = file.load();
    let session = window.global::<Session>();
    session.set_drawer_pinned(state.drawer_pinned);
    session.set_player_collapsed(state.player_collapsed);
    let (layout_sender, mut layout_receiver) = watch::channel(state);
    let actions = window.global::<Actions>();
    let drawer_sender = layout_sender.clone();
    actions.on_set_drawer_pinned(move |drawer_pinned| {
        drawer_sender.send_modify(|state| state.drawer_pinned = drawer_pinned);
    });
    actions.on_set_player_collapsed(move |player_collapsed| {
        layout_sender.send_modify(|state| state.player_collapsed = player_collapsed);
    });
    runtime.spawn(async move {
        while layout_receiver.changed().await.is_ok() {
            let state = *layout_receiver.borrow_and_update();
            let file = file.clone();
            let saved = tokio::task::spawn_blocking(move || file.save(state))
                .await
                .unwrap_or_else(|error| Err(io::Error::other(error)));
            if let Err(error) = saved {
                tracing::warn!(%error, "cannot save the window's layout");
            }
        }
    });
}

#[cfg(test)]
mod tests;
