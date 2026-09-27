<!-- Modified for Tufo in 2026: replaced registry installation guidance with workspace usage. -->

# field

Statically verified `struct` field names as strings.

Tufo vendors this dependency under `shared/field`. Workspace crates can use it
through the root dependency declaration:

```toml
[dependencies]
field.workspace = true
```

The crate has no runtime dependencies and remains `#![no_std]` compatible.

See [VENDORING.md](VENDORING.md) for the upstream source and local changes.

## License

Licensed under either the MIT License or Apache License 2.0, at your option.
