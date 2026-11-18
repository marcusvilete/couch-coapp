# nv-codec-headers: NVENC's API header

`nvEncodeAPI.h` from [FFmpeg/nv-codec-headers](https://github.com/FFmpeg/nv-codec-headers), branch `sdk/13.0`, commit `ced4f8eba3ba5dd431932cba17928f0dffdaeb2b`: the commit FFmpeg 8.1's pinned build (BtbN/FFmpeg-Builds, `scripts.d/50-ffnvcodec.sh`) compiles against, so the app's own NVENC encoder (`couch-codec-nvenc`, M12d) asks for the same API version (13.0) and needs no newer driver than FFmpeg's.

- SHA-256 of `nvEncodeAPI.h`: `4fe4094541ef0f8a13249d97a8692dc5f835a6e9dd42eeadb3e2f7321d54dc7e` (checked by `cargo xtask gen-nvenc-bindings` and by xtask's tests).
- Licence: MIT, NVIDIA's notice at the top of the file; carried in the third-party notices.
- Unmodified. The bindings in `crates/couch-codec-nvenc/bindings/` are generated from it by `cargo xtask gen-nvenc-bindings` (needs LLVM, as `gen-ffmpeg-bindings` does).
