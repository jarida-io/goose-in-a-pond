//! Labels motion as person/pet/package with YOLOX-Nano from `<data_dir>/models/vision/`, via
//! `ort` `load-dynamic`. Not Ultralytics YOLO: AGPL-3.0 is incompatible with Apache-2.0.

mod classifier;
mod decode;
mod labels;
mod preprocess;

pub use classifier::OnnxVisionClassifier;
