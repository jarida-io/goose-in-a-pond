//! Stubs `pond-desktop/dist` so `include_dir!` builds on a fresh checkout.
//! The stub is marked `data-giap-placeholder`; build the real UI before a release.

use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pond-desktop/dist");
    if let Err(e) = std::fs::create_dir_all(&dist) {
        println!("cargo:warning=could not create {}: {e}", dist.display());
        return;
    }
    let index = dist.join("index.html");
    if !index.exists() {
        let _ = std::fs::write(
            &index,
            "<!doctype html><html data-giap-placeholder><head><meta charset=\"utf-8\">\
             <title>Goose In A Pond</title></head><body>\
             <p>Web UI not built into this binary. Build it with \
             <code>cd pond-desktop &amp;&amp; npm run build</code> then rebuild, \
             or run the server with <code>--static-dir pond-desktop/dist</code>.</p>\
             </body></html>",
        );
    }
    // Re-embed when the built assets change.
    println!("cargo:rerun-if-changed=../../pond-desktop/dist");
    println!("cargo:rerun-if-changed=build.rs");
}
