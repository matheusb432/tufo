//! Absolute paths on the server's filesystem, stored and sent as text.
//!
//! Protobuf strings and SQLite text are UTF-8, so every stored path must be too. Scans skip
//! entries whose names aren't UTF-8 instead of storing a lossy copy that points nowhere.

use std::path::{Path, PathBuf};

use nutype::nutype;

use crate::{playback::AudioFilePath, storage::checked_column};

pub(crate) fn is_absolute_utf8(path: &Path) -> bool {
    path.is_absolute() && path.to_str().is_some()
}

/// An absolute, UTF-8 path to a folder: a dropped one, or one a collection follows.
#[nutype(
    validate(predicate = is_absolute_utf8),
    derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, AsRef)
)]
pub struct FolderPath(PathBuf);

/// An absolute, UTF-8 path whose kind isn't known yet, such as one dropped onto the window.
#[nutype(
    validate(predicate = is_absolute_utf8),
    derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, AsRef)
)]
pub struct LocalPath(PathBuf);

/// An absolute, UTF-8 path to an image a collection uses as its background.
#[nutype(
    validate(predicate = is_absolute_utf8),
    derive(Debug, Clone, PartialEq, Eq, AsRef)
)]
pub struct ImageFilePath(PathBuf);

/// The path's text, which the type guarantees is UTF-8.
fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

checked_column!(
    AudioFilePath,
    String,
    decode = |text| AudioFilePath::try_new(PathBuf::from(text)),
    encode = |path| path_text(path.as_ref()),
);
checked_column!(
    FolderPath,
    String,
    decode = |text| FolderPath::try_new(PathBuf::from(text)),
    encode = |path| path_text(path.as_ref()),
);
checked_column!(
    ImageFilePath,
    String,
    decode = |text| ImageFilePath::try_new(PathBuf::from(text)),
    encode = |path| path_text(path.as_ref()),
);

#[cfg(test)]
mod tests {
    use toasty::{schema::Load as _, stmt::Value};

    use super::*;

    #[test]
    fn stored_paths_decode_only_when_absolute() {
        let absolute = if cfg!(windows) {
            r"C:\music\rpg"
        } else {
            "/music/rpg"
        };

        assert_eq!(
            FolderPath::load(Value::from(absolute)).unwrap().as_ref(),
            Path::new(absolute)
        );
        assert!(FolderPath::load(Value::from("music/rpg")).is_err());
        assert!(AudioFilePath::load(Value::Null).is_err());
    }
}
