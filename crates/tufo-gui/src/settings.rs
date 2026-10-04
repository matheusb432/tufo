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

    pub(crate) fn update(
        &self,
        update: DesktopSettingsUpdate,
    ) -> Result<DesktopSettings, SettingsError> {
        self.file.update(update)
    }
}

#[cfg(test)]
mod tests;
