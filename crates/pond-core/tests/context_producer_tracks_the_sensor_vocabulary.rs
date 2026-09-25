//! Ties the producer's `DISCRETE_SENSOR_TYPES` allow-list to the Matter bridge's vocabulary.
//! An unlisted signal is silently dropped. A tripwire only: client-sent types are unbounded.

use std::path::{Path, PathBuf};

use pond_core::context::producer::DISCRETE_SENSOR_TYPES;

/// Matter signals that are measurements and must stay dropped (named, never inferred).
const KNOWN_CONTINUOUS: &[&str] = &[
    "temperature",
    "humidity",
    // Environmental clusters: a continuously moving `MeasuredValue`, i.e. a sampled series.
    "illuminance",
    "pressure",
    "flow",
    // `air_quality` is a 0-6 grade that moves with the concentrations, so sampled too.
    "air_quality",
    "carbon_monoxide",
    "carbon_dioxide",
    "nitrogen_dioxide",
    "ozone",
    "formaldehyde",
    "pm1",
    "pm2_5",
    "pm10",
    "radon",
    "total_volatile_organic_compounds",
    // Remaining filter life (%), falling continuously; the change indication is the kept event.
    "hepa_filter_condition",
    "carbon_filter_condition",
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/pond-core.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("pond-core must live two directories below the workspace root")
        .to_path_buf()
}

fn matter_sensor_source() -> String {
    let path = workspace_root().join("matter-server/src/mapping/sensors.ts");
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}. If the Matter sensor map moved, this tripwire is watching a \
             file that no longer exists and would never fire.",
            path.display()
        )
    })
}

/// Every `sensorType: "…"` literal in the map, by shape, so new names are found too.
fn matter_sensor_types(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for fragment in body.split("sensorType: \"").skip(1) {
        if let Some((literal, _)) = fragment.split_once('"') {
            if !out.iter().any(|s| s == literal) {
                out.push(literal.to_string());
            }
        }
    }
    out
}

/// Vacuity control: if the map's shape changes, the test below would pass by matching nothing.
#[test]
fn the_tripwire_is_reading_the_matter_cluster_map() {
    let body = matter_sensor_source();
    assert!(
        body.contains("Reading"),
        "the file this test reads no longer produces sensor readings, so it is not the bridge \
         whose vocabulary this rule is calibrated against"
    );

    let found = matter_sensor_types(&body);
    assert!(
        found.len() >= 4,
        "the cluster map yielded only {found:?}. Either the mapping is now written differently — \
         in which case this tripwire cannot see it and is watching nothing — or the bridge lost \
         most of its clusters."
    );
    // Needs both kinds, or the disposition test below only proves half its claim.
    assert!(
        found.iter().any(|s| s == "contact"),
        "no discrete signal found in {found:?}"
    );
    assert!(
        found.iter().any(|s| s == "temperature"),
        "no continuous signal found in {found:?}"
    );
}

#[test]
fn every_matter_signal_is_either_kept_or_named_as_a_measurement() {
    let found = matter_sensor_types(&matter_sensor_source());

    let undecided: Vec<&String> = found
        .iter()
        .filter(|s| {
            !DISCRETE_SENSOR_TYPES.contains(&s.as_str()) && !KNOWN_CONTINUOUS.contains(&s.as_str())
        })
        .collect();

    assert!(
        undecided.is_empty(),
        "the Matter bridge now emits {undecided:?}, and the on-pond producer has never heard of \
         it. `DISCRETE_SENSOR_TYPES` is an allow-list, so those readings are silently discarded — \
         the household's new device becomes the one thing the assistant never mentions, with \
         nothing anywhere reporting why. Decide which it is: add it to DISCRETE_SENSOR_TYPES in \
         crates/pond-core/src/context/producer.rs if it is a transition, or to KNOWN_CONTINUOUS in \
         this file if it is a measurement the reading series already holds. Signals found: \
         {found:?}"
    );

    // The lists must stay disjoint, or the disposition above means nothing.
    for signal in KNOWN_CONTINUOUS {
        assert!(
            !DISCRETE_SENSOR_TYPES.contains(signal),
            "`{signal}` is named here as a measurement and is also in DISCRETE_SENSOR_TYPES"
        );
    }
}
