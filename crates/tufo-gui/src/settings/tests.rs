use tufo_client::desktop::{Language, TourVisibility};

use super::*;

#[test]
fn gui_changes_preserve_external_desktop_settings() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let shared = tufo_client::desktop::SettingsFile::at(directory.path().join("config.toml"));
    let file = SettingsFile {
        file: shared.clone(),
    };
    let gui_settings = file.load()?;
    shared.update(DesktopSettingsUpdate {
        language: Some(Language::PortugueseBrazil),
        ..DesktopSettingsUpdate::default()
    })?;

    let saved = file.update(DesktopSettingsUpdate {
        quit_behavior: Some(QuitBehavior::KeepPlaying),
        ..DesktopSettingsUpdate::default()
    })?;
    assert_ne!(saved.language, gui_settings.language);

    let saved = file.update(DesktopSettingsUpdate {
        tours: Some(TourVisibility::AlwaysHide),
        ..DesktopSettingsUpdate::default()
    })?;
    assert_eq!(shared.load()?, saved);
    assert_eq!(saved.language, Language::PortugueseBrazil);
    assert_eq!(saved.quit_behavior, QuitBehavior::KeepPlaying);
    assert_eq!(saved.tours, TourVisibility::AlwaysHide);
    Ok(())
}
