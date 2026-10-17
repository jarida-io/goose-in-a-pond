//! A phone drives the setup channel end to end over an in-memory transport: the real SQLite
//! handshake and setup record behind it, pond-infra's CPace responder in front, at the smallest
//! ATT_MTU and at a typical negotiated one.

mod common;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use common::*;
use pond_connect::setup_channel::SessionEnd;
use pond_core::network::ports::setup_code_store::SetupCodeStore;
use pond_core::security::domain::event::AttributeValue;
use pond_core::security::ports::handshake::{Handshake, RefreshRequest};
use pond_infra::pairing_cpace::device_password;
use serde_json::{json, Value};

#[tokio::test]
async fn a_phone_claims_a_pond_with_its_card_and_puts_it_on_wifi() {
    for att_mtu in MTUS {
        let pond = pond().await;
        let (phone, _) = claim_and_join(&pond, att_mtu).await;
        let paired = pond
            .events
            .all()
            .into_iter()
            .find(|event| event.action == "auth.device_paired")
            .expect("the pairing is audited");
        assert_eq!(
            paired.attributes.get("method"),
            Some(&AttributeValue::from("cpace-setup"))
        );
        assert_eq!(
            paired.attributes.get("transport"),
            Some(&AttributeValue::from("setup_channel"))
        );
        phone.handle.writer.phone_disconnected().await;
        assert_eq!(phone.ended().await, Some(SessionEnd::PhoneDisconnected));
    }
}

#[tokio::test]
async fn a_phone_without_the_card_pairs_with_a_code_it_hears() {
    for att_mtu in MTUS {
        let pond = pond().await;
        let mut phone = Phone::connect(&pond, att_mtu, "install-heard").await;
        let spoken = phone.ask("spoken_code", json!({})).await;
        assert_eq!(spoken["ok"], true);
        assert_eq!(spoken["spoken"], true);
        let heard = wait_for_announcement(&pond.announcer).await;
        assert!(
            !String::from_utf8_lossy(&phone.heard).contains(&heard),
            "the code is never sent, only spoken"
        );

        let again = phone.ask("spoken_code", json!({})).await;
        assert_eq!(again["error"], "spoken_code_limited");
        assert_eq!(again["retry_after"], 30);

        let paired = phone
            .pake("pairing", &heard)
            .await
            .expect("the heard code pairs");
        assert!(pond
            .handshake
            .validate_token(&paired.session_token)
            .await
            .unwrap());
        let status = phone.ask("status", json!({})).await;
        assert_eq!(status["claimed"], true, "any first pairing claims the pond");
    }
}

#[tokio::test]
async fn a_claimed_pond_refuses_its_card_over_the_channel() {
    let pond = pond().await;
    let (first, _) = claim_and_join(&pond, 185).await;
    first.handle.writer.phone_disconnected().await;
    first.ended().await;
    wait_until_free(&pond.channel).await;

    let mut phone = Phone::connect(&pond, 23, "install-late").await;
    let refusal = phone.pake("setup", &pond.code.clone()).await.err().unwrap();
    assert_eq!(
        refusal,
        json!({"id": refusal["id"], "type": "pake_init", "ok": false,
               "error": "setup_code_unavailable"})
    );
    assert!(phone.cipher.is_none());
    let hello = phone.ask("hello", json!({})).await;
    assert_eq!(hello["claimed"], true);
}

#[tokio::test]
async fn a_wrong_card_is_refused_without_keys_or_tokens() {
    let pond = pond().await;
    let mut phone = Phone::connect(&pond, 23, "install-typo").await;
    let refusal = phone.pake("setup", "34970112330").await.err().unwrap();
    assert_eq!(
        refusal,
        json!({"id": refusal["id"], "type": "pake_verify", "ok": false, "accepted": false,
               "error": "invalid_confirmation", "rejection_reason": "invalid_confirmation"})
    );
    assert!(!pond
        .store
        .load()
        .await
        .unwrap()
        .unwrap()
        .claim_state()
        .is_claimed());
    let status = phone.ask("status", json!({})).await;
    assert_eq!(status["error"], "wrong_phase", "still in the clear");
}

#[tokio::test]
async fn a_tampered_ciphertext_ends_the_session() {
    let pond = pond().await;
    let (mut phone, _) = claim_and_join(&pond, 23).await;
    let cipher = phone.cipher.as_mut().unwrap();
    let envelope: Value = serde_json::from_slice(
        &cipher
            .seal_envelope(br#"{"id":90,"type":"status"}"#)
            .unwrap(),
    )
    .unwrap();
    let mut sealed = B64.decode(envelope["sealed"].as_str().unwrap()).unwrap();
    let last = sealed.len() - 1;
    sealed[last] ^= 0x01;
    let tampered = serde_json::to_vec(&json!({"sealed": B64.encode(sealed)})).unwrap();
    assert!(phone.send(&tampered).await);
    assert_eq!(phone.receive().await, None);
    assert_eq!(phone.ended().await, Some(SessionEnd::SealingFailure));
    wait_until_free(&pond.channel).await;
}

#[tokio::test]
async fn a_replayed_message_ends_the_session() {
    let pond = pond().await;
    let (mut phone, _) = claim_and_join(&pond, 185).await;
    let replay = phone
        .cipher
        .as_mut()
        .unwrap()
        .seal_envelope(br#"{"id":91,"type":"status"}"#)
        .unwrap();
    assert!(phone.send(&replay).await);
    let answer = phone.receive().await.expect("the first copy is answered");
    let answer: Value = serde_json::from_slice(
        &phone
            .cipher
            .as_mut()
            .unwrap()
            .open_envelope(&answer)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(answer["ok"], true);
    assert!(phone.send(&replay).await);
    assert_eq!(
        phone.receive().await,
        None,
        "the same counter twice ends the session"
    );
    assert_eq!(phone.ended().await, Some(SessionEnd::SealingFailure));
}

#[tokio::test]
async fn sealed_requests_before_the_exchange_are_refused() {
    let pond = pond().await;
    let mut phone = Phone::connect(&pond, 23, "install-early").await;
    for kind in ["status", "wifi_scan"] {
        let answer = phone.ask(kind, json!({})).await;
        assert_eq!(answer["error"], "wrong_phase", "{kind}");
    }
    let join = phone
        .ask(
            "wifi_join",
            json!({"ssid": "Home", "passphrase": HOME_PASSPHRASE}),
        )
        .await;
    assert_eq!(join["error"], "wrong_phase");
    assert!(
        pond.station.joins().is_empty(),
        "nothing reached the station"
    );
    assert_eq!(
        phone.ask("hello", json!({})).await["ok"],
        true,
        "and the session goes on"
    );

    // An envelope before any keys exist is unreadable, and that ends the session.
    assert!(
        phone
            .send(br#"{"sealed":"AAAAAAAAAAAAAAAAAAAAAA=="}"#)
            .await
    );
    assert_eq!(phone.receive().await, None);
    assert_eq!(phone.ended().await, Some(SessionEnd::UnreadableFrame));
}

#[tokio::test]
async fn a_second_phone_is_refused_while_a_session_is_open() {
    let pond = pond().await;
    let first = Phone::connect(&pond, 23, "install-a").await;
    assert!(pond.channel.open().is_err());
    first.handle.writer.phone_disconnected().await;
    first.ended().await;
    wait_until_free(&pond.channel).await;
    assert!(pond.channel.open().is_ok());
}

/// The refused verify a phone reads when its password is wrong, whatever made it wrong.
fn invalid_confirmation(id: &Value) -> Value {
    json!({"id": id, "type": "pake_verify", "ok": false, "accepted": false,
           "error": "invalid_confirmation", "rejection_reason": "invalid_confirmation"})
}

#[tokio::test]
async fn a_paired_phone_resumes_over_the_channel_and_its_old_refresh_token_stops_working() {
    for att_mtu in MTUS {
        let pond = pond().await;
        let (first, paired) = claim_and_join(&pond, att_mtu).await;
        first.leave(&pond).await;

        let mut phone = Phone::connect(&pond, att_mtu, "install-e2e").await;
        let resumed = phone
            .pake("device", &device_password(&paired.refresh_token))
            .await
            .expect("the paired phone resumes with no code");
        assert_ne!(resumed.refresh_token, paired.refresh_token);
        assert!(pond
            .handshake
            .validate_token(&resumed.session_token)
            .await
            .unwrap());
        assert!(!pond
            .handshake
            .validate_token(&paired.session_token)
            .await
            .unwrap());
        let replay = pond
            .handshake
            .refresh(RefreshRequest {
                refresh_token: paired.refresh_token.clone(),
            })
            .await
            .unwrap();
        assert_eq!(
            replay.rejection_reason.as_deref(),
            Some("invalid_or_expired_refresh"),
            "the old refresh token is spent"
        );

        let status = phone.ask("status", json!({})).await;
        assert_eq!(status["ok"], true, "the channel is sealed");
        assert_eq!(status["claimed"], true);
        let record = pond.store.load().await.unwrap().unwrap();
        assert_eq!(record.claimed_by.as_deref(), Some("install-e2e"));

        let event = pond
            .events
            .all()
            .into_iter()
            .find(|event| event.action == "auth.device_resumed")
            .expect("the resume is audited as a resume");
        assert_eq!(
            event.attributes.get("method"),
            Some(&AttributeValue::from("cpace-device"))
        );
        assert_eq!(
            event.attributes.get("transport"),
            Some(&AttributeValue::from("setup_channel"))
        );
        assert_eq!(
            pond.events
                .all()
                .iter()
                .filter(|event| event.action == "auth.device_paired")
                .count(),
            1,
            "a resume is not a new pairing"
        );
        phone.leave(&pond).await;
    }
}

#[tokio::test]
async fn a_rotated_refresh_token_cannot_resume_and_the_current_one_can() {
    let pond = pond().await;
    let (first, paired) = claim_and_join(&pond, 185).await;
    first.leave(&pond).await;
    let rotated = pond
        .handshake
        .refresh(RefreshRequest {
            refresh_token: paired.refresh_token.clone(),
        })
        .await
        .unwrap();
    assert!(rotated.accepted);

    let mut phone = Phone::connect(&pond, 23, "install-e2e").await;
    let refusal = phone
        .pake("device", &device_password(&paired.refresh_token))
        .await
        .err()
        .unwrap();
    assert_eq!(refusal, invalid_confirmation(&refusal["id"]));
    assert!(phone.cipher.is_none(), "no keys for a stale token");

    let current = rotated.refresh_token.unwrap();
    let resumed = phone
        .pake("device", &device_password(&current))
        .await
        .expect("the current token resumes on the same connection");
    assert!(pond
        .handshake
        .validate_token(&resumed.session_token)
        .await
        .unwrap());
}

#[tokio::test]
async fn an_unknown_install_id_is_refused_exactly_as_a_wrong_token_is() {
    let pond = pond().await;
    let (first, _) = claim_and_join(&pond, 185).await;
    first.leave(&pond).await;

    let mut known = Phone::connect(&pond, 23, "install-e2e").await;
    let wrong = known
        .pake("device", &device_password("not-the-token"))
        .await
        .err()
        .unwrap();
    known.leave(&pond).await;
    let mut stranger = Phone::connect(&pond, 23, "install-stranger").await;
    let unknown = stranger
        .pake("device", &device_password("not-the-token"))
        .await
        .err()
        .unwrap();

    assert_eq!(wrong, invalid_confirmation(&wrong["id"]));
    assert_eq!(
        unknown, wrong,
        "the answer says nothing about which ids are paired"
    );
    let failures: Vec<_> = pond
        .events
        .all()
        .into_iter()
        .filter(|event| event.action == "auth.device_resume_failed")
        .map(|event| event.attributes)
        .collect();
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0], failures[1], "and neither does the audit trail");
}

#[tokio::test]
async fn five_failed_resumes_cool_the_device_and_each_connection_still_gets_three_inits() {
    let pond = pond().await;
    let (first, paired) = claim_and_join(&pond, 185).await;
    first.leave(&pond).await;
    let wrong = device_password("not-the-token");

    let mut phone = Phone::connect(&pond, 185, "install-e2e").await;
    for _ in 0..3 {
        let refusal = phone.pake("device", &wrong).await.err().unwrap();
        assert_eq!(refusal["error"], "invalid_confirmation");
    }
    let fourth = phone.pake("device", &wrong).await.err().unwrap();
    assert_eq!(
        fourth,
        json!({"id": fourth["id"], "type": "pake_init", "ok": false, "error": "too_many_attempts"})
    );
    assert_eq!(phone.ended().await, Some(SessionEnd::AttemptsExhausted));
    wait_until_free(&pond.channel).await;

    let mut phone = Phone::connect(&pond, 185, "install-e2e").await;
    for _ in 0..2 {
        let refusal = phone.pake("device", &wrong).await.err().unwrap();
        assert_eq!(refusal["error"], "invalid_confirmation");
    }
    let cooling = phone
        .pake("device", &device_password(&paired.refresh_token))
        .await
        .err()
        .unwrap();
    assert_eq!(cooling["type"], "pake_init");
    assert_eq!(cooling["ok"], false, "a refusal in the clear is ok: false");
    assert_eq!(cooling["error"], "device_cooling");
    let wait = cooling["retry_after"].as_u64().expect("whole seconds");
    assert!((1..=600).contains(&wait), "waits {wait} s");
    assert_eq!(cooling.as_object().unwrap().len(), 5, "{cooling}");
    assert!(phone.cipher.is_none());
    assert!(
        pond.handshake
            .refresh(RefreshRequest {
                refresh_token: paired.refresh_token,
            })
            .await
            .unwrap()
            .accepted,
        "cooling stops resumes over the channel, never the phone's session"
    );
}
