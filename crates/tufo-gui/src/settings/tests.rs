use tufo_client::desktop::Language;

use super::*;

#[test]
fn a_gui_quit_change_preserves_a_cli_language_change() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let shared = tufo_client::desktop::SettingsFile::at(directory.path().join("config.toml"));
    let file = SettingsFile {
        file: shared.clone(),
    };
    let mut gui_settings = file.load()?;
    shared.update(DesktopSettingsUpdate {
        language: Some(Language::PortugueseBrazil),
        ..DesktopSettingsUpdate::default()
    })?;

    gui_settings.quit_behavior = QuitBehavior::KeepPlaying;
    file.save_quit_behavior(gui_settings.quit_behavior)?;

    let saved = shared.load()?;
    assert_eq!(saved.language, Language::PortugueseBrazil);
    assert_eq!(saved.quit_behavior, QuitBehavior::KeepPlaying);
    Ok(())
}
