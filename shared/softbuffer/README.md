# softbuffer for Tufo

This is `softbuffer` 0.4.8 from crates.io. Tufo patches its Wayland backend to
share buffers as `Argb8888` rather than `Xrgb8888`, so the alpha that Slint's
software renderer writes reaches the compositor and the frameless window's
rounded corners show the desktop behind them. Every other backend is unchanged;
X11 already keeps alpha on a 32-bit visual.

The workspace patches this crate until an upstream release lets a surface keep
its alpha. The upstream source and documentation are at
[github.com/rust-windowing/softbuffer](https://github.com/rust-windowing/softbuffer).

The upstream release is from commit `d871852faa1137d4615b99b9ad31c3ba80f345b5`.
Its [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE) licenses are retained.
This local copy is not published.
