//! Linux static-link isolation for the pinned Whisper copy of GGML.
use std::{collections::BTreeSet, fs, path::Path};

fn owned(name: &str) -> bool {
    name == "ggml"
        || name == "sum_rows_f32_cuda"
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
        Some("h" | "hpp" | "c" | "cpp" | "cu" | "cuh" | "inc")
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
/// Compile every C, C++ and CUDA use with the same private names. Rust sees the
/// original types and API names; bindgen emits explicit names for the FFI links.
pub fn header(source: &Path, destination: &Path) -> std::io::Result<()> {
    let mut names = BTreeSet::new();
    collect(source, &mut names)?;
    let mut output =
        String::from("/* Private Whisper GGML namespace. Generated from pinned sources. */\n");
    for name in names {
        output.push_str(&format!("#define {name} pond_whisper_{name}\n"));
    }
    fs::write(destination, output)
}
#[derive(Debug)]
pub struct Bindings;
impl bindgen::callbacks::ParseCallbacks for Bindings {
    fn generated_link_name_override(
        &self,
        item: bindgen::callbacks::ItemInfo<'_>,
    ) -> Option<String> {
        owned(item.name).then(|| format!("pond_whisper_{}", item.name))
    }
}
