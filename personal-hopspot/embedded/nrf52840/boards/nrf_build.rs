use std::env;
use std::fs;
use std::path::PathBuf;

/// Stage the board's memory layout into `OUT_DIR` as the `memory.x` the linker script includes.
/// The layout file is named for what determines it, the SoftDevice build in the board's
/// bootloader, so boards on the same SoftDevice share one file.
pub fn link_memory_layout(layout: &str) {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let memory = manifest.join(layout);
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::copy(&memory, out.join("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed={}", memory.display());
}
