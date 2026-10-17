//! The Windows browser folder: CEF's bootstrap executable as `tufo-browser.exe`, which sets up
//! Chromium's sandbox and then loads the host from `tufo-browser.dll` beside it, and Chromium's
//! runtime. The server looks for the host in this folder beside itself.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::cef_distribution;

pub(crate) const FOLDER_NAME: &str = "browser";
const CEF_DISTRIBUTION_FOLDER_NAME: &str = "cef_windows_x86_64";
/// The bootstrap for applications without a console, so Chromium's processes open none.
const BOOTSTRAP_NAME: &str = "bootstrap.exe";
const HOST_EXECUTABLE_NAME: &str = "tufo-browser.exe";
/// The bootstrap loads the library named like itself.
const HOST_LIBRARY_NAME: &str = "tufo-browser.dll";
/// Cargo names the library after the crate.
const BUILT_LIBRARY_NAME: &str = "tufo_browser.dll";
const LOCALES_FOLDER_NAME: &str = "locales";
/// The runtime files other than the libraries, packs, and snapshots.
const RUNTIME_FILE_NAMES: [&str; 3] = ["icudtl.dat", "vk_swiftshader_icd.json", "CREDITS.html"];
const RUNTIME_FILE_SUFFIXES: [&str; 3] = [".dll", ".pak", ".bin"];

/// A built host library and the CEF runtime it was built against.
pub(crate) struct BrowserSource {
    library: PathBuf,
    runtime: PathBuf,
}

/// The host library in `binaries` and its unpacked CEF runtime, or `None` without a built
/// library.
pub(crate) fn find_source(workspace_root: &Path, binaries: &Path) -> Result<Option<BrowserSource>> {
    let library = binaries.join(BUILT_LIBRARY_NAME);
    if !library.is_file() {
        return Ok(None);
    }
    let runtime = cef_distribution::directory(
        workspace_root,
        &cef_distribution::pinned_version(workspace_root)?,
        CEF_DISTRIBUTION_FOLDER_NAME,
    );
    ensure!(
        runtime.join(BOOTSTRAP_NAME).is_file(),
        "missing {}; build tufo-browser for Windows to download CEF",
        runtime.join(BOOTSTRAP_NAME).display()
    );
    Ok(Some(BrowserSource { library, runtime }))
}

/// Replaces `folder` with the host and the runtime, so no file of an earlier CEF build stays.
pub(crate) fn place(source: &BrowserSource, folder: &Path) -> Result<()> {
    remove_if_present(folder)?;
    let locales = folder.join(LOCALES_FOLDER_NAME);
    fs::create_dir_all(&locales).with_context(|| format!("create {}", locales.display()))?;
    copy(
        &source.runtime.join(BOOTSTRAP_NAME),
        &folder.join(HOST_EXECUTABLE_NAME),
    )?;
    copy(&source.library, &folder.join(HOST_LIBRARY_NAME))?;
    for path in files(&source.runtime)? {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let wanted = RUNTIME_FILE_NAMES.contains(&name)
            || RUNTIME_FILE_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix));
        if wanted {
            copy(&path, &folder.join(name))?;
        }
    }
    for path in files(&source.runtime.join(LOCALES_FOLDER_NAME))? {
        if let Some(name) = path.file_name() {
            copy(&path, &locales.join(name))?;
        }
    }
    Ok(())
}

/// Lets Chromium's `AppContainer` sandboxes read and run the browser, as ALL APPLICATION PACKAGES
/// and ALL RESTRICTED APPLICATION PACKAGES; a user's folders don't grant them that.
#[cfg(windows)]
pub(crate) fn grant_app_container_access(folder: &Path) -> Result<()> {
    const APP_CONTAINER_GRANTS: [&str; 2] =
        ["*S-1-15-2-1:(OI)(CI)(RX)", "*S-1-15-2-2:(OI)(CI)(RX)"];
    let status = std::process::Command::new("icacls")
        .arg(folder)
        .arg("/grant")
        .args(APP_CONTAINER_GRANTS)
        .args(["/T", "/Q"])
        .status()
        .context("run icacls")?;
    ensure!(
        status.success(),
        "icacls failed to grant {}: {status}",
        folder.display()
    );
    Ok(())
}

/// Removes the folder `place` filled, and names it.
#[cfg(windows)]
pub(crate) fn remove(folder: &Path) -> Result<()> {
    if remove_if_present(folder)? {
        println!("removed {}", folder.display());
    }
    Ok(())
}

/// Whether there was a folder to remove.
fn remove_if_present(folder: &Path) -> Result<bool> {
    match fs::remove_dir_all(folder) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("remove {}", folder.display())),
    }
}

fn files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let path = entry?.path();
        if path.is_file() {
            files.push(path);
        }
    }
    Ok(files)
}

fn copy(source: &Path, destination: &Path) -> Result<()> {
    fs::copy(source, destination)
        .map(drop)
        .with_context(|| format!("copy {}", source.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(root: &Path) -> BrowserSource {
        let runtime = root.join("cef");
        fs::create_dir_all(runtime.join(LOCALES_FOLDER_NAME)).unwrap();
        fs::create_dir_all(runtime.join("include")).unwrap();
        for name in [
            BOOTSTRAP_NAME,
            "bootstrapc.exe",
            "libcef.dll",
            "libcef.lib",
            "chrome_elf.dll",
            "resources.pak",
            "icudtl.dat",
            "v8_context_snapshot.bin",
            "vk_swiftshader_icd.json",
            "CREDITS.html",
            "CMakeLists.txt",
            "archive.json",
        ] {
            fs::write(runtime.join(name), name).unwrap();
        }
        fs::write(runtime.join("locales/pt-BR.pak"), "pt-BR").unwrap();
        let library = root.join(BUILT_LIBRARY_NAME);
        fs::write(&library, "host").unwrap();
        BrowserSource { library, runtime }
    }

    #[test]
    fn place_renames_the_bootstrap_and_library_beside_the_runtime() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join(FOLDER_NAME);

        place(&source(root.path()), &folder).unwrap();

        assert_eq!(
            fs::read_to_string(folder.join(HOST_EXECUTABLE_NAME)).unwrap(),
            BOOTSTRAP_NAME
        );
        assert_eq!(
            fs::read_to_string(folder.join(HOST_LIBRARY_NAME)).unwrap(),
            "host"
        );
        for name in [
            "libcef.dll",
            "chrome_elf.dll",
            "resources.pak",
            "icudtl.dat",
            "v8_context_snapshot.bin",
            "vk_swiftshader_icd.json",
            "CREDITS.html",
            "locales/pt-BR.pak",
        ] {
            assert!(folder.join(name).is_file(), "{name} is missing");
        }
        for name in [
            BOOTSTRAP_NAME,
            "bootstrapc.exe",
            "libcef.lib",
            "CMakeLists.txt",
            "archive.json",
            "include",
        ] {
            assert!(!folder.join(name).exists(), "{name} was copied");
        }
    }

    #[test]
    fn placing_again_drops_files_of_the_earlier_build() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join(FOLDER_NAME);
        let source = source(root.path());
        place(&source, &folder).unwrap();
        fs::write(folder.join("stale.pak"), "stale").unwrap();

        place(&source, &folder).unwrap();

        assert!(!folder.join("stale.pak").exists());
        assert!(folder.join(HOST_EXECUTABLE_NAME).is_file());
    }

    #[test]
    fn a_build_without_the_library_has_no_browser() {
        let root = tempfile::tempdir().unwrap();

        assert!(find_source(root.path(), root.path()).unwrap().is_none());
    }
}
