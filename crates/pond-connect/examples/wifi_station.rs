//! Drives this host's Wi-Fi station exactly as the setup channel does, so the adapter, the polkit
//! grant and the error codes can be checked on a real device before a phone exists to drive them.
//!
//! ```text
//! cargo run -p pond-connect --example wifi_station -- capabilities | status | scan
//! read -rs PSK; printf '%s' "$PSK" | cargo run -p pond-connect --example wifi_station -- join <ssid> [--hidden]
//! ```
//!
//! `join` reads the passphrase from stdin (empty for an open network) and never prints it.

use std::io::Read;

use pond_core::network::domain::wifi::{rank_networks, WifiPassphrase};
use zeroize::Zeroize;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let station = pond_connect::wifi::platform();
    match args.first().map(String::as_str) {
        Some("capabilities") => {
            let capabilities = station.capabilities().await;
            println!("scan: {:?}", capabilities.scan);
            println!("join: {:?}", capabilities.join);
        }
        Some("status") => println!("{}", json(&station.status().await)),
        Some("scan") => match station.scan().await {
            Ok(networks) => println!("{}", json(&rank_networks(networks))),
            Err(error) => return refused(error.code()),
        },
        Some("join") => {
            let Some(ssid) = args.get(1) else {
                return usage();
            };
            let hidden = args.iter().any(|arg| arg == "--hidden");
            let mut text = String::new();
            if std::io::stdin().read_to_string(&mut text).is_err() {
                text.zeroize();
                return usage();
            }
            // Trimmed in place: a trimmed copy would leave the original unwiped.
            while text.ends_with(['\r', '\n']) {
                text.pop();
            }
            let passphrase = if text.is_empty() {
                None
            } else {
                match WifiPassphrase::new(text) {
                    Ok(passphrase) => Some(passphrase),
                    Err(error) => {
                        eprintln!("{error}");
                        return refused("invalid_request");
                    }
                }
            };
            match station.join(ssid, passphrase.as_ref(), hidden).await {
                Ok(joined) => println!("joined {} with address {}", joined.ssid, joined.ipv4),
                Err(error) => return refused(error.code()),
            }
        }
        _ => return usage(),
    }
    std::process::ExitCode::SUCCESS
}

fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|error| error.to_string())
}

/// The code the phone would read, on stdout, so a script can compare it.
fn refused(code: &str) -> std::process::ExitCode {
    println!("refused: {code}");
    std::process::ExitCode::FAILURE
}

fn usage() -> std::process::ExitCode {
    eprintln!(
        "usage: wifi_station capabilities | status | scan | join <ssid> [--hidden] < passphrase"
    );
    std::process::ExitCode::from(2)
}
