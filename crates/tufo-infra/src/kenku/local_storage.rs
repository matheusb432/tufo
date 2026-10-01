//! Chromium's `localStorage`, read from a copy so Kenku FM's own files stay untouched.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use rusty_leveldb::{DB, Options};

// POSIX record locks belong to the process, so serialize copies even for direct reader calls.
static SOURCE_COPY_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    #[error("Kenku FM is running")]
    Running,
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
        let _copy_gate = SOURCE_COPY_GATE.lock().map_err(|_| {
            OpenLocalStorageError::Copy(io::Error::other("the Kenku storage reader stopped"))
        })?;
        let source = LOCAL_STORAGE_FOLDER
            .iter()
            .fold(app_folder.to_path_buf(), |path, part| path.join(part));
        if !source.is_dir() {
            return Err(OpenLocalStorageError::NotFound);
        }
        // Hold Chromium's native lock throughout the snapshot. Kenku cannot start changing
        // its manifests or tables halfway through the copy.
        let _lock = lock_source(&source.join(LOCK_FILE_NAME))?;
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

fn lock_source(path: &Path) -> Result<fs::File, OpenLocalStorageError> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(lock_open_error)?;
    try_lock_chromium(&file).map_err(|error| match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied => {
            OpenLocalStorageError::Running
        }
        _ => OpenLocalStorageError::Copy(error),
    })?;
    Ok(file)
}

fn lock_open_error(error: io::Error) -> OpenLocalStorageError {
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(32 | 33)) {
        return OpenLocalStorageError::Running;
    }
    OpenLocalStorageError::Copy(error)
}

#[cfg(unix)]
fn try_lock_chromium(file: &fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;

    // Chromium uses process-owned POSIX record locks, not flock (used by File::try_lock).
    // SAFETY: an all-zero flock is valid on the supported Unix targets; the fields below
    // describe a whole-file, nonblocking exclusive lock. The FD and pointer stay valid.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::c_short::try_from(libc::F_WRLCK).map_err(io::Error::other)?;
    lock.l_whence = libc::c_short::try_from(libc::SEEK_SET).map_err(io::Error::other)?;
    // SAFETY: fcntl reads this initialized flock and does not retain its pointer.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn try_lock_chromium(file: &fs::File) -> io::Result<()> {
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
        fs::TryLockError::Error(error) => error,
    })
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
    fn items_come_back_from_a_closed_copy_without_changing_original_files() {
        let app = tempfile::tempdir().unwrap();
        write_file_origin_items(app.path(), &[("persist:root", "{\"a\":\"é♫\"}")]).unwrap();
        let source = LOCAL_STORAGE_FOLDER
            .iter()
            .fold(app.path().to_path_buf(), |path, part| path.join(part));
        let original = files_to_copy(&source)
            .unwrap()
            .into_iter()
            .map(|path| {
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect::<Vec<_>>();

        let mut storage = LocalStorageCopy::open(app.path()).unwrap();

        assert_eq!(
            storage.file_origin_item("persist:root").as_deref(),
            Some("{\"a\":\"é♫\"}")
        );
        assert_eq!(storage.file_origin_item("persist:player"), None);
        for (path, bytes) in original {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn chromium_lock_holder() {
        use std::io::Read as _;
        let Some(folder) = std::env::var_os("TUFO_KENKU_TEST_LOCK") else {
            return;
        };
        let folder = PathBuf::from(folder);
        let _lock = lock_source(&folder.join(LOCK_FILE_NAME)).unwrap();
        fs::write(folder.join("ready"), b"ready").unwrap();
        std::io::stdin().read_exact(&mut [0]).unwrap();
    }

    #[test]
    fn a_running_chromium_writer_is_rejected_until_it_closes() {
        use std::{
            io::Write as _,
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let app = tempfile::tempdir().unwrap();
        write_file_origin_items(app.path(), &[("persist:root", "{}")]).unwrap();
        let folder = app.path().join("Local Storage").join("leveldb");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "kenku::local_storage::tests::chromium_lock_holder",
            ])
            .env("TUFO_KENKU_TEST_LOCK", &folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !folder.join("ready").exists() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                assert!(
                    Instant::now() < deadline,
                    "the native lock holder did not start"
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(
            LocalStorageCopy::open(app.path()),
            Err(OpenLocalStorageError::Running)
        ));
        child.stdin.take().unwrap().write_all(b"x").unwrap();
        assert!(child.wait().unwrap().success());
        assert!(LocalStorageCopy::open(app.path()).is_ok());
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
