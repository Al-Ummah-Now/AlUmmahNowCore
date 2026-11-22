//! With `whisper-vulkan`, links our own stand-in for the Vulkan loader (`vulkan_loader.c`).
//!
//! ggml-vulkan calls Vulkan through a run-time dispatcher, but links three loader symbols
//! directly, and `whisper-rs-sys` links the loader library (`vulkan-1.dll`, `libvulkan.so.1`). A
//! computer without one (no GPU driver, a VM, a minimal Linux) could then not load `aun_core` at
//! all. The stand-in defines those symbols and opens the real loader at run time; without one,
//! ggml finds no Vulkan device and whisper runs on the CPU. Its object is linked whole, so the
//! symbols are defined before any library is searched, and an empty `vulkan`/`vulkan-1` library
//! in `OUT_DIR` answers `whisper-rs-sys`'s link line, so the real loader is never linked.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/transcribe/vulkan_loader.c");
    if std::env::var_os("CARGO_FEATURE_WHISPER_VULKAN").is_none() {
        return;
    }
    vulkan_loader();
}

#[cfg(feature = "whisper-vulkan")]
fn vulkan_loader() {
    use std::path::PathBuf;

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default());
    cc::Build::new()
        .file("src/transcribe/vulkan_loader.c")
        .warnings(true)
        .cargo_metadata(false)
        .compile("aun_vulkan_loader");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static:-bundle,+whole-archive=aun_vulkan_loader");

    // An empty library for whisper-rs-sys's `-lvulkan` / `vulkan-1.lib`, found before the system's
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let empty = out.join("aun_vulkan_empty");
    std::fs::create_dir_all(&empty).unwrap_or_else(|e| panic!("create {}: {e}", empty.display()));
    let stub = empty.join("empty.c");
    std::fs::write(
        &stub,
        "/* answers the link line's Vulkan loader library; see build.rs */\nint aun_vulkan_empty_library;\n",
    )
    .unwrap_or_else(|e| panic!("write {}: {e}", stub.display()));
    cc::Build::new()
        .file(&stub)
        .out_dir(&empty)
        .cargo_metadata(false)
        .compile(if windows { "vulkan-1" } else { "vulkan" });
    println!("cargo:rustc-link-search=native={}", empty.display());
}

#[cfg(not(feature = "whisper-vulkan"))]
fn vulkan_loader() {}
