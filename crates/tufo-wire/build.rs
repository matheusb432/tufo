use std::{fs, path::PathBuf, process::Command};

use anyhow::{Context as _, bail};
use prost::Message as _;
use prost_types::FileDescriptorSet;

fn main() -> anyhow::Result<()> {
    let manifest_directory = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let workspace_root = manifest_directory
        .ancestors()
        .nth(2)
        .context("tufo-wire must be two directories below the workspace root")?;
    let descriptor_path = PathBuf::from(std::env::var("OUT_DIR")?).join("tufo_descriptor.bin");
    let output = Command::new("buf")
        .current_dir(workspace_root)
        .args([
            "build",
            "--as-file-descriptor-set",
            "--path",
            "crates/tufo-wire/proto/tufo/v1",
            "--output",
        ])
        .arg(&descriptor_path)
        .output()
        .context("run `buf build`; install the repository's pinned tools with `just bootstrap`")?;
    if !output.status.success() {
        bail!(
            "buf build failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let descriptor_bytes = fs::read(&descriptor_path)
        .with_context(|| format!("read {}", descriptor_path.display()))?;
    let descriptors = FileDescriptorSet::decode(descriptor_bytes.as_slice())
        .context("decode the Buf file descriptor set")?;
    // Clients connect through tufo-local-transport, so the wire crate stays transport-agnostic.
    tonic_prost_build::configure()
        .build_transport(false)
        .compile_fds(descriptors)?;

    for path in ["proto", "../../buf.yaml"] {
        println!("cargo:rerun-if-changed={path}");
    }
    Ok(())
}
