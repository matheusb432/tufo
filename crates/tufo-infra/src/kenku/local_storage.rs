//! Chromium's `localStorage`, read from a copy so Kenku FM's own files stay untouched.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use rusty_leveldb::{DB, Options};

const LOCAL_STORAGE_FOLDER: [&str; 2] = ["Local Storage", "leveldb"];
/// Held by Chromium while Kenku FM runs.
const LOCK_FILE_NAME: &str = "LOCK";
/// Every site Kenku FM's browser tabs visited shares the database.
const LOCAL_STORAGE_BYTES_MAX: u64 = 256 * 1024 * 1024;
const LOCAL_STORAGE_FILES_MAX: usize = 10_000;
/// `_`, the origin, NUL, and the Latin-1 marker that precede each key.
const FILE_ORIGIN_KEY_PREFIX: &[u8] = b"_file://\x00\x01";
/// The first byte of each value.
const UTF16_VALUE_FORMAT: u8 = 0;
const LATIN1_VALUE_FORMAT: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub(super) enum OpenLocalStorageError {
    #[error("the folder has no Local Storage")]
    NotFound,
    #[error("Local Storage holds more than {LOCAL_STORAGE_BYTES_MAX} bytes")]
    TooLarge,
    #[error("cannot copy Local Storage")]
    Copy(#[source] io::Error),
    #[error("cannot open Local Storage's LevelDB")]
    Open(#[source] rusty_leveldb::Status),
}

/// Deleted on drop.
pub(super) struct LocalStorageCopy {
    database: DB,
    _folder: tempfile::TempDir,
}

impl LocalStorageCopy {
    pub(super) fn open(app_folder: &Path) -> Result<Self, OpenLocalStorageError> {
        let source = LOCAL_STORAGE_FOLDER
            .iter()
            .fold(app_folder.to_path_buf(), |path, part| path.join(part));
        if !source.is_dir() {
            return Err(OpenLocalStorageError::NotFound);
        }
        let copy = tempfile::tempdir().map_err(OpenLocalStorageError::Copy)?;
        for file in files_to_copy(&source)? {
            let Some(name) = file.file_name() else {
                continue;
            };
            fs::copy(&file, copy.path().join(name)).map_err(OpenLocalStorageError::Copy)?;
        }
        let options = Options {
            create_if_missing: false,
            ..Options::default()
        };
        let database = DB::open(copy.path(), options).map_err(OpenLocalStorageError::Open)?;
        Ok(Self {
            database,
            _folder: copy,
        })
    }

    pub(super) fn file_origin_item(&mut self, key: &str) -> Option<String> {
        let mut stored_key = FILE_ORIGIN_KEY_PREFIX.to_vec();
        stored_key.extend(key.bytes());
        decode_value(&self.database.get(&stored_key)?)
    }
}

fn files_to_copy(source: &Path) -> Result<Vec<PathBuf>, OpenLocalStorageError> {
    let mut files = Vec::new();
    let mut bytes_total = 0_u64;
    for entry in fs::read_dir(source).map_err(OpenLocalStorageError::Copy)? {
        let entry = entry.map_err(OpenLocalStorageError::Copy)?;
        let metadata = entry.metadata().map_err(OpenLocalStorageError::Copy)?;
        if !metadata.is_file() || entry.file_name() == LOCK_FILE_NAME {
            continue;
        }
        bytes_total = bytes_total.saturating_add(metadata.len());
        if bytes_total > LOCAL_STORAGE_BYTES_MAX || files.len() >= LOCAL_STORAGE_FILES_MAX {
            return Err(OpenLocalStorageError::TooLarge);
        }
        files.push(entry.path());
    }
    Ok(files)
}

fn decode_value(stored: &[u8]) -> Option<String> {
    let (&format, bytes) = stored.split_first()?;
    match format {
        UTF16_VALUE_FORMAT => {
            let (pairs, remainder) = bytes.as_chunks::<2>();
            if !remainder.is_empty() {
                return None;
            }
            let units = pairs.iter().copied().map(u16::from_le_bytes);
            char::decode_utf16(units).collect::<Result<_, _>>().ok()
        }
        LATIN1_VALUE_FORMAT => Some(bytes.iter().copied().map(char::from).collect()),
        _ => None,
    }
}

/// Writes `items` the way Chromium does.
#[cfg(any(test, feature = "test-support"))]
pub fn write_file_origin_items(app_folder: &Path, items: &[(&str, &str)]) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let folder = LOCAL_STORAGE_FOLDER
        .iter()
        .fold(app_folder.to_path_buf(), |path, part| path.join(part));
    fs::create_dir_all(&folder).context("creating Local Storage")?;
    let mut database = DB::open(&folder, Options::default())
        .map_err(|status| anyhow::anyhow!("opening Local Storage: {status}"))?;
    for (key, value) in items {
        let mut stored_key = FILE_ORIGIN_KEY_PREFIX.to_vec();
        stored_key.extend(key.bytes());
        let stored_value = if value.chars().all(|character| u32::from(character) <= 0xFF) {
            std::iter::once(LATIN1_VALUE_FORMAT)
                .chain(value.chars().map(|character| character as u8))
                .collect::<Vec<_>>()
        } else {
            std::iter::once(UTF16_VALUE_FORMAT)
                .chain(value.encode_utf16().flat_map(u16::to_le_bytes))
                .collect()
        };
        database
            .put(&stored_key, &stored_value)
            .map_err(|status| anyhow::anyhow!("writing {key}: {status}"))?;
    }
    database
        .close()
        .map_err(|status| anyhow::anyhow!("closing Local Storage: {status}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_decode_from_both_chromium_formats() {
        let utf16 = std::iter::once(UTF16_VALUE_FORMAT)
            .chain("Ação ♫".encode_utf16().flat_map(u16::to_le_bytes))
            .collect::<Vec<_>>();
        let latin1 = [LATIN1_VALUE_FORMAT, b'o', 0xE9];

        assert_eq!(decode_value(&utf16).as_deref(), Some("Ação ♫"));
        assert_eq!(decode_value(&latin1).as_deref(), Some("oé"));
        assert_eq!(decode_value(&[UTF16_VALUE_FORMAT, 0x41]), None);
        assert_eq!(decode_value(&[7, b'a']), None);
    }

    #[test]
    fn items_come_back_from_a_copy_while_the_original_stays_locked() {
        let app = tempfile::tempdir().unwrap();
        write_file_origin_items(app.path(), &[("persist:root", "{\"a\":\"é♫\"}")]).unwrap();
        let lock = LOCAL_STORAGE_FOLDER
            .iter()
            .fold(app.path().to_path_buf(), |path, part| path.join(part))
            .join(LOCK_FILE_NAME);
        let held = fs::File::open(&lock).unwrap();
        held.lock().unwrap();

        let mut storage = LocalStorageCopy::open(app.path()).unwrap();

        assert_eq!(
            storage.file_origin_item("persist:root").as_deref(),
            Some("{\"a\":\"é♫\"}")
        );
        assert_eq!(storage.file_origin_item("persist:player"), None);
    }

    #[test]
    fn a_folder_without_local_storage_is_not_found() {
        let app = tempfile::tempdir().unwrap();

        assert!(matches!(
            LocalStorageCopy::open(app.path()),
            Err(OpenLocalStorageError::NotFound)
        ));
    }
}
