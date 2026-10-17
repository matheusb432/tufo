//! The CEF build cef-dll-sys unpacked for the browser host, under `CEF_PATH` or `target/cef`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

/// The CEF build Cargo.lock pins, such as `154.0.34`.
pub(crate) fn pinned_version(workspace_root: &Path) -> Result<String> {
    let lock = fs::read_to_string(workspace_root.join("Cargo.lock")).context("read Cargo.lock")?;
    cef_version(&lock).context("Cargo.lock names no cef-dll-sys build")
}

/// Where `version`'s distribution for one platform, such as `cef_linux_x86_64`, is unpacked,
/// if it is yet.
pub(crate) fn directory(workspace_root: &Path, version: &str, folder_name: &str) -> PathBuf {
    std::env::var_os("CEF_PATH")
        .map_or_else(|| workspace_root.join("target/cef"), PathBuf::from)
        .join(version)
        .join(folder_name)
}

/// The CEF build after `+` in cef-dll-sys's version, such as `154.0.34`.
fn cef_version(lock: &str) -> Option<String> {
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line == r#"name = "cef-dll-sys""# {
            let version = lines
                .next()?
                .strip_prefix(r#"version = ""#)?
                .strip_suffix('"')?;
            return version.split_once('+').map(|(_, cef)| cef.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cef_build_comes_from_the_lock_file() {
        let lock = "[[package]]\nname = \"cef-dll-sys\"\nversion = \"154.5.0+154.0.34\"\n";
        assert_eq!(cef_version(lock).as_deref(), Some("154.0.34"));
        assert_eq!(cef_version("[[package]]\nname = \"cef\"\n"), None);
    }
}
