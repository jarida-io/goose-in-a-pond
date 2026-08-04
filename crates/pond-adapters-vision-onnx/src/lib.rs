//! ONNX vision classifier (#130 follow-up) — upgrades the vision pipeline's
//! plain `"motion"` events into `"person"` / `"pet"` / `"package"` using a
//! small local detector. All inference is on-device.
//!
//! ```text
//! motion frame ─► letterbox 416² BGR ─► YOLOX-Nano (ort, load-dynamic)
//!                                          │ [1, 3549, 85]
//!                         decode + NMS ◄───┘
//!                              │ COCO classes → person / pet / package
//!                              ▼
//!                     Vec<Detection> → pipeline picks best ≥ confidence floor
//! ```
//!
//! Model: **YOLOX-Nano** (Apache-2.0, ~0.9M params). Drop `yolox_nano.onnx`
//! into `<data_dir>/models/vision/` and set `vision_classifier_model` — the
//! adapter loads it at startup and degrades to unlabelled motion when absent.
//! NanoDet-Plus (Apache-2.0) is the planned alternative; it needs its own
//! decoder, tracked as a follow-up. Ultralytics YOLO models are deliberately
//! avoided: AGPL-3.0 is incompatible with GIAP's Apache-2.0 license.
//!
//! Runtime: `ort` with `load-dynamic`, exactly like `pond-adapters-face-onnx`
//! — the ONNX Runtime is `dlopen`ed at startup (`ORT_DYLIB_PATH`), so the
//! runtime can be swapped without recompiling.
//!
//! **Swapping in a CUDA/TensorRT build does not make this adapter use the
//! GPU.** An earlier version of this note implied it would. `ort` uses an
//! execution provider only when one is explicitly registered on the
//! `SessionBuilder`, and this crate registers none — the GPU runtime loads and
//! then runs on the CPU, with no error to notice. See
//! `pond-adapters-face-onnx` for the same caveat and what changing it costs.

mod classifier;
mod decode;
mod labels;
mod preprocess;

pub use classifier::OnnxVisionClassifier;
