# softbuffer

This is `softbuffer` 0.4.8 from crates.io, with its Wayland backend patched to
share buffers as `Argb8888` rather than `Xrgb8888`, so the alpha that Slint's
software renderer writes reaches the compositor and the frameless window's
rounded corners show the desktop behind them. X11 already keeps alpha on a
32-bit visual.

Its Win32 backend reports the previous frame as reusable only while the window
still shows all of it: the last present covered the window's whole client area,
the client area kept its size, and Windows hasn't invalidated any of it. Slint
makes its windows transparent, so pixels that Windows drops when a window grows,
or that it never received while off-screen, would otherwise show the desktop
until something redraws them. Every other backend is unchanged.

The workspace patches this crate until an upstream release lets a surface keep
its alpha and stops trusting a Win32 window's stale contents. The upstream
source and documentation are at [github.com/rust-windowing/softbuffer](https://github.com/rust-windowing/softbuffer).

The upstream release is from commit `d871852faa1137d4615b99b9ad31c3ba80f345b5`.
Its [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE) licenses are retained.
This local copy is not published.
