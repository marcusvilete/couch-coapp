# third_party

Native dependencies downloaded by `cargo xtask fetch-deps`, git-ignored; and one patched crate, `vigem-rust/`, committed.

| Folder | Contents | Pinned in |
|---|---|---|
| `ffmpeg/` | FFmpeg 8.1 win64 LGPL shared build from BtbN/FFmpeg-Builds | `xtask/src/deps.rs` |
| `sdl3/` | SDL3 3.4.16 Visual C++ development package | `xtask/src/deps.rs` |
| `vigem-rust/` | vigem-rust 0.2.0 (MIT OR Apache-2.0), **committed, not downloaded**: patched to plug pads from serial 1 and to keep a pad Windows is still setting up (`vigem-rust/PATCHED.md`) | `[patch.crates-io]` in `Cargo.toml` |

Each download is checked against a pinned SHA-256 before it is extracted.

The FFmpeg build is LGPL-3.0-or-later. It is linked dynamically and its DLLs ship unmodified, so they stay replaceable.
