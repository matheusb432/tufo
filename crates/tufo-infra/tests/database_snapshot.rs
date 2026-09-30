use std::fs;

use tufo_infra::database::snapshot::{remove_staged_database, replace_database_file};

#[test]
fn staged_cleanup_is_repeatable_and_keeps_the_live_database() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let live = directory.path().join("tufo.sqlite3");
    let staged = directory.path().join("tufo.sqlite3.import");
    fs::write(&live, b"live")?;
    fs::write(directory.path().join("tufo.sqlite3-wal"), b"live journal")?;
    for name in [
        "tufo.sqlite3.import",
        "tufo.sqlite3.import-wal",
        "tufo.sqlite3.import-shm",
        "tufo.sqlite3.import-journal",
    ] {
        fs::write(directory.path().join(name), b"staged")?;
    }

    remove_staged_database(&staged)?;
    remove_staged_database(&staged)?;

    assert_eq!(fs::read(&live)?, b"live");
    assert_eq!(
        fs::read(directory.path().join("tufo.sqlite3-wal"))?,
        b"live journal"
    );
    assert_eq!(fs::read_dir(directory.path())?.count(), 2);
    Ok(())
}

#[test]
fn replacing_a_database_removes_old_journals_and_keeps_other_files() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let live = directory.path().join("tufo.sqlite3");
    let staged = directory.path().join("tufo.sqlite3.import");
    fs::write(&live, b"old")?;
    fs::write(&staged, b"new")?;
    fs::write(directory.path().join("config.toml"), b"settings")?;
    for name in [
        "tufo.sqlite3-wal",
        "tufo.sqlite3-shm",
        "tufo.sqlite3-journal",
    ] {
        fs::write(directory.path().join(name), b"old journal")?;
    }

    replace_database_file(&staged, &live)?;

    assert_eq!(fs::read(&live)?, b"new");
    assert!(!staged.exists());
    assert_eq!(fs::read(directory.path().join("config.toml"))?, b"settings");
    assert_eq!(fs::read_dir(directory.path())?.count(), 2);
    Ok(())
}
