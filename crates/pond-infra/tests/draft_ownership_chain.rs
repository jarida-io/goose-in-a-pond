//! The draft-ownership chain end to end on real SQLite, every column written by production code.
//! Unit tests stub `RepoDraftAuthority`, so only this sees a chain that stamps owners NULL.

use std::sync::Arc;

use pond_core::security::ports::draft_authority::DraftAuthority;
use pond_core::security::ports::policy::{
    is_draft_decision_permitted, PolicyMode, REASON_FOREIGN_DRAFT, REASON_UNRESOLVED_ACTOR,
};
use pond_core::security::services::draft_authority::RepoDraftAuthority;
use pond_core::user_data::domain::profile::{CreateProfileRequest, ProfileScope};
use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::db::Database;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_settings::SqliteSettingsRepository;

struct Fixture {
    authority: RepoDraftAuthority,
    liz: String,
    jerry: String,
    _tmp: tempfile::TempDir,
}

/// Two members and a session owned by one, bound and engine-paired via production writers.
async fn two_member_pond(engine_session_id: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();

    let profiles = Arc::new(SqliteProfileRepository::new(db.system.clone()));
    let sessions = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let settings = Arc::new(SqliteSettingsRepository::new(db.system.clone()));

    let liz = profiles
        .create(CreateProfileRequest {
            display_name: "Liz".to_string(),
            avatar_emoji: "L".to_string(),
        })
        .await
        .unwrap()
        .id;
    let jerry = profiles
        .create(CreateProfileRequest {
            display_name: "Jerry".to_string(),
            avatar_emoji: "J".to_string(),
        })
        .await
        .unwrap()
        .id;

    sessions
        .create_session("giap-sess-1".to_string())
        .await
        .unwrap();
    let bound = sessions
        .set_session_identity_if_stronger(
            "giap-sess-1",
            &SessionIdentity {
                profile_id: Some(liz.clone()),
                source: IdentificationSource::Explicit,
                confidence: None,
            },
        )
        .await
        .unwrap();
    assert!(bound, "the identity write must actually land");

    sessions
        .set_engine_session_id("giap-sess-1", engine_session_id)
        .await
        .unwrap();

    Fixture {
        authority: RepoDraftAuthority::new(settings, sessions, profiles, None),
        liz,
        jerry,
        _tmp: tmp,
    }
}

/// If this yields `Household`/`None`, `save_draft` stamps NULL owners and the rule is inert.
#[tokio::test]
async fn an_engine_session_resolves_to_the_member_who_owns_the_giap_session() {
    let f = two_member_pond("20260805_9").await;

    let actor = f.authority.actor_for_engine_session("20260805_9").await;
    let (scope, source) = actor.expect("the engine session must resolve to a speaker");

    assert_eq!(
        scope,
        ProfileScope::Owner(f.liz.clone()),
        "a two-member pond with an explicitly bound session must name the member"
    );
    assert_eq!(
        source,
        IdentificationSource::Explicit,
        "provenance must survive the whole chain: PAI-1 invariant 3"
    );

    // ...and the rule's `Some(owner)` arm discriminates on it both ways.
    assert_eq!(
        is_draft_decision_permitted(
            Some(&scope),
            "20260805_9",
            Some(f.jerry.as_str()),
            "20260805_9"
        ),
        Err(REASON_FOREIGN_DRAFT),
        "the reported hole, with a scope resolved by production code"
    );
    assert_eq!(
        is_draft_decision_permitted(
            Some(&scope),
            "20260805_9",
            Some(f.liz.as_str()),
            "20260805_9"
        ),
        Ok(())
    );
}

/// Unmapped is common on upgraded ponds: `engine_session_map` (0032) lacks older sessions.
#[tokio::test]
async fn an_unmapped_or_blank_engine_session_refuses_rather_than_widens() {
    let f = two_member_pond("20260805_9").await;

    for engine in ["20260805_nope", "", "   "] {
        let actor = f.authority.actor_for_engine_session(engine).await;
        assert!(
            actor.is_none(),
            "engine session {engine:?} must be unresolvable, got {actor:?}"
        );
        assert_eq!(
            is_draft_decision_permitted(None, engine, Some(f.liz.as_str()), "giap-sess-1"),
            Err(REASON_UNRESOLVED_ACTOR)
        );
    }
}

/// An unconfigured pond must never silently be in `off`.
#[tokio::test]
async fn the_mode_comes_from_settings_and_defaults_to_audit() {
    let f = two_member_pond("20260805_9").await;
    assert_eq!(f.authority.policy_mode().await, PolicyMode::Audit);
}
