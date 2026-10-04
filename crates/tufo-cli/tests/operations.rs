//! Executable contracts with disposable profiles and audio rendered only into memory.

#[path = "operations/cancellation.rs"]
mod cancellation;
#[cfg(unix)]
#[path = "operations/canonical_import.rs"]
mod canonical_import;
#[path = "operations/concurrent.rs"]
mod concurrent;
#[path = "operations/credentials.rs"]
mod credentials;
#[path = "operations/fixtures.rs"]
mod fixtures;
#[path = "operations/kenku.rs"]
mod kenku;
#[path = "operations/library.rs"]
mod library;
#[path = "operations/parsing.rs"]
mod parsing;
#[path = "operations/playback.rs"]
mod playback;
#[path = "operations/pulled.rs"]
mod pulled;
#[path = "operations/settings.rs"]
mod settings;
#[path = "operations/streams.rs"]
mod streams;
#[path = "operations/support.rs"]
mod support;
#[cfg(unix)]
#[path = "operations/transport.rs"]
mod transport;
