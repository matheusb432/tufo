use super::*;

fn settings_file(directory: &tempfile::TempDir) -> SettingsFile {
    SettingsFile::at(directory.path().join("tufo").join(SETTINGS_FILE_NAME))
}

#[test]
fn a_missing_file_holds_the_defaults() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let file = settings_file(&directory);
    let settings = file.load()?;
    assert_eq!(settings.quit_behavior, QuitBehavior::StandBy);
    assert_eq!(settings.language, Language::System);
    assert_eq!(settings.tours, TourVisibility::ShowUnseen);
    assert_eq!(settings.sidebar_hidden_pages.iter().count(), 0);
    assert!(!file.path.exists());
    Ok(())
}

#[test]
fn an_unknown_key_is_an_error_that_names_the_file() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let file = settings_file(&directory);
    std::fs::create_dir_all(directory.path().join("tufo"))?;
    std::fs::write(&file.path, "[quit]\nkeep_playng = true\n")?;
    let Err(error) = file.load() else {
        anyhow::bail!("settings with an unknown key were accepted");
    };

    assert!(matches!(error, SettingsError::Invalid { .. }));
    assert!(error.to_string().contains(&file.path.display().to_string()));
    Ok(())
}

#[test]
fn updates_never_replace_an_invalid_file() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let file = settings_file(&directory);
    std::fs::create_dir_all(directory.path().join("tufo"))?;
    let hand_edit = "[quit]\nkeep_playing = yes\n";
    std::fs::write(&file.path, hand_edit)?;

    assert!(file.update(DesktopSettingsUpdate::default()).is_err());
    assert!(
        file.update(DesktopSettingsUpdate {
            quit_behavior: Some(QuitBehavior::KeepPlaying),
            ..DesktopSettingsUpdate::default()
        })
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&file.path)?, hand_edit);
    Ok(())
}

#[test]
fn a_contended_settings_lock_times_out_and_can_be_reacquired_after_release() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let file = settings_file(&directory);
    let timeout = Duration::from_millis(10);
    let holder = file.lock(SETTINGS_LOCK_TIMEOUT)?;

    let started = Instant::now();
    let Err(SettingsError::Lock { source, .. }) = file.lock(timeout) else {
        anyhow::bail!("a held settings lock did not return a lock error");
    };

    assert_eq!(source.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() >= timeout);
    assert!(!file.path.exists());

    drop(holder);
    let _settings_lock = file.lock(SETTINGS_LOCK_TIMEOUT)?;
    Ok(())
}

#[test]
fn partial_updates_preserve_omitted_fields_and_allow_defaults() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let file = settings_file(&directory);
    let hidden_pages = SidebarPages::default().with(SidebarPage::Browser, true);
    file.update(DesktopSettingsUpdate {
        language: Some(Language::PortugueseBrazil),
        quit_behavior: Some(QuitBehavior::KeepPlaying),
        tours: Some(TourVisibility::AlwaysHide),
        sidebar_hidden_pages: SidebarPagesPatch::replace(hidden_pages),
    })?;

    let settings = file.update(DesktopSettingsUpdate {
        quit_behavior: Some(QuitBehavior::StandBy),
        ..DesktopSettingsUpdate::default()
    })?;
    assert_eq!(settings.language, Language::PortugueseBrazil);
    assert_eq!(settings.quit_behavior, QuitBehavior::StandBy);
    assert_eq!(settings.tours, TourVisibility::AlwaysHide);
    assert_eq!(settings.sidebar_hidden_pages, hidden_pages);
    assert_eq!(file.load()?, settings);

    file.update(DesktopSettingsUpdate {
        quit_behavior: Some(QuitBehavior::KeepPlaying),
        ..DesktopSettingsUpdate::default()
    })?;
    let settings = file.update(DesktopSettingsUpdate {
        language: Some(Language::System),
        ..DesktopSettingsUpdate::default()
    })?;
    assert_eq!(settings.language, Language::System);
    assert_eq!(settings.quit_behavior, QuitBehavior::KeepPlaying);
    assert_eq!(settings.tours, TourVisibility::AlwaysHide);
    assert_eq!(file.load()?, settings);
    Ok(())
}

#[test]
fn an_empty_update_preserves_a_hand_edit_and_does_not_create_a_missing_file() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let file = settings_file(&directory);
    file.update(DesktopSettingsUpdate::default())?;
    assert!(!file.path.exists());
    assert!(!directory.path().join("tufo").exists());

    std::fs::create_dir_all(directory.path().join("tufo"))?;
    let hand_edit = "# keep this comment\nlanguage = \"pt_BR\"\n";
    std::fs::write(&file.path, hand_edit)?;
    file.update(DesktopSettingsUpdate::default())?;
    assert_eq!(std::fs::read_to_string(&file.path)?, hand_edit);
    assert_eq!(std::fs::read_dir(directory.path().join("tufo"))?.count(), 1);
    Ok(())
}

#[test]
fn unknown_settings_fields_and_languages_are_rejected() {
    assert!(parse("language = \"pt-BRR\"").is_err());
    assert!(parse("langauge = \"en\"\n").is_err());
    assert!(parse("[quit]\nkeep_playing = true\nunknown = true\n").is_err());
    assert!(parse("tours = \"sometimes\"\n").is_err());
    assert!(parse("[sidebar]\nhidden_pages = [\"settings\"]\n").is_err());
    assert!(parse("[sidebar]\nhidden_pages = \"songs\"\n").is_err());
    assert!(parse("[sidebar]\nhiden_pages = [\"songs\"]\n").is_err());
}

#[test]
fn hiding_one_sidebar_page_keeps_the_others_and_every_other_setting() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let file = settings_file(&directory);
    std::fs::create_dir_all(directory.path().join("tufo"))?;
    std::fs::write(
        &file.path,
        "language = \"pt-BR\"\n[quit]\nkeep_playing = true\n\
         [sidebar]\nhidden_pages = [\"soundboards\", \"home\", \"home\"]\n",
    )?;
    let hidden =
        |settings: DesktopSettings| settings.sidebar_hidden_pages.iter().collect::<Vec<_>>();
    assert_eq!(
        hidden(file.load()?),
        [SidebarPage::Home, SidebarPage::Soundboards]
    );

    let settings = file.update(DesktopSettingsUpdate {
        sidebar_hidden_pages: SidebarPagesPatch::default()
            .set(SidebarPage::Songs, true)
            .set(SidebarPage::Home, false),
        ..DesktopSettingsUpdate::default()
    })?;
    assert_eq!(
        hidden(settings),
        [SidebarPage::Songs, SidebarPage::Soundboards]
    );
    assert_eq!(settings.language, Language::PortugueseBrazil);
    assert_eq!(settings.quit_behavior, QuitBehavior::KeepPlaying);
    assert_eq!(file.load()?, settings);

    let settings = file.update(DesktopSettingsUpdate {
        sidebar_hidden_pages: SidebarPagesPatch::replace(SidebarPages::default()),
        ..DesktopSettingsUpdate::default()
    })?;
    assert_eq!(hidden(settings), []);
    assert_eq!(file.load()?, settings);
    Ok(())
}

#[test]
fn tour_visibility_round_trips_and_old_settings_default_to_unseen() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let file = settings_file(&directory);
    std::fs::create_dir_all(directory.path().join("tufo"))?;
    std::fs::write(
        &file.path,
        "language = \"pt-BR\"\n[quit]\nkeep_playing = true\n",
    )?;
    assert_eq!(file.load()?.tours, TourVisibility::ShowUnseen);

    for tours in [
        TourVisibility::AlwaysShow,
        TourVisibility::AlwaysHide,
        TourVisibility::ShowUnseen,
    ] {
        let settings = file.update(DesktopSettingsUpdate {
            tours: Some(tours),
            ..DesktopSettingsUpdate::default()
        })?;
        assert_eq!(file.load()?, settings);
        assert_eq!(settings.tours, tours);
        assert_eq!(settings.language, Language::PortugueseBrazil);
        assert_eq!(settings.quit_behavior, QuitBehavior::KeepPlaying);
    }
    Ok(())
}

#[test]
fn language_override_and_system_fallback() {
    for locale in ["pt-BR", "pt_BR.UTF-8", "pt", "pt-PT"] {
        assert_eq!(
            Language::System.resolve([locale.to_owned()]),
            Language::PortugueseBrazil
        );
    }
    assert_eq!(
        Language::English.resolve(["pt-BR".to_owned()]),
        Language::English
    );
    assert_eq!(
        Language::System.resolve(["de-DE".to_owned()]),
        Language::English
    );
    assert_eq!(
        Language::System.resolve(["en-US".to_owned(), "pt-BR".to_owned()]),
        Language::English
    );
}
