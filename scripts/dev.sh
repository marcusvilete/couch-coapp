# Prepare a shell for development on Linux (M16, ADR 0011). Source it from the repo root:
#   . scripts/dev.sh
# Chooses Linux's FFmpeg bindings over .cargo/config.toml's Windows default (a variable
# set here wins over the file's [env]), and points bindgen at the system's libclang.

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
export FFMPEG_BINDING_PATH="$repo/crates/couch-codec-ffmpeg/bindings/ffmpeg-x86_64-unknown-linux-gnu.rs"
export FFMPEG_LIBS_DIR="$repo/third_party/ffmpeg/lib"
# Only `cargo xtask gen-ffmpeg-bindings` needs it.
for dir in $(ls -d /usr/lib/llvm-*/lib 2>/dev/null | sort -V); do
  if [ -e "$dir/libclang.so" ]; then export LIBCLANG_PATH="$dir"; fi
done
[ -n "${RUST_LOG:-}" ] || export RUST_LOG='couch=debug,iroh=info'
