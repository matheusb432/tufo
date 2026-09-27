# Toasty SQLite driver for Tufo

This is `toasty-driver-sqlite` 0.10.0 from crates.io. Tufo patches its
SQLite value codec to bind UUIDs as 16-byte BLOBs and decode those BLOBs
back into UUIDs. Toasty's SQLite schema generator already declares UUID
columns as BLOB, but the published driver omits these two codec cases.

The workspace patches this crate until an upstream release supports UUID BLOBs.
The upstream source and documentation are at
[github.com/tokio-rs/toasty](https://github.com/tokio-rs/toasty).
