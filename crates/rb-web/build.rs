//! Points `RB_WEB_DIST` at `web/dist`, or at an empty directory when the UI hasn't been built, so
//! the crate compiles without bun (the server then answers 503 "web UI not built").

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dist = manifest.join("../../web/dist");
    // A directory: Cargo rescans it recursively, so rebuilding the UI re-embeds it.
    println!("cargo:rerun-if-changed={}", dist.display());
    let dir = if dist.join("index.html").is_file() {
        dist.canonicalize().expect("canonical web/dist")
    } else {
        let empty = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("empty-dist");
        std::fs::create_dir_all(&empty).expect("create empty dist");
        empty
    };
    println!("cargo:rustc-env=RB_WEB_DIST={}", dir.display());
}
