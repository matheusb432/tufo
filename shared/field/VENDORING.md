# Vendoring provenance

- Upstream: <https://github.com/Carnagion/field>
- Crates.io release: `field` 0.1.0
- Registry archive SHA-256:
  `ccccd906aacf6f92d665a5a3c4757d02781a8aedc8db733a1b370a838f477f15`
- Retrieved: 2026-08-30

## Local changes

- Upgraded the crate from Rust 2021 to Rust 2024.
- Inherited the source workspace's Rust version and lint policy.
- Narrowed the macro implementation to avoid a broad lint suppression.
- Added external compile-pass, compile-fail, and runtime contract tests.
- Replaced the installation-focused README with workspace usage guidance.
