//! PAI-1 P9's identity half, end to end, through the writers production uses.
//!
//! `device_rung_wiring.rs` next door asserts the wiring is present. This asserts
//! what it does: a phone paired with a member-bound code makes that member the
//! speaker on every turn it sends, at `IdentificationSource::PairedDevice`
//! strength — which outranks face and explicit identification.
//!
//! # Why nothing here writes a column by hand
//!
//! `ProfileScope::Owner` was a no-op in production for a whole phase because
//! every fixture that produced an owned row set `profile_id` directly, which no
//! code path did. So the pairing code is issued through
//! `Handshake::issue_pairing_code_for`, the pair goes through
//! `init_handshake`/`verify_handshake` with a MAC computed the way a phone
//! computes it, the device id comes back out of `Handshake::caller_for_token`,
//! and the member comes out of `DeviceAttribution::device_profile`. The only
//! step written by hand is the `Principal`, and it is written exactly as
//! `auth_middleware` writes it — a guard in `device_rung_wiring.rs` asserts that
//! line is still the one the middleware runs, and that it is the only one in the
//! workspace that may.
//!
//! The two household members are inserted with SQL because there is no profile
//! *port* in this crate's dependency graph to insert them with, and a profile
//! row is not the thing under test.

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use hmac::{Hmac, Mac};
use pond_core::security::domain::proven_device::{DeviceRung, ProvenDevice};
use pond_core::security::ports::handshake::{Handshake, InitRequest, VerifyRequest};
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::services::identity_resolution::{resolve, ResolutionInputs};
use pond_infra::db::Database;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use sha2::Sha256;
use sqlx::{Pool, Sqlite};

type HmacSha256 = Hmac<Sha256>;

struct Pond {
    handshake: SqliteHandshakeAdapter,
    attribution: SqliteDeviceAttribution,
    pool: Pool<Sqlite>,
}

async fn pond_with_two_members() -> Pond {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    for (id, name) in [("liz", "Liz"), ("jerry", "Jerry")] {
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
            .bind(id)
            .bind(name)
            .execute(&db.system)
            .await
            .unwrap();
    }
    // Keep the tempdir alive for the test, else the sqlite file vanishes under
    // the pool.
    std::mem::forget(tmp);
    Pond {
        handshake: SqliteHandshakeAdapter::new(db.system.clone(), None),
        attribution: SqliteDeviceAttribution::new(db.system.clone()),
        pool: db.system,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a phone computes: `HMAC-SHA256(pairing_code, challenge || client_id)`.
fn client_mac(code: &str, challenge_b64: &str, client_id: &str) -> String {
    let challenge = B64.decode(challenge_b64.as_bytes()).unwrap();
    let mut mac = HmacSha256::new_from_slice(code.as_bytes()).unwrap();
    mac.update(&challenge);
    mac.update(client_id.as_bytes());
    hex_lower(&mac.finalize().into_bytes())
}

/// Pair `client_id` with `code` and return the session token the pond issued.
async fn pair(pond: &Pond, code: &str, client_id: &str) -> String {
    let init = pond
        .handshake
        .init_handshake(InitRequest {
            client_id: client_id.into(),
            client_type: "gotg".into(),
            client_version: "1.0".into(),
        })
        .await
        .unwrap();
    let response = pond
        .handshake
        .verify_handshake(VerifyRequest {
            channel_binding: None,
            challenge_id: init.challenge_id,
            mac: client_mac(code, &init.challenge, client_id),
            device_name: Some(format!("{client_id} phone")),
        })
        .await
        .unwrap();
    assert!(response.accepted, "the pair was refused: {response:?}");
    response
        .session_token
        .expect("an accepted pair mints a session token")
}

/// Exactly what `auth_middleware` does with the lookup's answer, and the only
/// hand-written step in this file.
async fn principal_for(pond: &Pond, token: &str) -> Principal {
    match pond.handshake.caller_for_token(token).await {
        Ok(Some(caller)) => Principal::token(caller.client_id).with_device(caller.device_id),
        // The middleware's narrowing branches: a token that names nobody, and a
        // lookup that failed, both produce a principal with no device.
        Ok(None) | Err(_) => Principal::token("unknown"),
    }
}

/// And what `resolve_turn_scope` does with it, minus the `AppState` this crate
/// cannot build.
async fn turn_scope(
    pond: &Pond,
    principal: &Principal,
    session: &SessionIdentity,
    household_has_multiple_members: bool,
) -> (ProfileScope, IdentificationSource) {
    let device = ProvenDevice::from_principal(principal);
    let rung = match device.id() {
        None => DeviceRung::NoDevice,
        Some(id) => device.rung(pond.attribution.device_profile(id).await),
    };
    let resolved = resolve(&ResolutionInputs {
        paired_device_profile: rung.profile_id(),
        session,
        household_has_multiple_members,
    });
    (resolved.scope, resolved.source)
}

/// The whole rung: operator issues Liz a code, Liz's phone pairs with it, and
/// every turn that phone sends is Liz's — with nobody having said so in a
/// request body.
#[tokio::test]
async fn a_paired_device_makes_its_member_the_speaker() {
    let pond = pond_with_two_members().await;
    let code = pond
        .handshake
        .issue_pairing_code_for(Some("liz"))
        .await
        .unwrap();
    let token = pair(&pond, &code.code, "device-liz").await;

    let principal = principal_for(&pond, &token).await;
    assert_eq!(
        principal.device_id.as_deref(),
        Some("device-liz"),
        "the auth layer must surface the device the pond issued this token to; \
         session_tokens.device_id has existed since #93 and nothing read it"
    );

    let (scope, source) = turn_scope(
        &pond,
        &principal,
        &SessionIdentity::unknown(),
        // Two members, so an unidentified speaker would be a Guest. This is the
        // pond where the rung is worth having.
        true,
    )
    .await;
    assert_eq!(
        scope,
        ProfileScope::Owner("liz".to_string()),
        "a turn from Liz's paired phone must resolve to Liz"
    );
    assert_eq!(
        source,
        IdentificationSource::PairedDevice,
        "and it must say so: invariant 3 requires a decision made on a paired token to be \
         distinguishable in an audit log from one made on a 0.6 face match"
    );
}

/// The vacuity control for the test above, and the one this programme has an
/// incident about: prove the fixture is what production produces.
///
/// If `verify_handshake` stopped writing `devices.profile_id`, or the token
/// stopped carrying a device id, the test above would still pass against a
/// hand-written row. Nothing here writes a column: the assertion is that the
/// production pairing path, on its own, leaves a database in which the token
/// names a device and that device names a member.
#[tokio::test]
async fn nothing_but_the_pairing_flow_wrote_any_of_this() -> Result<()> {
    let pond = pond_with_two_members().await;
    let code = pond
        .handshake
        .issue_pairing_code_for(Some("liz"))
        .await
        .unwrap();
    let token = pair(&pond, &code.code, "device-liz").await;

    let stored_profile: Option<Option<String>> =
        sqlx::query_scalar("SELECT profile_id FROM devices WHERE id = 'device-liz'")
            .fetch_optional(&pond.pool)
            .await?;
    assert_eq!(
        stored_profile.flatten().as_deref(),
        Some("liz"),
        "the pairing flow, unaided, must be what attributes the device -- if this row is NULL \
         then every test that resolves a member is testing a fixture no production path builds"
    );

    let caller = pond
        .handshake
        .caller_for_token(&token)
        .await?
        .expect("a freshly issued token must name its caller");
    assert_eq!(caller.device_id, "device-liz");
    assert_eq!(caller.client_id, "device-liz");

    // The derived lookup rides on the same read, so it cannot answer while the
    // device answer is missing.
    assert_eq!(
        pond.handshake.client_id_for_token(&token).await?.as_deref(),
        Some("device-liz"),
        "`client_id_for_token` is derived from `caller_for_token`; if this is None the device \
         lookup is gone too, which is the silent half of the same failure"
    );
    Ok(())
}

/// An unattributed device falls THROUGH. Migration 0043's header: NULL is "no
/// household member has claimed this", never "everybody", and never "whoever
/// paired last".
#[tokio::test]
async fn an_unattributed_device_resolves_nobody() {
    let pond = pond_with_two_members().await;
    // The default, and what every shipped caller of `issue_pairing_code` does.
    let code = pond.handshake.issue_pairing_code().await.unwrap();
    let token = pair(&pond, &code.code, "kitchen-tablet").await;

    let principal = principal_for(&pond, &token).await;
    assert_eq!(
        principal.device_id.as_deref(),
        Some("kitchen-tablet"),
        "the device is still named -- it is registered and usable, it is simply nobody's"
    );

    let (scope, source) = turn_scope(&pond, &principal, &SessionIdentity::unknown(), true).await;
    assert_eq!(
        scope,
        ProfileScope::Guest,
        "an unclaimed shared tablet must leave an unidentified speaker a stranger, not promote \
         them to the last member who paired a phone"
    );
    assert_eq!(source, IdentificationSource::Unknown);
}

/// And the fall-through is a fall-through, not a veto: an unattributed device
/// leaves whatever the session already knew standing.
#[tokio::test]
async fn an_unattributed_device_leaves_an_explicit_identification_alone() {
    let pond = pond_with_two_members().await;
    let code = pond.handshake.issue_pairing_code().await.unwrap();
    let token = pair(&pond, &code.code, "kitchen-tablet").await;
    let principal = principal_for(&pond, &token).await;

    let mut session = SessionIdentity::unknown();
    session.profile_id = Some("jerry".to_string());
    session.source = IdentificationSource::Explicit;

    let (scope, source) = turn_scope(&pond, &principal, &session, true).await;
    assert_eq!(
        scope,
        ProfileScope::Owner("jerry".to_string()),
        "an unattributed device must not override the member this session was bound to; \
         falling through means leaving the next rung standing, not answering over it"
    );
    assert_eq!(source, IdentificationSource::Explicit);
}

/// A device id the CLIENT chose reaches nothing.
///
/// The pairing client's own `client_id` is self-reported, and it is what
/// `verify_handshake` derives the device id from — so this test pairs an
/// UNATTRIBUTED phone whose client id is the same string as Liz's device, and
/// checks it does not thereby become Liz. The structural half (there is no
/// signature a header or body field can satisfy) is in
/// `device_rung_wiring.rs`; this is the half that a type cannot state.
#[tokio::test]
async fn a_client_that_names_liz_s_device_does_not_become_liz() {
    let pond = pond_with_two_members().await;
    let liz_code = pond
        .handshake
        .issue_pairing_code_for(Some("liz"))
        .await
        .unwrap();
    let liz_token = pair(&pond, &liz_code.code, "device-liz").await;
    let (liz_scope, _) = turn_scope(
        &pond,
        &principal_for(&pond, &liz_token).await,
        &SessionIdentity::unknown(),
        true,
    )
    .await;
    assert_eq!(
        liz_scope,
        ProfileScope::Owner("liz".to_string()),
        "vacuity control: Liz's own phone does resolve to Liz, so the refusal below is a \
         refusal and not a resolver that never resolves anybody"
    );

    // A second client pairs claiming to be the same install, with an ordinary
    // code it obtained honestly. `verify_handshake` re-registers the device with
    // `profile_id = excluded.profile_id`, so the attribution is RELEASED rather
    // than inherited.
    let plain = pond.handshake.issue_pairing_code().await.unwrap();
    let impostor_token = pair(&pond, &plain.code, "device-liz").await;

    let (scope, source) = turn_scope(
        &pond,
        &principal_for(&pond, &impostor_token).await,
        &SessionIdentity::unknown(),
        true,
    )
    .await;
    assert_eq!(
        scope,
        ProfileScope::Guest,
        "a client that re-pairs Liz's device id with an ordinary code must not inherit Liz. \
         Losing an attribution is a narrowing the operator can undo by re-issuing; inheriting \
         one is the impersonation this rung exists to prevent"
    );
    assert_eq!(source, IdentificationSource::Unknown);
}

/// A revoked token names nobody, so the rung goes with it.
#[tokio::test]
async fn a_revoked_token_carries_no_device() {
    let pond = pond_with_two_members().await;
    let code = pond
        .handshake
        .issue_pairing_code_for(Some("liz"))
        .await
        .unwrap();
    let token = pair(&pond, &code.code, "device-liz").await;
    pond.handshake.revoke_token(&token).await.unwrap();

    assert_eq!(
        pond.handshake.caller_for_token(&token).await.unwrap(),
        None,
        "a revoked token must name no caller and no device"
    );
    let principal = principal_for(&pond, &token).await;
    assert_eq!(principal.device_id, None);

    let (scope, _) = turn_scope(&pond, &principal, &SessionIdentity::unknown(), true).await;
    assert_eq!(scope, ProfileScope::Guest);
}

/// A failed attribution read identifies nobody.
///
/// The store is broken for real — the `devices` table is dropped out from under
/// a live pool — rather than simulated with a stub, because the thing under test
/// is what a `sqlx` error does to the resolution, and a stub would be asserting
/// that my own `Err` reaches my own match arm.
#[tokio::test]
async fn a_failed_attribution_read_narrows_instead_of_assuming_a_member() {
    let pond = pond_with_two_members().await;
    let code = pond
        .handshake
        .issue_pairing_code_for(Some("liz"))
        .await
        .unwrap();
    let token = pair(&pond, &code.code, "device-liz").await;
    let principal = principal_for(&pond, &token).await;

    // Vacuity control: it resolves Liz while the store is readable.
    let (before, _) = turn_scope(&pond, &principal, &SessionIdentity::unknown(), true).await;
    assert_eq!(before, ProfileScope::Owner("liz".to_string()));

    sqlx::query("DROP TABLE devices")
        .execute(&pond.pool)
        .await
        .expect("the fixture must be able to break the store");

    let device = ProvenDevice::from_principal(&principal);
    let read = pond
        .attribution
        .device_profile(device.id().expect("the principal still names a device"))
        .await;
    assert!(
        read.is_err(),
        "dropping the table must make the read fail; if it does not, the assertion below is \
         testing the happy path"
    );
    let rung = device.rung(read);
    assert!(
        matches!(rung, DeviceRung::Unavailable(_)),
        "a failed read must be Unavailable, not Unattributed: a broken database and a shared \
         tablet must not look the same in a log. Got {rung:?}"
    );
    assert_eq!(
        rung.profile_id(),
        None,
        "an unreadable attribution store must identify nobody. Assuming the member it answered \
         a moment ago is exactly the widening this programme exists to prevent."
    );

    let resolved = resolve(&ResolutionInputs {
        paired_device_profile: rung.profile_id(),
        session: &SessionIdentity::unknown(),
        household_has_multiple_members: true,
    });
    assert_eq!(resolved.scope, ProfileScope::Guest);
}

/// The loopback dev bypass inserts `Principal::loopback()` and returns before a
/// token is read. That principal carries no device, so the rung is unreachable
/// on that path — which is the narrowing answer and deliberately so.
#[tokio::test]
async fn a_loopback_principal_resolves_no_member_however_many_phones_are_paired() {
    let pond = pond_with_two_members().await;
    let code = pond
        .handshake
        .issue_pairing_code_for(Some("liz"))
        .await
        .unwrap();
    let _ = pair(&pond, &code.code, "device-liz").await;

    for principal in [Principal::loopback(), Principal::internal()] {
        let (scope, source) =
            turn_scope(&pond, &principal, &SessionIdentity::unknown(), true).await;
        assert_eq!(
            scope,
            ProfileScope::Guest,
            "{:?} presented no token, so it has no device the pond issued one to. Resolving it \
             to the most recently paired phone would make every local request speak as whoever \
             last paired.",
            principal.kind
        );
        assert_eq!(source, IdentificationSource::Unknown);
    }
}
