//! The first-run path, `#[ignore]`d because `npm ci` hits the network; CI uses the mock suite.
//! ```text
//! cargo test -p pond-adapters-matter --test end_to_end_install -- --ignored --nocapture
//! ```

use std::time::Duration;

use pond_adapters_matter::{ensure_matter_server, MatterClient, MatterNotifier};

#[tokio::test]
#[ignore = "runs npm ci against the network"]
async fn a_fresh_pond_installs_a_controller_and_talks_to_it() {
    let data_dir = std::env::temp_dir().join(format!("giap-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::create_dir_all(&data_dir).unwrap();

    let port = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let p = l.local_addr().unwrap().port();
        drop(l);
        p
    };

    let url = format!("ws://127.0.0.1:{port}/giap");

    let child = ensure_matter_server(
        &data_dir,
        port,
        Duration::from_secs(240),
        &MatterNotifier::disabled(),
        &url,
        // IP only: macOS kills a `node` that touches BLE without an Info.plist entry.
        false,
    )
    .await
    .expect("a fresh Pond must be able to install and start its own controller");
    assert!(
        child.is_some(),
        "GIAP started it, so it must hold the handle"
    );

    // Idempotent: the second call recognises its controller by greeting and reuses it.
    let reused = ensure_matter_server(
        &data_dir,
        port,
        Duration::from_secs(10),
        &MatterNotifier::disabled(),
        &url,
        // IP only.
        false,
    )
    .await
    .unwrap();
    assert!(
        reused.is_none(),
        "a live controller must be reused, not replaced"
    );

    let (client, _events) = MatterClient::connect(&url)
        .await
        .expect("the greeting must be one this version accepts");

    client.send("ping", serde_json::json!({})).await.unwrap();

    let snapshot = client
        .send("subscribe", serde_json::json!({}))
        .await
        .unwrap();
    assert!(snapshot["devices"].is_array(), "got {snapshot}");
    assert!(snapshot["readings"].is_array(), "got {snapshot}");

    // The fabric landed where GIAP put it, not beside the process's cwd.
    assert!(
        data_dir.join("matter-server/storage-js").is_dir(),
        "the fabric must live under the data dir or it is lost on the next start"
    );
    assert!(data_dir.join("matter-server/app/.giap-install").is_file());
    // The install did not drag a node_modules across before npm ci ran.
    assert!(data_dir
        .join("matter-server/app/node_modules/@matter")
        .is_dir());

    drop(child);
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// Either greeting passes, if a controller comes up, speaks the protocol and reports `ble` truly.
#[tokio::test]
#[ignore = "runs npm ci against the network"]
async fn asking_for_ble_never_costs_the_controller() {
    let data_dir = std::env::temp_dir().join(format!("giap-e2e-ble-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::create_dir_all(&data_dir).unwrap();

    let port = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let p = l.local_addr().unwrap().port();
        drop(l);
        p
    };
    let url = format!("ws://127.0.0.1:{port}/giap");

    let child = ensure_matter_server(
        &data_dir,
        port,
        Duration::from_secs(240),
        &MatterNotifier::disabled(),
        &url,
        true,
    )
    .await
    .expect("a controller must come back whether or not BLE could be had");
    assert!(
        child.is_some(),
        "GIAP started it, so it must hold the handle"
    );

    let (client, _events) = MatterClient::connect(&url)
        .await
        .expect("the controller must speak the protocol either way");
    client.send("ping", serde_json::json!({})).await.unwrap();

    // The fallback drops BLE, so reporting `true` here means it really loaded.
    println!(
        "BLE was {} on this host",
        if client.has_ble() {
            "available and honoured"
        } else {
            "unavailable; the controller fell back to IP only"
        }
    );

    drop(child);
    let _ = std::fs::remove_dir_all(&data_dir);
}
