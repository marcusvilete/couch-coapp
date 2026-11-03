# evdev 0.13.2, patched for Couch Co-App

This is evdev 0.13.2 as published on crates.io (https://github.com/emberian/evdev, Apache-2.0 OR MIT; its licence texts are beside this file), with one change, wired in by `[patch.crates-io]` in the workspace's `Cargo.toml`, marked "Couch Co-App" in the code:

- `sys::ui_set_phys` (`src/sys.rs`), behind `VirtualDeviceBuilder::with_phys`: the ioctl's number now carries a pointer's size, as the kernel's `UI_SET_PHYS` is `_IOW('U', 108, char*)`. evdev 0.13.2 declared it with `ioctl_write_ptr!(…, libc::c_char)`, a size of 1, which the kernel refuses with `EINVAL`, so `with_phys` always failed.

**Why.** A Linux host's virtual controllers (M16c, `couch_platform_linux::pads`) name themselves by their `phys`, `couch-coapp/<pid>/pad<N>`, so a stream window on the same PC skips exactly those (found 2026-11-03, building M16c-1).

Everything else is unchanged. To update: copy the new release over this folder (a release with the fix needs no patch: then remove this folder and the `[patch.crates-io]` line).
