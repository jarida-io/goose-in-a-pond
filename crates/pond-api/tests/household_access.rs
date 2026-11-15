//! Who may change the household: its owner is the pairing lock's main member, and only they
//! change roles, the primary member and the household's members.

#[path = "support/household.rs"]
mod household;

use axum::http::{Method, StatusCode};
use household::{household, ADA, JERRY, LIZ};
use pond_core::user_data::domain::profile::Role;
use serde_json::json;

#[tokio::test]
async fn a_member_cannot_make_themselves_the_owner_through_settings() {
    let h = household().await;

    let (status, body) = h
        .send(
            Method::PUT,
            "/api/v1/settings",
            LIZ,
            Some(json!({"primary_profile_id": h.liz})),
        )
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(h.role_of(&h.liz).await, Role::Member);
    assert_eq!(h.role_of(&h.jerry).await, Role::Owner);
}

#[tokio::test]
async fn naming_the_primary_member_names_no_owner() {
    let h = household().await;

    let (status, body) = h
        .send(
            Method::PUT,
            "/api/v1/settings",
            JERRY,
            Some(json!({"primary_profile_id": h.liz})),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.role_of(&h.liz).await, Role::Member);
    assert_eq!(h.role_of(&h.jerry).await, Role::Owner);
}

#[tokio::test]
async fn resaving_the_same_primary_member_is_anyones_to_do() {
    let h = household().await;

    let (status, body) = h
        .send(
            Method::PUT,
            "/api/v1/settings",
            ADA,
            Some(json!({"primary_profile_id": h.jerry})),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn handing_the_main_members_seat_on_hands_the_owner_role_with_it() {
    let h = household().await;

    let (status, body) = h
        .send(
            Method::PUT,
            "/api/v1/pairing/owner",
            JERRY,
            Some(json!({"profile_id": h.liz})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(h.role_of(&h.liz).await, Role::Owner);
    assert_eq!(h.role_of(&h.jerry).await, Role::Member);
    let (status, _) = h
        .send(
            Method::PUT,
            &format!("/api/v1/profiles/{}/role", h.ada),
            JERRY,
            Some(json!({"role": "member"})),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the old owner manages no roles"
    );
    let (status, body) = h
        .send(
            Method::PUT,
            &format!("/api/v1/profiles/{}/role", h.ada),
            LIZ,
            Some(json!({"role": "member"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn naming_another_owner_hands_the_main_members_seat_on() {
    let h = household().await;

    let (status, body) = h
        .send(
            Method::PUT,
            &format!("/api/v1/profiles/{}/role", h.liz),
            JERRY,
            Some(json!({"role": "owner"})),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], "owner", "{body}");
    assert_eq!(h.main_member().await.as_deref(), Some(h.liz.as_str()));
    assert_eq!(
        h.role_of(&h.jerry).await,
        Role::Member,
        "one owner, never two"
    );
}

#[tokio::test]
async fn the_owners_own_role_changes_only_by_handing_it_on() {
    let h = household().await;

    let (status, body) = h
        .send(
            Method::PUT,
            &format!("/api/v1/profiles/{}/role", h.jerry),
            JERRY,
            Some(json!({"role": "member"})),
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(h.role_of(&h.jerry).await, Role::Owner);
}

#[tokio::test]
async fn a_stale_owner_role_grants_nothing_once_the_seat_has_moved() {
    let h = household().await;
    h.state
        .handshake
        .set_pairing_owner(&h.liz, true)
        .await
        .unwrap();

    let (status, body) = h
        .send(
            Method::PUT,
            &format!("/api/v1/profiles/{}/role", h.ada),
            JERRY,
            Some(json!({"role": "member"})),
        )
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn only_the_owner_removes_a_member() {
    let h = household().await;
    let delete = |id: &str| format!("/api/v1/profiles/{id}");

    let (status, body) = h.send(Method::DELETE, &delete(&h.liz), ADA, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a guest: {body}");
    let (status, body) = h.send(Method::DELETE, &delete(&h.ada), LIZ, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a member: {body}");
    assert_eq!(h.role_of(&h.liz).await, Role::Member, "Liz is still here");
    assert_eq!(h.role_of(&h.ada).await, Role::Guest, "Ada is still here");

    let (status, body) = h.send(Method::DELETE, &delete(&h.ada), JERRY, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn the_owner_is_not_removed_until_the_seat_is_handed_on() {
    let h = household().await;

    let (status, body) = h
        .send(
            Method::DELETE,
            &format!("/api/v1/profiles/{}", h.jerry),
            JERRY,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(h.role_of(&h.jerry).await, Role::Owner);
}
