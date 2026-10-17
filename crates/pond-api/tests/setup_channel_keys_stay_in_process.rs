//! The setup channel's keys come only from the handshake's channel methods, and those exist only
//! for an in-process transport. No route may call them, or a remote caller could hold the keys.

use std::path::Path;

fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push((
                path.display().to_string(),
                std::fs::read_to_string(&path).unwrap(),
            ));
        }
    }
}

#[test]
fn no_route_reaches_the_channel_handshake() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() > 10, "the scan found pond-api's sources");
    for (path, text) in files {
        for method in ["init_channel_pake", "verify_channel_pake"] {
            assert!(!text.contains(method), "{path} calls {method}");
        }
    }
}
