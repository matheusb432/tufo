pub(crate) use tufo_client::desktop::{DesktopSettings, QuitBehavior};
use tufo_client::desktop::{DesktopSettingsUpdate, SettingsError};

#[derive(Debug, Clone)]
pub(crate) struct SettingsFile {
    file: tufo_client::desktop::SettingsFile,
}

impl SettingsFile {
    pub(crate) fn from_environment() -> Option<Self> {
        tufo_client::desktop::SettingsFile::from_environment().map(|file| Self { file })
    }

    pub(crate) fn load(&self) -> Result<DesktopSettings, SettingsError> {
        self.file.load()
    }

    pub(crate) fn save_quit_behavior(
        &self,
        quit_behavior: QuitBehavior,
    ) -> Result<(), SettingsError> {
        self.file.update(DesktopSettingsUpdate {
            quit_behavior: Some(quit_behavior),
            ..DesktopSettingsUpdate::default()
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
