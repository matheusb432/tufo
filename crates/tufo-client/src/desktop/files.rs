//! Small desktop files replaced in one rename.

use std::{io, io::Write as _, path::Path};

use tempfile::NamedTempFile;

pub fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let folder = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(folder)?;
    let mut staged = NamedTempFile::new_in(folder)?;
    staged.write_all(contents.as_bytes())?;
    staged
        .persist(path)
        .map(|_| ())
        .map_err(|error| error.error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_creates_the_folder_and_leaves_no_staged_file() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("tufo").join("window.toml");

        write_atomically(&path, "first")?;
        write_atomically(&path, "second")?;

        assert_eq!(std::fs::read_to_string(&path)?, "second");
        assert_eq!(std::fs::read_dir(directory.path().join("tufo"))?.count(), 1);
        Ok(())
    }

    #[test]
    fn a_failed_replacement_removes_the_staging_file() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("destination");
        std::fs::create_dir(&path)?;

        assert!(write_atomically(&path, "contents").is_err());

        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_replacement_is_private_to_its_owner() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.toml");
        std::fs::write(&path, "original")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;

        write_atomically(&path, "replacement")?;

        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        Ok(())
    }
}
