//! Cross-builds the Windows executables and the browser host's library, and packs them with
//! Chromium's runtime into an unsigned NSIS installer.

use std::{fs, io, path::Path, process::Command};

use anyhow::{Context as _, Result, bail, ensure};
use cargo_metadata::{Metadata, semver::Version};

use crate::{install::BINARY_NAMES, release, windows_browser};

const TARGET: &str = "x86_64-pc-windows-msvc";
const PACKAGES: [&str; 3] = ["tufo-gui", "tufo-server", "tufo-cli"];
const BROWSER_PACKAGE: &str = "tufo-browser";
const INSTALLER_VERSION_PACKAGE: &str = "tufo-gui";
const INSTALLER_SCRIPT: &str = "packaging/windows/tufo.nsi";
const LICENSES_FOLDER_NAME: &str = "licenses";
const CMAKE_TOOLCHAIN: &str = "packaging/windows/cross-toolchain.cmake";
const OPUS_CMAKE_FILES: [&str; 2] = [CMAKE_TOOLCHAIN, "packaging/windows/opus-clang-cl.cmake"];
/// A copy of the files and C flags that configured the cross-built Opus.
const OPUS_CMAKE_STAMP: &str = "xwin-opus-cmake.stamp";
/// `libopus_sys` links libm whenever its build script runs on Unix, whatever the target.
const LIBM_STUB_DIR: &str = "xwin-link-stubs";
const EMPTY_ARCHIVE: &[u8] = b"!<arch>\n";
/// Spares installed machines the Visual C++ Redistributable.
const STATIC_CRT_FLAG: &str = "-C target-feature=+crt-static";
/// cargo-xwin's download lacks the static runtime's debug files.
const IGNORE_MISSING_PDB_FLAG: &str = "-C link-arg=/IGNORE:4099";
/// clang-cl reads extra options from `CL`. cargo-xwin's clang-cl may be older than the MSVC
/// standard library it downloads, which CEF's C++ wrapper includes.
const CLANG_CL_OPTIONS: (&str, &str) = ("CL", "/D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH");

pub(crate) fn build(workspace_root: &Path, metadata: &Metadata) -> Result<()> {
    let version = &metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == INSTALLER_VERSION_PACKAGE)
        .with_context(|| format!("the workspace has no {INSTALLER_VERSION_PACKAGE} package"))?
        .version;
    let target_dir = metadata.target_directory.as_std_path();
    clean_opus_after_cmake_changes(workspace_root, target_dir)?;
    cross_build(workspace_root, target_dir)?;

    let binaries = target_dir.join(TARGET).join("release");
    for name in BINARY_NAMES {
        let binary = binaries.join(format!("{name}.exe"));
        ensure!(
            binary.is_file(),
            "the cross-build left no {}",
            binary.display()
        );
    }
    let browser_source = windows_browser::find_source(workspace_root, &binaries)?
        .context("the cross-build left no browser host library")?;
    let browser = binaries.join(windows_browser::FOLDER_NAME);
    windows_browser::place(&browser_source, &browser)?;
    let licenses = binaries.join(LICENSES_FOLDER_NAME);
    match fs::remove_dir_all(&licenses) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("remove {}", licenses.display())),
    }
    release::stage_notices(workspace_root, &licenses)?;
    let output_dir = workspace_root.join(release::OUTPUT_DIR);
    fs::create_dir_all(&output_dir).with_context(|| format!("create {}", output_dir.display()))?;
    let installer = output_dir.join(format!("tufo-{version}-windows-x86_64-setup.exe"));
    pack(
        workspace_root,
        &InstallerInputs {
            binaries: &binaries,
            browser: &browser,
            licenses: &licenses,
        },
        version,
        &installer,
    )?;
    release::write_checksum(&installer)?;
    println!("built the installer {}", installer.display());
    Ok(())
}

/// `libopus_sys` builds again only when its own sources change.
fn clean_opus_after_cmake_changes(workspace_root: &Path, target_dir: &Path) -> Result<()> {
    let mut cmake_files = Vec::new();
    for file in OPUS_CMAKE_FILES {
        let path = workspace_root.join(file);
        cmake_files.extend(fs::read(&path).with_context(|| format!("read {}", path.display()))?);
    }
    cmake_files.extend(cross_cflags()?.as_bytes());
    let stamp = target_dir.join(OPUS_CMAKE_STAMP);
    if fs::read(&stamp).is_ok_and(|built_from| built_from == cmake_files) {
        return Ok(());
    }
    run(
        Command::new("cargo").current_dir(workspace_root).args([
            "clean",
            "--release",
            "--target",
            TARGET,
            "-p",
            "libopus_sys",
        ]),
        "cargo clean",
    )?;
    fs::write(&stamp, cmake_files).with_context(|| format!("write {}", stamp.display()))
}

fn cross_build(workspace_root: &Path, target_dir: &Path) -> Result<()> {
    let libm_stub_dir = target_dir.join(LIBM_STUB_DIR);
    // Cargo splits target rustflags on whitespace.
    ensure!(
        !libm_stub_dir
            .to_string_lossy()
            .contains(char::is_whitespace),
        "the cross-build needs a target directory without spaces, not {}",
        target_dir.display()
    );
    fs::create_dir_all(&libm_stub_dir)
        .with_context(|| format!("create {}", libm_stub_dir.display()))?;
    fs::write(libm_stub_dir.join("m.lib"), EMPTY_ARCHIVE).context("write the empty m.lib")?;
    let rustflags = format!(
        "{STATIC_CRT_FLAG} {IGNORE_MISSING_PDB_FLAG} -L native={} --remap-path-prefix={}=/usr/src/tufo",
        libm_stub_dir.display(),
        std::env::var("HOME").context("the cross-build needs HOME for private-path remapping")?
    );

    let cross_cargo = |arguments: &[&str]| -> Result<Command> {
        let mut command = Command::new("cargo");
        command
            .current_dir(workspace_root)
            .arg("xwin")
            .args(arguments)
            .args(["--locked", "--release", "--target", TARGET])
            // cargo-xwin merges its own flags into these.
            .env("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS", &rustflags)
            // Only the cross-built C takes these; build scripts' own C, such as download-cef's
            // ring, compiles for Linux.
            .env("TARGET_CFLAGS", cross_cflags()?)
            .env(CLANG_CL_OPTIONS.0, CLANG_CL_OPTIONS.1)
            // The cmake crate prefers this spelling over cargo-xwin's underscored one, which it
            // wraps.
            .env(
                format!("CMAKE_TOOLCHAIN_FILE_{TARGET}"),
                workspace_root.join(CMAKE_TOOLCHAIN),
            );
        Ok(command)
    };
    let mut executables = cross_cargo(&["build"])?;
    executables.args(PACKAGES.into_iter().flat_map(|package| ["-p", package]));
    run(&mut executables, "cargo-xwin")?;
    // CEF's bootstrap executable loads the host as a DLL.
    let mut browser = cross_cargo(&["rustc"])?;
    browser.args(["-p", BROWSER_PACKAGE, "--lib", "--crate-type", "cdylib"]);
    run(&mut browser, "cargo-xwin")
}

fn cross_cflags() -> Result<String> {
    let home =
        std::env::var("HOME").context("the cross-build needs HOME for private-path remapping")?;
    Ok(format!("/clang:-ffile-prefix-map={home}=/usr/src/tufo"))
}

/// The folders the installer packs.
struct InstallerInputs<'a> {
    binaries: &'a Path,
    browser: &'a Path,
    licenses: &'a Path,
}

fn pack(
    workspace_root: &Path,
    inputs: &InstallerInputs<'_>,
    version: &Version,
    installer: &Path,
) -> Result<()> {
    let mut command = Command::new("makensis");
    command
        .args(["-V2", "-WX", "-INPUTCHARSET", "UTF8"])
        .arg(format!("-DVERSION={version}"))
        .arg(format!("-DVERSION_NUMERIC={}", numeric_version(version)?))
        .arg(format!("-DBINARIES_DIR={}", inputs.binaries.display()))
        .arg(format!("-DBROWSER_DIR={}", inputs.browser.display()))
        .arg(format!("-DLICENSES_DIR={}", inputs.licenses.display()))
        .arg(format!("-DOUTFILE={}", installer.display()))
        .arg(workspace_root.join(INSTALLER_SCRIPT));
    run(&mut command, "makensis")
}

/// Windows version resources hold four numbers of up to 16 bits, and no pre-release label.
fn numeric_version(version: &Version) -> Result<String> {
    let parts = [version.major, version.minor, version.patch];
    if parts.iter().any(|&part| part > u64::from(u16::MAX)) {
        bail!("Windows version resources can't hold version {version}");
    }
    Ok(format!("{}.{}.{}.0", parts[0], parts[1], parts[2]))
}

fn run(command: &mut Command, program: &str) -> Result<()> {
    let status = match command.status() {
        Ok(status) => status,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            bail!("{program} is missing; `just bootstrap` installs it")
        }
        Err(error) => return Err(error).with_context(|| format!("run {program}")),
    };
    ensure!(status.success(), "{program} failed: {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_version_drops_the_pre_release_label_and_rejects_oversized_parts() {
        assert_eq!(
            numeric_version(&Version::parse("1.2.3-beta.1").unwrap()).unwrap(),
            "1.2.3.0"
        );
        assert!(numeric_version(&Version::parse("1.70000.0").unwrap()).is_err());
    }
}
