//! Release packages in `dist/`, each with a `.sha256` file: the Ubuntu `.deb` and the macOS
//! `.pkg`. The Windows installer comes from `windows_installer`.

use std::{
    fmt::Write as _,
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail, ensure};
use cargo_metadata::{Metadata, MetadataCommand, semver::Version};
use clap::ValueEnum;
use sha2::{Digest as _, Sha256};

pub(crate) const OUTPUT_DIR: &str = "dist";
const APPLICATION_PACKAGES: [&str; 3] = ["tufo-cli", "tufo-server", "tufo-gui"];
/// The licenses and credits `THIRD-PARTY.md` links, which every package ships.
const NOTICE_PATHS: [&str; 16] = [
    "LICENSE",
    "THIRD-PARTY.md",
    "assets/backgrounds/CREDITS.md",
    "assets/backgrounds/SOURCES.md",
    "assets/effects/CREDITS.md",
    "crates/tufo-gui/fonts/OFL-AlegreyaSans.txt",
    "crates/tufo-gui/ui/icons/LICENSE",
    "crates/tufo-gui/icons/sounds/LICENSE",
    "shared/field/LICENSE-MIT",
    "shared/field/LICENSE-APACHE",
    "shared/field/VENDORING.md",
    "shared/error-meta/LICENSE",
    "shared/error-meta/README.md",
    "shared/softbuffer/LICENSE-MIT",
    "shared/softbuffer/LICENSE-APACHE",
    "shared/softbuffer/README.md",
];
const DEPENDENCY_LICENSES_FILE: &str = "DEPENDENCY-LICENSES.txt";
const DEPENDENCY_LICENSES_TEMPLATE: &str = "packaging/licenses.hbs";
const SLINT_LICENSE_ID: &str = "LicenseRef-Slint-Royalty-free-2.0";
const SLINT_LICENSE_FILE: &str = "SLINT-LICENSE.md";
#[cfg(target_os = "linux")]
const LINUX_PACKAGING_DIR: &str = "packaging/linux";
#[cfg(target_os = "linux")]
const APPARMOR_PROFILE: &str = "etc/apparmor.d/tufo-browser";
/// winit opens the X11, Wayland, and keyboard libraries at runtime, so linking doesn't name them.
#[cfg(target_os = "linux")]
const RUNTIME_OPENED_PACKAGES: [&str; 9] = [
    "libx11-6",
    "libx11-xcb1",
    "libxcb1",
    "libxcursor1",
    "libxi6",
    "libxrandr2",
    "libxkbcommon0",
    "libxkbcommon-x11-0",
    "libwayland-client0",
];
#[cfg(target_os = "macos")]
const MACOS_COMPONENTS_PLIST: &str = "packaging/macos/components.plist";
#[cfg(target_os = "macos")]
const MACOS_IDENTIFIER: &str = "dev.tufo.Tufo";

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Platform {
    Linux,
    Macos,
}

/// The version every application package shares, with three numeric components only.
pub(crate) fn version(metadata: &Metadata) -> Result<Version> {
    let mut selected: Option<&Version> = None;
    for name in APPLICATION_PACKAGES {
        let current = &metadata
            .packages
            .iter()
            .find(|package| package.name.as_str() == name)
            .with_context(|| format!("the workspace has no {name} package"))?
            .version;
        match selected {
            Some(expected) => ensure!(
                current == expected,
                "{name} must have the same version as tufo-cli ({expected})"
            ),
            None => selected = Some(current),
        }
    }
    let version = selected
        .context("the workspace has no application packages")?
        .clone();
    ensure!(
        version.pre.is_empty() && version.build.is_empty(),
        "release version {version} needs three numeric components only"
    );
    Ok(version)
}

pub(crate) fn package(
    workspace_root: &Path,
    metadata: &Metadata,
    platform: Platform,
) -> Result<()> {
    let version = version(metadata)?;
    let release_dir = metadata
        .target_directory
        .join("release")
        .into_std_path_buf();
    let output_dir = workspace_root.join(OUTPUT_DIR);
    fs::create_dir_all(&output_dir).with_context(|| format!("create {}", output_dir.display()))?;
    let installer = match platform {
        Platform::Linux => package_linux(workspace_root, &release_dir, &output_dir, &version)?,
        Platform::Macos => package_macos(workspace_root, &release_dir, &output_dir, &version)?,
    };
    write_checksum(&installer)?;
    println!("packaged {}", installer.display());
    Ok(())
}

/// Copies the bundled material's licenses and credits into `destination`, with the licenses of
/// every Rust dependency the release targets link.
pub(crate) fn stage_notices(workspace_root: &Path, destination: &Path) -> Result<()> {
    for relative in NOTICE_PATHS {
        let source = workspace_root.join(relative);
        let target = destination.join(relative);
        fs::create_dir_all(target.parent().context("a notice has no parent folder")?)?;
        fs::copy(&source, &target).with_context(|| format!("copy {}", source.display()))?;
    }
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args(["about", "generate", "--locked", "--fail", "--workspace"])
        .arg("--output-file")
        .arg(destination.join(DEPENDENCY_LICENSES_FILE))
        .arg(DEPENDENCY_LICENSES_TEMPLATE)
        .status()
        .context("run cargo-about; `just bootstrap` installs it")?;
    ensure!(status.success(), "cargo-about failed: {status}");
    fs::write(
        destination.join(SLINT_LICENSE_FILE),
        slint_license_text(workspace_root)?,
    )
    .context("write Slint's license")
}

/// cargo-about leaves out license texts that only a `LicenseRef` names, such as Slint's.
fn slint_license_text(workspace_root: &Path) -> Result<Vec<u8>> {
    let metadata = MetadataCommand::new()
        .manifest_path(workspace_root.join("Cargo.toml"))
        .other_options(vec!["--locked".to_owned()])
        .exec()
        .context("read the locked dependency graph")?;
    let package = metadata
        .packages
        .iter()
        .find(|package| {
            package
                .license
                .as_deref()
                .is_some_and(|license| license.contains(SLINT_LICENSE_ID))
        })
        .context("the dependency graph has no Slint package")?;
    let source = package
        .manifest_path
        .parent()
        .context("Slint's manifest has no parent folder")?
        .join(format!("LICENSES/{SLINT_LICENSE_ID}.md"));
    fs::read(&source).with_context(|| format!("read {source}"))
}

pub(crate) fn write_checksum(installer: &Path) -> Result<PathBuf> {
    let mut file =
        File::open(installer).with_context(|| format!("open {}", installer.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 16_384];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let file_name = installer
        .file_name()
        .context("the installer has no file name")?
        .to_string_lossy();
    let hex = hash
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    let checksum = installer.with_file_name(format!("{file_name}.sha256"));
    fs::write(&checksum, format!("{hex}  {file_name}\n"))
        .with_context(|| format!("write {}", checksum.display()))?;
    Ok(checksum)
}

#[cfg(target_os = "linux")]
fn package_linux(
    workspace_root: &Path,
    release_dir: &Path,
    output_dir: &Path,
    version: &Version,
) -> Result<PathBuf> {
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "the Linux package needs an x86-64 build"
    );
    let staging = tempfile::tempdir_in(output_dir).context("create the package staging")?;
    let root = staging.path();
    crate::install::linux::stage_package(workspace_root, release_dir, root)?;
    stage_notices(workspace_root, &root.join("usr/share/doc/tufo"))?;
    let packaging = workspace_root.join(LINUX_PACKAGING_DIR);
    let profile = root.join(APPARMOR_PROFILE);
    fs::create_dir_all(
        profile
            .parent()
            .context("the profile has no parent folder")?,
    )?;
    fs::copy(packaging.join("tufo-browser.apparmor"), &profile)
        .context("copy the browser's AppArmor profile")?;
    normalize_unix_modes(root)?;

    let control = root.join("DEBIAN");
    fs::create_dir_all(&control)?;
    for script in ["postinst", "prerm"] {
        fs::copy(packaging.join(script), control.join(script))
            .with_context(|| format!("copy the {script} script"))?;
    }
    fs::write(control.join("conffiles"), format!("/{APPARMOR_PROFILE}\n"))?;
    let depends = with_runtime_opened_packages(&shared_library_depends(root)?);
    fs::write(
        control.join("control"),
        debian_control(version, &depends, installed_size_kib(root)?),
    )?;
    normalize_unix_modes(&control)?;

    let installer = output_dir.join(format!("tufo-{version}-linux-x86_64.deb"));
    run(
        Command::new("dpkg-deb")
            .args(["--build", "--root-owner-group"])
            .arg(root)
            .arg(&installer),
        "dpkg-deb",
    )?;
    Ok(installer)
}

#[cfg(not(target_os = "linux"))]
fn package_linux(_: &Path, _: &Path, _: &Path, _: &Version) -> Result<PathBuf> {
    bail!("the Linux package must be built on Linux")
}

#[cfg(target_os = "linux")]
fn debian_control(version: &Version, depends: &str, installed_size_kib: u64) -> String {
    format!(
        "Package: tufo\n\
         Version: {version}\n\
         Architecture: amd64\n\
         Maintainer: Matheus Bastos <matheusb432@users.noreply.github.com>\n\
         Installed-Size: {installed_size_kib}\n\
         Depends: {depends}\n\
         Section: sound\n\
         Priority: optional\n\
         Homepage: {}\n\
         Description: Music and sound effects player for tabletop sessions\n \
         Plays playlists and soundboards on this computer or in one Discord voice channel.\n",
        env!("CARGO_PKG_REPOSITORY")
    )
}

/// The packages that provide the system libraries the package's executables and libraries link,
/// as `dpkg-shlibdeps` reads them from the build host.
#[cfg(target_os = "linux")]
fn shared_library_depends(root: &Path) -> Result<String> {
    let mut elf_files = Vec::new();
    collect_elf_files(root, &mut elf_files)?;
    // dpkg-shlibdeps reads the source package's name from debian/control.
    let source_package = tempfile::tempdir().context("create the dpkg-shlibdeps folder")?;
    let debian = source_package.path().join("debian");
    fs::create_dir_all(&debian)?;
    fs::write(
        debian.join("control"),
        "Source: tufo\n\nPackage: tufo\nArchitecture: amd64\n",
    )?;
    let output = Command::new("dpkg-shlibdeps")
        .current_dir(source_package.path())
        .args(["-O", "--ignore-missing-info"])
        // The browser's own libraries, such as libcef.so, ship in the package.
        .arg(format!(
            "-l{}",
            root.join(crate::install::linux::PACKAGE_BROWSER.trim_start_matches('/'))
                .display()
        ))
        .args(elf_files.iter().map(|file| {
            let mut argument = std::ffi::OsString::from("-e");
            argument.push(file);
            argument
        }))
        .output()
        .context("run dpkg-shlibdeps; install dpkg-dev")?;
    ensure!(
        output.status.success(),
        "dpkg-shlibdeps failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).context("dpkg-shlibdeps printed non-UTF-8")?;
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("shlibs:Depends="))
        .map(str::to_owned)
        .context("dpkg-shlibdeps printed no dependencies")
}

/// Appends the runtime-opened packages that linking didn't already name.
#[cfg(target_os = "linux")]
fn with_runtime_opened_packages(linked: &str) -> String {
    let mut depends: Vec<&str> = linked.split(", ").filter(|item| !item.is_empty()).collect();
    for package in RUNTIME_OPENED_PACKAGES {
        let named = depends
            .iter()
            .any(|item| item.split_whitespace().next() == Some(package));
        if !named {
            depends.push(package);
        }
    }
    depends.join(", ")
}

#[cfg(target_os = "linux")]
fn collect_elf_files(folder: &Path, elf_files: &mut Vec<PathBuf>) -> Result<()> {
    const ELF_MAGIC: [u8; 4] = *b"\x7fELF";
    for entry in fs::read_dir(folder).with_context(|| format!("read {}", folder.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_elf_files(&path, elf_files)?;
        } else if file_type.is_file() {
            let mut magic = [0_u8; 4];
            let is_elf = File::open(&path)
                .and_then(|mut file| file.read_exact(&mut magic))
                .is_ok()
                && magic == ELF_MAGIC;
            if is_elf {
                elf_files.push(path);
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn installed_size_kib(root: &Path) -> Result<u64> {
    fn bytes(path: &Path) -> Result<u64> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() {
            return Ok(metadata.len());
        }
        let mut total = 0;
        for entry in fs::read_dir(path)? {
            total += bytes(&entry?.path())?;
        }
        Ok(total)
    }
    Ok(bytes(root)?.div_ceil(1024))
}

#[cfg(target_os = "macos")]
fn package_macos(
    workspace_root: &Path,
    release_dir: &Path,
    output_dir: &Path,
    version: &Version,
) -> Result<PathBuf> {
    ensure!(
        std::env::consts::ARCH == "aarch64",
        "the macOS package needs an Apple silicon build"
    );
    let staging = tempfile::tempdir_in(output_dir).context("create the package staging")?;
    let root = staging.path();
    crate::install::macos::stage_bundle(
        workspace_root,
        release_dir,
        &root.join("Applications/Tufo.app"),
        &version.to_string(),
    )?;
    let link = root.join("usr/local/bin/tufo");
    fs::create_dir_all(
        link.parent()
            .context("the command link has no parent folder")?,
    )?;
    std::os::unix::fs::symlink("/Applications/Tufo.app/Contents/MacOS/tufo", &link)?;
    normalize_unix_modes(root)?;
    let installer = output_dir.join(format!("tufo-{version}-macos-arm64.pkg"));
    run(
        Command::new("pkgbuild")
            .arg("--root")
            .arg(root)
            .args(["--identifier", MACOS_IDENTIFIER, "--version"])
            .arg(version.to_string())
            .args(["--install-location", "/"])
            .arg("--component-plist")
            .arg(workspace_root.join(MACOS_COMPONENTS_PLIST))
            .arg(&installer),
        "pkgbuild",
    )?;
    Ok(installer)
}

#[cfg(not(target_os = "macos"))]
fn package_macos(_: &Path, _: &Path, _: &Path, _: &Version) -> Result<PathBuf> {
    bail!("the macOS package must be built on macOS")
}

/// Executables and folders become 0755 and other files 0644, whatever the build's umask left.
#[cfg(unix)]
fn normalize_unix_modes(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    let mode = if metadata.is_dir() || metadata.permissions().mode() & 0o111 != 0 {
        0o755
    } else {
        0o644
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            normalize_unix_modes(&entry?.path())?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn run(command: &mut Command, program: &str) -> Result<()> {
    let status = command.status().with_context(|| format!("run {program}"))?;
    ensure!(status.success(), "{program} failed: {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_names_the_installer_and_matches_its_bytes() {
        let root = tempfile::tempdir().unwrap();
        let installer = root.path().join("tufo-1.2.3-linux-x86_64.deb");
        fs::write(&installer, b"package bytes").unwrap();

        let checksum = write_checksum(&installer).unwrap();

        assert_eq!(
            fs::read_to_string(checksum).unwrap(),
            "2e547448dcd0f2fcd9dbc386d33f1553369883451898177559bcf3e3b1083d16  tufo-1.2.3-linux-x86_64.deb\n"
        );
    }

    #[test]
    fn every_notice_exists_in_the_source_tree() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for relative in NOTICE_PATHS {
            assert!(
                workspace_root.join(relative).is_file(),
                "{relative} is missing"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_opened_packages_join_the_linked_ones_once() {
        let depends = with_runtime_opened_packages("libc6 (>= 2.39), libxcb1 (>= 1.1)");

        assert!(depends.starts_with("libc6 (>= 2.39), libxcb1 (>= 1.1), libx11-6, libx11-xcb1, "));
        assert_eq!(depends.matches("libxcb1").count(), 1);
        assert!(depends.ends_with("libwayland-client0"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_control_file_carries_the_version_size_and_dependencies() {
        let control = debian_control(&Version::new(1, 2, 3), "libc6 (>= 2.39)", 42);

        assert!(control.starts_with("Package: tufo\nVersion: 1.2.3\nArchitecture: amd64\n"));
        assert!(control.contains("\nInstalled-Size: 42\nDepends: libc6 (>= 2.39)\n"));
        assert!(control.ends_with(
            " Plays playlists and soundboards on this computer or in one Discord voice channel.\n"
        ));
    }
}
