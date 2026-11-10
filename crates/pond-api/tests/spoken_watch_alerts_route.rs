//! A member turns spoken watch matches on from their own phone, and moves their quiet hours; it
//! starts off, and one member's choice is never another's.

#[path = "support/ride_phone.rs"]
mod ride_phone;

use axum::http::{Method, StatusCode};
use ride_phone::{member_and_phone, phone_of_nobody, send};
use serde_json::json;

const ROUTE: &str = "/api/v1/web-watches/spoken-alerts";

#[tokio::test]
async fn spoken_matches_start_off_and_follow_the_members_own_choice() {
    let p = ride_phone::pond().await;
    let (amina, phone) = member_and_phone(&p, "amina").await;
    let (_, other) = member_and_phone(&p, "baraka").await;

    let (status, shown) = send(&p.lan, Method::GET, ROUTE, Some(&phone), None).await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["enabled"], false);
    assert_eq!(shown["quiet_start"], "22:00");
    assert_eq!(shown["quiet_end"], "07:00");
    assert_eq!(shown["household_allows_speech"], false);

    let (status, set) = send(
        &p.lan,
        Method::PUT,
        ROUTE,
        Some(&phone),
        Some(json!({"enabled": true, "quiet_start": "21:30"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{set}");
    assert_eq!(set["enabled"], true);
    assert_eq!(set["quiet_start"], "21:30");
    assert_eq!(set["quiet_end"], "07:00");
    let kept = p.profiles.get(&amina).await.unwrap().unwrap().preferences;
    assert_eq!(kept["web_watch_spoken_alerts"], "true");

    let (status, _) = send(
        &p.lan,
        Method::PUT,
        ROUTE,
        Some(&phone),
        Some(json!({"quiet_end": "7am"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, theirs) = send(&p.lan, Method::GET, ROUTE, Some(&other), None).await;
    assert_eq!(
        theirs["enabled"], false,
        "one member's choice reached another"
    );

    let nobodys = phone_of_nobody(&p, "kiosk").await;
    let (status, _) = send(&p.lan, Method::GET, ROUTE, Some(&nobodys), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
