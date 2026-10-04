//! Local desktop settings shared by the GUI and CLI, independent of the server.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::{
    fs::{File, TryLockError},
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

pub mod autostart;
pub mod files;

const SETTINGS_FILE_NAME: &str = "config.toml";
const SETTINGS_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const SETTINGS_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "pt-BR", alias = "pt_BR")]
    PortugueseBrazil,
}

impl Language {
    #[must_use]
    pub const fn config_name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "en",
            Self::PortugueseBrazil => "pt-BR",
        }
    }

    #[must_use]
    pub fn resolve(self, locales: impl IntoIterator<Item = String>) -> Self {
        if self != Self::System {
            return self;
        }
        for locale in locales {
            let language = locale.split(['-', '_', '.', '@']).next().unwrap_or("");
            if language.eq_ignore_ascii_case("pt") {
                return Self::PortugueseBrazil;
            }
            if language.eq_ignore_ascii_case("en") || matches!(language, "C" | "POSIX") {
                return Self::English;
            }
        }
        Self::English
    }
}

/// The server keeps running for either quit behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuitBehavior {
    /// Fades everything out, closes the audio device, and leaves any voice channel.
    #[default]
    StandBy,
    KeepPlaying,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TourVisibility {
    AlwaysShow,
    #[default]
    ShowUnseen,
    AlwaysHide,
}

impl TourVisibility {
    #[must_use]
    pub const fn config_name(self) -> &'static str {
        match self {
            Self::AlwaysShow => "always-show",
            Self::ShowUnseen => "show-unseen",
            Self::AlwaysHide => "always-hide",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopSettings {
    pub quit_behavior: QuitBehavior,
    pub language: Language,
    pub tours: TourVisibility,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopSettingsUpdate {
    pub quit_behavior: Option<QuitBehavior>,
    pub language: Option<Language>,
    pub tours: Option<TourVisibility>,
}

impl DesktopSettingsUpdate {
    fn apply(self, current: DesktopSettings) -> DesktopSettings {
        DesktopSettings {
            quit_behavior: self.quit_behavior.unwrap_or(current.quit_behavior),
            language: self.language.unwrap_or(current.language),
            tours: self.tours.unwrap_or(current.tours),
        }
    }
}

/// The file's layout. Unknown keys are errors, so a misspelled key never silently does nothing.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsDocument {
    #[serde(default)]
    language: Language,
    #[serde(default)]
    tours: TourVisibility,
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
            tours: document.tours,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("cannot lock {}: {source}", path.display())]
    Lock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
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
pub struct SettingsFile {
    path: PathBuf,
}

impl SettingsFile {
    /// `TUFO_DESKTOP_SETTINGS_PATH` overrides the platform config directory.
    #[must_use]
    pub fn from_environment() -> Option<Self> {
        if let Some(path) = std::env::var_os("TUFO_DESKTOP_SETTINGS_PATH") {
            return Some(Self::at(path.into()));
        }
        let directories = directories::ProjectDirs::from("", "", "tufo")?;
        Some(Self::at(directories.config_dir().join(SETTINGS_FILE_NAME)))
    }

    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Reads the settings; a missing file holds the defaults.
    pub fn load(&self) -> Result<DesktopSettings, SettingsError> {
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

    /// Merges supplied fields into the latest file, preserving omitted fields.
    /// Empty updates only read the file; other updates wait up to five seconds for its lock.
    pub fn update(&self, update: DesktopSettingsUpdate) -> Result<DesktopSettings, SettingsError> {
        if update == DesktopSettingsUpdate::default() {
            return self.load();
        }
        let _settings_lock = self.lock(SETTINGS_LOCK_TIMEOUT)?;
        let current = self.load()?;
        let settings = update.apply(current);
        if settings != current {
            self.replace(settings)?;
        }
        Ok(settings)
    }

    fn lock(&self, timeout: Duration) -> Result<File, SettingsError> {
        let mut path = self.path.as_os_str().to_owned();
        path.push(".lock");
        let path = PathBuf::from(path);
        let lock = (|| -> io::Result<File> {
            if let Some(folder) = path.parent() {
                std::fs::create_dir_all(folder)?;
            }
            let mut options = File::options();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            options.mode(0o600);
            let file = options.open(&path)?;
            lock_with_timeout(&file, timeout)?;
            Ok(file)
        })();
        // Retain the companion file so atomic settings replacement cannot split the lock.
        lock.map_err(|source| SettingsError::Lock { path, source })
    }

    fn replace(&self, settings: DesktopSettings) -> Result<(), SettingsError> {
        files::write_atomically(&self.path, &render(settings)).map_err(|source| {
            SettingsError::Write {
                path: self.path.clone(),
                source,
            }
        })
    }
}

fn lock_with_timeout(file: &File, timeout: Duration) -> io::Result<()> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::Error(error)) => return Err(error),
            Err(TryLockError::WouldBlock) => {}
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        std::thread::sleep(remaining.min(SETTINGS_LOCK_RETRY_DELAY));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "timed out waiting for the desktop settings lock",
    ))
}

fn parse(text: &str) -> Result<DesktopSettings, toml::de::Error> {
    toml::from_str::<SettingsDocument>(text).map(DesktopSettings::from)
}

fn render(settings: DesktopSettings) -> String {
    let language = settings.language.config_name();
    let tours = settings.tours.config_name();
    let keep_playing = settings.quit_behavior == QuitBehavior::KeepPlaying;
    format!(
        "# Tufo's desktop settings. Changing a setting in Tufo rewrites this file.\n\
         \n\
         language = \"{language}\"\n\
         tours = \"{tours}\"\n\
         \n\
         [quit]\n\
         # Leave the music and sounds playing after Tufo quits.\n\
         keep_playing = {keep_playing}\n"
    )
}

#[cfg(test)]
mod tests;
