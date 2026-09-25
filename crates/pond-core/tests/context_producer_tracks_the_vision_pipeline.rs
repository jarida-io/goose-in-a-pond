//! Ties `UNCLASSIFIED_CAMERA_EVENT_TYPES` to the fallback label `pond-adapters-vision` emits.
//! pond-core can't depend on it, and a rename would make every frame a prompt-injected row.

use std::path::{Path, PathBuf};

use pond_core::context::producer::UNCLASSIFIED_CAMERA_EVENT_TYPES;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/pond-core.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("pond-core must live two directories below the workspace root")
        .to_path_buf()
}

fn pipeline_source() -> String {
    let path = workspace_root().join("crates/pond-adapters-vision/src/pipeline.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}. If the vision pipeline moved, this tripwire is watching a file \
             that no longer exists and would never fire.",
            path.display()
        )
    })
}

/// Vacuity control: an emptied or reshaped file would let the assertions below match nothing.
#[test]
fn the_tripwire_is_reading_the_vision_pipeline() {
    let body = pipeline_source();
    assert!(
        body.contains("BusEvent::Camera("),
        "the file this test reads no longer publishes camera events to the bus, so it is not the \
         producer whose labels this rule is calibrated against"
    );
    assert!(
        body.contains("CameraEvent {"),
        "the file this test reads no longer constructs a CameraEvent"
    );
    assert!(
        body.contains("classifier"),
        "the file this test reads no longer has a classifier, so the fallback label it is being \
         checked for may no longer exist"
    );
}

/// A build without `vision-onnx` emits this fallback for every event, every 10 s.
#[test]
fn the_pipelines_unclassified_label_is_one_the_producer_refuses() {
    let body = pipeline_source();

    // Collect every `("…".to_string(), …)` literal, not one known spelling, so a rename fails.
    let mut fallbacks: Vec<String> = Vec::new();
    for fragment in body.split("(\"").skip(1) {
        let Some((literal, rest)) = fragment.split_once('"') else {
            continue;
        };
        if rest.starts_with(".to_string(),") {
            fallbacks.push(literal.to_string());
        }
    }

    assert!(
        !fallbacks.is_empty(),
        "no `(\"label\".to_string(), ...)` fallback was found in the vision pipeline. Either the \
         fallback is now written differently -- in which case this tripwire cannot see it and is \
         watching nothing -- or the pipeline no longer has one."
    );

    for label in &fallbacks {
        assert!(
            UNCLASSIFIED_CAMERA_EVENT_TYPES.contains(&label.as_str()),
            "pond-adapters-vision emits `{label}` when nothing classified the frame, and the \
             on-pond producer does NOT refuse it. On a build without the vision-onnx classifier \
             that is every camera event, so every camera source would ingest roughly 8640 rows a \
             day saying only that the pixels changed -- and each of them is read back into a \
             model's context window. Add `{label}` to UNCLASSIFIED_CAMERA_EVENT_TYPES in \
             crates/pond-core/src/context/producer.rs, or say in that constant's docs why this \
             one names a household fact. Labels found: {fallbacks:?}"
        );
    }
}
