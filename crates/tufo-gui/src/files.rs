//! Small files written whole in one rename.

use std::{
    io,
    path::{Path, PathBuf},
};

pub(crate) fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let staged = staged_path(path);
    std::fs::write(&staged, contents)?;
    std::fs::rename(&staged, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

fn staged_path(path: &Path) -> PathBuf {
    let mut staged = path.as_os_str().to_owned();
    staged.push(".staged");
    PathBuf::from(staged)
}

#[cfg(test)]
mod tests;
