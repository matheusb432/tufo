//! The browser host and Chromium's runtime, in their own folder beside `bin`, with a link in `bin`
//! so the server finds the host beside itself. The host loads `libcef.so` and Chromium's resources
//! from the folder that holds it.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, ensure};

use crate::cef_distribution;

pub(super) const HOST_NAME: &str = "tufo-browser";
/// Records which CEF build the folder holds, so updates copy the runtime only when it changes.
const RUNTIME_STAMP_NAME: &str = ".cef-runtime";
const LIBCEF_NAME: &str = "libcef.so";
const LOCALES_FOLDER_NAME: &str = "locales";
/// The runtime files beside `libcef.so`, other than the libraries, packs, and snapshots.
const RUNTIME_FILE_NAMES: [&str; 4] = [
    "icudtl.dat",
    "vk_swiftshader_icd.json",
    "chrome-sandbox",
    "CREDITS.html",
];
const RUNTIME_FILE_SUFFIXES: [&str; 4] = [".so", ".so.1", ".pak", ".bin"];

/// A built host and the CEF runtime it was built against.
pub(super) struct BrowserSource {
    pub(super) host: PathBuf,
    pub(super) runtime: PathBuf,
    pub(super) cef_version: String,
}

/// The release build's host and its unpacked CEF runtime, or `None` without a built host.
pub(super) fn find_source(
    workspace_root: &Path,
    release_dir: &Path,
) -> Result<Option<BrowserSource>> {
    let host = release_dir.join(HOST_NAME);
    if !host.is_file() {
        return Ok(None);
    }
    let cef_version = cef_distribution::pinned_version(workspace_root)?;
    let runtime = cef_distribution::directory(
        workspace_root,
        &cef_version,
        &format!("cef_linux_{}", std::env::consts::ARCH),
    );
    ensure!(
        runtime.join(LIBCEF_NAME).is_file(),
        "missing {}; build tufo-browser to download CEF",
        runtime.display()
    );
    Ok(Some(BrowserSource {
        host,
        runtime,
        cef_version,
    }))
}

/// Places the host in `folder`, the runtime once per CEF build, and the link in `binaries`.
pub(super) fn install(source: &BrowserSource, folder: &Path, binaries: &Path) -> Result<()> {
    fs::create_dir_all(folder).with_context(|| format!("create {}", folder.display()))?;
    let stamp = folder.join(RUNTIME_STAMP_NAME);
    let installed = fs::read_to_string(&stamp).unwrap_or_default();
    if installed != source.cef_version {
        copy_runtime(&source.runtime, folder)?;
        fs::write(&stamp, &source.cef_version)
            .with_context(|| format!("write {}", stamp.display()))?;
    }
    super::super::replace_file(&source.host, &folder.join(HOST_NAME))?;
    let link = binaries.join(HOST_NAME);
    match fs::remove_file(&link) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("remove {}", link.display())),
    }
    std::os::unix::fs::symlink(folder.join(HOST_NAME), &link)
        .with_context(|| format!("link {}", link.display()))
}

fn copy_runtime(runtime: &Path, folder: &Path) -> Result<()> {
    for entry in fs::read_dir(runtime).with_context(|| format!("read {}", runtime.display()))? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let wanted = RUNTIME_FILE_NAMES.contains(&name)
            || RUNTIME_FILE_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix));
        if !path.is_file() || !wanted {
            continue;
        }
        let destination = folder.join(name);
        if name == LIBCEF_NAME {
            strip_into(&path, &destination)?;
        } else {
            super::super::replace_file(&path, &destination)?;
        }
    }
    let locales = folder.join(LOCALES_FOLDER_NAME);
    fs::create_dir_all(&locales).with_context(|| format!("create {}", locales.display()))?;
    let source_locales = runtime.join(LOCALES_FOLDER_NAME);
    for entry in fs::read_dir(&source_locales)
        .with_context(|| format!("read {}", source_locales.display()))?
    {
        let path = entry?.path();
        if let Some(name) = path.file_name() {
            super::super::replace_file(&path, &locales.join(name))?;
        }
    }
    Ok(())
}

/// CEF ships `libcef.so` with its debug information, five times its stripped size.
fn strip_into(library: &Path, destination: &Path) -> Result<()> {
    let mut staged = destination.as_os_str().to_owned();
    staged.push(".installing");
    let stripped = Command::new("strip")
        .arg("--strip-unneeded")
        .arg("-o")
        .arg(&staged)
        .arg(library)
        .status()
        .is_ok_and(|status| status.success());
    if !stripped {
        println!("strip is unavailable or failed; installing libcef.so with its debug information");
        return super::super::replace_file(library, destination);
    }
    fs::rename(&staged, destination).with_context(|| format!("replace {}", destination.display()))
}

pub(super) fn uninstall(folder: &Path, binaries: &Path) -> Result<()> {
    super::super::remove_files(&[&binaries.join(HOST_NAME)])?;
    match fs::remove_dir_all(folder) {
        Ok(()) => {
            println!("removed {}", folder.display());
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", folder.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(root: &Path) -> PathBuf {
        let runtime = root.join("cef");
        fs::create_dir_all(runtime.join(LOCALES_FOLDER_NAME)).unwrap();
        fs::create_dir_all(runtime.join("include")).unwrap();
        for name in [
            LIBCEF_NAME,
            "libvk_swiftshader.so",
            "resources.pak",
            "icudtl.dat",
            "v8_context_snapshot.bin",
            "CREDITS.html",
            "CMakeLists.txt",
            "archive.json",
        ] {
            fs::write(runtime.join(name), name).unwrap();
        }
        fs::write(runtime.join("locales/pt-BR.pak"), "pt-BR").unwrap();
        runtime
    }

    fn source(root: &Path) -> BrowserSource {
        let host = root.join(HOST_NAME);
        fs::write(&host, "host").unwrap();
        BrowserSource {
            host,
            runtime: runtime(root),
            cef_version: "154.0.34".to_owned(),
        }
    }

    #[test]
    fn install_places_the_runtime_the_host_and_a_link() {
        let root = tempfile::tempdir().unwrap();
        let (folder, binaries) = (root.path().join("lib"), root.path().join("bin"));
        fs::create_dir_all(&binaries).unwrap();

        install(&source(root.path()), &folder, &binaries).unwrap();

        for name in [
            LIBCEF_NAME,
            "resources.pak",
            "icudtl.dat",
            "locales/pt-BR.pak",
            "CREDITS.html",
        ] {
            assert!(folder.join(name).is_file(), "{name} is missing");
        }
        for name in ["CMakeLists.txt", "archive.json", "include"] {
            assert!(!folder.join(name).exists(), "{name} was copied");
        }
        assert_eq!(
            fs::read_link(binaries.join(HOST_NAME)).unwrap(),
            folder.join(HOST_NAME)
        );
        assert_eq!(
            fs::read_to_string(binaries.join(HOST_NAME)).unwrap(),
            "host"
        );
    }

    #[test]
    fn updates_copy_the_runtime_only_for_a_new_cef_build() {
        let root = tempfile::tempdir().unwrap();
        let (folder, binaries) = (root.path().join("lib"), root.path().join("bin"));
        fs::create_dir_all(&binaries).unwrap();
        let mut source = source(root.path());
        install(&source, &folder, &binaries).unwrap();
        fs::write(source.runtime.join("resources.pak"), "newer").unwrap();
        fs::write(&source.host, "newer host").unwrap();

        install(&source, &folder, &binaries).unwrap();
        assert_eq!(
            fs::read_to_string(folder.join("resources.pak")).unwrap(),
            "resources.pak"
        );
        assert_eq!(
            fs::read_to_string(folder.join(HOST_NAME)).unwrap(),
            "newer host"
        );

        source.cef_version = "155.0.1".to_owned();
        install(&source, &folder, &binaries).unwrap();
        assert_eq!(
            fs::read_to_string(folder.join("resources.pak")).unwrap(),
            "newer"
        );
    }

    #[test]
    fn uninstall_removes_the_folder_and_the_link() {
        let root = tempfile::tempdir().unwrap();
        let (folder, binaries) = (root.path().join("lib"), root.path().join("bin"));
        fs::create_dir_all(&binaries).unwrap();
        install(&source(root.path()), &folder, &binaries).unwrap();

        uninstall(&folder, &binaries).unwrap();

        assert!(!folder.exists());
        assert!(fs::symlink_metadata(binaries.join(HOST_NAME)).is_err());
    }
}
