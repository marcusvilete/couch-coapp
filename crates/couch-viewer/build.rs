//! sdl3-sys links `SDL3` without a search path when it isn't building SDL from
//! source, so point the linker at the prebuilt SDL3 in third_party/.

fn main() {
    println!("cargo::rerun-if-env-changed=COUCH_SDL3_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let Ok(dir) = std::env::var("COUCH_SDL3_DIR") else {
        println!("cargo::warning=COUCH_SDL3_DIR is not set; run `cargo xtask fetch-deps`");
        return;
    };
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x64",
        Ok("x86") => "x86",
        Ok("aarch64") => "arm64",
        Ok(other) => panic!("unsupported target architecture {other}"),
        Err(_) => panic!("CARGO_CFG_TARGET_ARCH not set"),
    };
    println!("cargo::rustc-link-search=native={dir}/lib/{arch}");
}
