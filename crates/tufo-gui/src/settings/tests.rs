use super::*;

fn settings_file(directory: &tempfile::TempDir) -> SettingsFile {
    SettingsFile::at(directory.path().join("tufo").join(SETTINGS_FILE_NAME))
}

const KEEP_PLAYING: DesktopSettings = DesktopSettings {
    quit_behavior: QuitBehavior::KeepPlaying,
    language: Language::System,
};

#[test]
fn a_missing_file_holds_the_defaults() {
    let directory = tempfile::tempdir().unwrap();

    let settings = settings_file(&directory).load().unwrap();

    assert_eq!(settings.quit_behavior, QuitBehavior::StandBy);
}

#[test]
fn saved_settings_load_back() {
    let directory = tempfile::tempdir().unwrap();
    let file = settings_file(&directory);

    file.save(KEEP_PLAYING).unwrap();
    assert_eq!(file.load().unwrap(), KEEP_PLAYING);
    file.save(DesktopSettings::default()).unwrap();
    assert_eq!(file.load().unwrap(), DesktopSettings::default());
}

#[test]
fn language_override_survives_saving_other_settings() {
    let directory = tempfile::tempdir().unwrap();
    let file = settings_file(&directory);
    let settings = parse("language = \"pt-BR\"\n[quit]\nkeep_playing = true\n").unwrap();
    assert_eq!(settings.language, Language::PortugueseBrazil);
    file.save(settings).unwrap();
    assert_eq!(file.load().unwrap(), settings);
    assert_eq!(
        parse("language = \"en\"").unwrap().language,
        Language::English
    );
    assert!(parse("language = \"pt-BRR\"").is_err());
}

#[test]
fn keep_playing_leaves_the_audio_playing_on_quit() {
    assert_eq!(
        parse("[quit]\nkeep_playing = true\n").unwrap(),
        KEEP_PLAYING
    );
    assert_eq!(parse("").unwrap(), DesktopSettings::default());
}

#[test]
fn an_unknown_key_is_an_error_that_names_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let file = settings_file(&directory);
    std::fs::create_dir_all(file.path.parent().unwrap()).unwrap();
    std::fs::write(&file.path, "[quit]\nkeep_playng = true\n").unwrap();

    let error = file.load().unwrap_err();

    assert!(matches!(error, SettingsError::Invalid { .. }));
    assert!(error.to_string().contains(&file.path.display().to_string()));
}

#[test]
fn saving_never_replaces_a_file_it_cannot_read() {
    let directory = tempfile::tempdir().unwrap();
    let file = settings_file(&directory);
    std::fs::create_dir_all(file.path.parent().unwrap()).unwrap();
    let hand_edit = "[quit]\nkeep_playing = yes\n";
    std::fs::write(&file.path, hand_edit).unwrap();

    assert!(file.save(KEEP_PLAYING).is_err());
    assert_eq!(std::fs::read_to_string(&file.path).unwrap(), hand_edit);
}
