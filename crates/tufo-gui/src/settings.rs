//! Tufo's desktop settings in `config.toml`, in Tufo's config directory: `$XDG_CONFIG_HOME/tufo`
//! on Linux and `%APPDATA%\tufo\config` on Windows. The window needs them before it reaches the
//! server, so the window owns them rather than the server's database.

use std::{io, path::PathBuf};

use crate::{files, i18n::Language};

const SETTINGS_FILE_NAME: &str = "config.toml";

/// What quitting Tufo does to the audio. tufo-server keeps running either way, so the next launch
/// finds the library and the Discord bot ready.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum QuitBehavior {
    /// Fades everything out, closes the audio device, and leaves any voice channel.
    #[default]
    StandBy,
    KeepPlaying,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DesktopSettings {
    pub(crate) quit_behavior: QuitBehavior,
    pub(crate) language: Language,
}

/// The file's layout. Unknown keys are errors, so a misspelled key never silently does nothing.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsDocument {
    #[serde(default)]
    language: Language,
    #[serde(default)]
    quit: QuitSection,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QuitSection {
    #[serde(default)]
    keep_playing: bool,
}

impl From<SettingsDocument> for DesktopSettings {
    fn from(document: SettingsDocument) -> Self {
        let quit_behavior = if document.quit.keep_playing {
            QuitBehavior::KeepPlaying
        } else {
            QuitBehavior::StandBy
        };
        Self {
            quit_behavior,
            language: document.language,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SettingsError {
    #[error("cannot read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not valid; fix or delete it: {source}", path.display())]
    Invalid {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("cannot write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct SettingsFile {
    path: PathBuf,
}

impl SettingsFile {
    /// The file in Tufo's config directory, or `None` when the system names no home directory.
    pub(crate) fn from_environment() -> Option<Self> {
        let directories = directories::ProjectDirs::from("", "", "tufo")?;
        Some(Self::at(directories.config_dir().join(SETTINGS_FILE_NAME)))
    }

    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Reads the settings; a missing file holds the defaults.
    pub(crate) fn load(&self) -> Result<DesktopSettings, SettingsError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => parse(&text).map_err(|source| SettingsError::Invalid {
                path: self.path.clone(),
                source: Box::new(source),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DesktopSettings::default()),
            Err(source) => Err(SettingsError::Read {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Replaces the file in one rename. It refuses to replace a file it cannot read as settings,
    /// so a hand edit with a typo is never lost.
    pub(crate) fn save(&self, settings: DesktopSettings) -> Result<(), SettingsError> {
        self.load()?;
        files::write_atomically(&self.path, &render(settings)).map_err(|source| {
            SettingsError::Write {
                path: self.path.clone(),
                source,
            }
        })
    }
}

fn parse(text: &str) -> Result<DesktopSettings, toml::de::Error> {
    toml::from_str::<SettingsDocument>(text).map(DesktopSettings::from)
}

fn render(settings: DesktopSettings) -> String {
    let language = settings.language.config_name();
    let keep_playing = settings.quit_behavior == QuitBehavior::KeepPlaying;
    format!(
        "# Tufo's desktop settings. Saving Settings in Tufo rewrites this file.\n\
         \n\
         language = \"{language}\"\n\
         \n\
         [quit]\n\
         # Leave the music and sounds playing after Tufo quits.\n\
         keep_playing = {keep_playing}\n"
    )
}

#[cfg(test)]
mod tests;
