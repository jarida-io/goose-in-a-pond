//! Voice Match's speaker model: WeSpeaker's ResNet34-LM, trained on VoxCeleb (CC BY 4.0), pinned
//! to one revision and checksum, fetched by default with its credit shown.

use super::curated::FilePin;

pub const REPO: &str = "Wespeaker/wespeaker-voxceleb-resnet34-LM";
pub const REVISION: &str = "f0c48c298fd835726c27956a5d617bad7115627e";
pub const FILE: &str = "voxceleb_resnet34_LM.onnx";
/// Where the pond keeps it, under the data directory (`pond_ear::locations::speaker`).
pub const LOCAL_PATH: &str = "models/voice-match/resnet34_LM.onnx";
pub const SIZE_BYTES: u64 = 26_530_309;
pub const SHA256: &str = "7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068";
/// What its prints are stored under.
pub const MODEL_NAME: &str = "wespeaker-resnet34-lm";

pub fn url() -> String {
    format!("https://huggingface.co/{REPO}/resolve/{REVISION}/{FILE}")
}

pub fn pinned(repo: &str, revision: &str, filename: &str) -> Option<FilePin> {
    (repo == REPO && revision == REVISION && filename == FILE).then_some(FilePin {
        size_bytes: SIZE_BYTES,
        sha256: SHA256,
    })
}
