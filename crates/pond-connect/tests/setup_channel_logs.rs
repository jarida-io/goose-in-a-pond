//! No secret of a setup channel session reaches a log line or the audit trail. Its own binary,
//! because the capturing subscriber must be the process's only one.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::*;
use pond_infra::pairing_cpace::device_password;
use serde_json::json;

/// Formats every event at every level into one buffer, fields included.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Capture {
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// The lines written since `from` at `levels`, without their timestamps.
    fn lines_since(&self, from: usize, levels: &[&str]) -> Vec<String> {
        let text = String::from_utf8(self.0.lock().unwrap()[from..].to_vec()).unwrap();
        text.lines()
            .filter_map(|line| line.split_once(' ').map(|(_, rest)| rest.trim_start()))
            .filter(|line| levels.iter().any(|level| line.starts_with(level)))
            .map(str::to_string)
            .collect()
    }
}

/// One failed device run from a fresh connection, as the lines it logged.
async fn failed_resume(
    pond: &Pond,
    capture: &Capture,
    client_id: &str,
) -> (Vec<String>, Vec<String>) {
    let from = capture.len();
    let mut phone = Phone::connect(pond, 23, client_id).await;
    let refusal = phone
        .pake("device", &device_password("not-the-token"))
        .await
        .err()
        .unwrap();
    assert_eq!(refusal["error"], "invalid_confirmation");
    phone.leave(pond).await;
    (
        capture.lines_since(from, &["INFO", "WARN", "ERROR"]),
        capture.lines_since(from, &["DEBUG"]),
    )
}

#[tokio::test]
async fn no_secret_reaches_a_log_line() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let pond = pond().await;
    let (mut phone, paired) = claim_and_join(&pond, 23).await;
    let wrong = phone
        .ask(
            "wifi_join",
            json!({"ssid": "Home", "passphrase": "not-the-passphrase-77"}),
        )
        .await;
    assert_eq!(wrong["error"], "wrong_password");
    phone.handle.writer.phone_disconnected().await;
    phone.ended().await;
    wait_until_free(&pond.channel).await;

    let mut second = Phone::connect(&pond, 185, "install-heard").await;
    second.ask("spoken_code", json!({})).await;
    let spoken = wait_for_announcement(&pond.announcer).await;
    let second_paired = second.pake("pairing", &spoken).await.unwrap();
    second.leave(&pond).await;

    let mut third = Phone::connect(&pond, 23, "install-e2e").await;
    let old_hash = device_password(&paired.refresh_token);
    let resumed = third.pake("device", &old_hash).await.unwrap();
    third.leave(&pond).await;

    let (wrong_token, wrong_token_debug) = failed_resume(&pond, &capture, "install-e2e").await;
    let (unknown_id, unknown_id_debug) = failed_resume(&pond, &capture, "install-stranger").await;
    assert!(
        wrong_token
            .iter()
            .any(|line| line.contains("device run CPace attempt failed")),
        "{wrong_token:#?}"
    );
    assert_eq!(
        wrong_token, unknown_id,
        "a wrong token and an unknown id log the same lines"
    );
    let unpaired = |lines: &[String]| {
        lines
            .iter()
            .filter(|line| line.contains("device_run_unpaired"))
            .count()
    };
    assert_eq!(unpaired(&wrong_token_debug), 0);
    assert_eq!(
        unpaired(&unknown_id_debug),
        1,
        "only a debug line says which case it was"
    );

    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("setup_channel_request") && log.contains("wifi_join"),
        "the capture saw the session's lines: {}",
        log.len()
    );
    let events = format!("{:?}", pond.events.all());
    for (name, secret) in [
        ("the passphrase", HOME_PASSPHRASE.to_string()),
        ("a wrong passphrase", "not-the-passphrase-77".to_string()),
        ("the setup code", pond.code.clone()),
        ("the spoken code", spoken.clone()),
        ("a session token", paired.session_token.clone()),
        ("a refresh token", paired.refresh_token.clone()),
        (
            "a second session token",
            second_paired.session_token.clone(),
        ),
        ("k_p2c", paired.keys_hex[0].clone()),
        ("k_c2p", paired.keys_hex[1].clone()),
        ("a second k_p2c", second_paired.keys_hex[0].clone()),
        ("a resumed refresh hash", old_hash.to_string()),
        ("a resumed session token", resumed.session_token.clone()),
        ("a rotated refresh token", resumed.refresh_token.clone()),
        (
            "a rotated refresh hash",
            device_password(&resumed.refresh_token).to_string(),
        ),
        ("a resumed k_p2c", resumed.keys_hex[0].clone()),
        ("a resumed k_c2p", resumed.keys_hex[1].clone()),
        (
            "a wrong token's hash",
            device_password("not-the-token").to_string(),
        ),
    ] {
        assert!(!log.contains(&secret), "{name} reached a log line");
        assert!(!events.contains(&secret), "{name} reached the audit trail");
    }
}
