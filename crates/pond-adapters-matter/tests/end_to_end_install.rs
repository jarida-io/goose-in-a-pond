//! The real first-run path: install the controller from the shipped sources and
//! talk to it over the protocol. No mock anywhere.
//!
//! `#[ignore]` because it runs `npm ci` against the network and takes tens of
//! seconds — CI's coverage is the mock suite, which needs neither. Run it by
//! hand when the controller, the install, or the protocol changes:
//!
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
        // IP only. BLE is a radio on the machine running the test, and on macOS
        // the OS kills a process that touches it without an Info.plist entry —
        // which `node` run from a test harness has not got.
        false,
    )
    .await
    .expect("a fresh Pond must be able to install and start its own controller");
    assert!(
        child.is_some(),
        "GIAP started it, so it must hold the handle"
    );

    // Idempotent: a second call recognises the running controller as one of
    // ours — by its greeting, not merely by something answering the port — and
    // reuses it.
    let reused = ensure_matter_server(
        &data_dir,
        port,
        Duration::from_secs(10),
        &MatterNotifier::disabled(),
        &url,
        // IP only. BLE is a radio on the machine running the test, and on macOS
        // the OS kills a process that touches it without an Info.plist entry —
        // which `node` run from a test harness has not got.
        false,
    )
    .await
    .unwrap();
    assert!(
        reused.is_none(),
        "a live controller must be reused, not replaced"
    );

    // And it speaks the protocol.
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

/// Asking for BLE must never cost the controller.
///
/// On macOS the OS SIGKILLs a process that touches CoreBluetooth without an
/// `NSBluetoothAlwaysUsageDescription` in its bundle's Info.plist — and the
/// controller is a bare `node`, which has no bundle. The kill cannot be caught
/// in-process, so `ble.ts`'s try/catch does not help: the process is gone.
/// Without the fallback the supervisor would respawn it and the OS would kill it
/// again, forever, and Matter would be unusable BECAUSE a transport was switched
/// on.
///
/// This test asserts the property, not the platform: wherever the request cannot
/// be honoured — no `@stoprocent/noble`, no `cap_net_raw` on Linux, no bundle on
/// macOS — `ensure_matter_server` must still hand back a controller that speaks
/// the protocol, with `ble: false` in its greeting. Where BLE DOES work it is
/// honoured on the first attempt and the greeting says `true`; both are a pass,
/// because both are the controller telling the truth about what it can reach.
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

    // Whatever it says, it must be what it actually loaded — the fallback drops
    // the transport, so a controller reporting `true` here has really got it.
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
