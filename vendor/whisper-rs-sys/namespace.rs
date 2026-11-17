//! Static-link isolation for the pinned Whisper copy of GGML, on Linux and macOS.
use std::{collections::BTreeSet, fs, path::Path};

fn owned(name: &str) -> bool {
    name == "ggml"
        || name == "sum_rows_f32_cuda"
        // An Objective-C class is a global name too, and llama.cpp's Metal backend has its own.
        || name == "GGMLMetalClass"
        || [
            "ggml_",
            "gguf_",
            "quantize_",
            "dequantize_",
            "block_",
            "kvalues_",
            "iq2xs_",
            "iq3xs_",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}
fn collect(path: &Path, names: &mut BTreeSet<String>) -> std::io::Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            collect(&entry?.path(), names)?;
        }
    } else if matches!(
        path.extension().and_then(|x| x.to_str()),
        Some("h" | "hpp" | "c" | "cpp" | "cu" | "cuh" | "inc" | "m")
    ) {
        let source = fs::read_to_string(path)?;
        for token in source.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if owned(token) {
                names.insert(token.to_owned());
            }
        }
    }
    Ok(())
}
/// Compile every C, C++, Objective-C and CUDA use with the same private names. Rust sees the
/// original types and API names; bindgen emits explicit names for the FFI links.
pub fn header(source: &Path, destination: &Path) -> std::io::Result<()> {
    let mut names = BTreeSet::new();
    collect(source, &mut names)?;
    let mut output =
        String::from("/* Private Whisper GGML namespace. Generated from pinned sources. */\n");
    for name in names {
        output.push_str(&format!("#define {name} pond_whisper_{name}\n"));
    }
    // Every native file includes this header, so rewriting it unchanged would recompile all
    // of them whenever the build script reruns.
    if fs::read_to_string(destination).ok().as_deref() == Some(output.as_str()) {
        return Ok(());
    }
    fs::write(destination, output)
}
/// Bindgen writes an overridden link name verbatim, so it carries the platform's C symbol
/// prefix itself: `_` on Apple targets, where C's `f` is the symbol `_f`, and none on Linux.
#[derive(Debug)]
pub struct Bindings {
    pub symbol_prefix: &'static str,
}
impl bindgen::callbacks::ParseCallbacks for Bindings {
    fn generated_link_name_override(
        &self,
        item: bindgen::callbacks::ItemInfo<'_>,
    ) -> Option<String> {
        owned(item.name).then(|| format!("{}pond_whisper_{}", self.symbol_prefix, item.name))
    }
}
