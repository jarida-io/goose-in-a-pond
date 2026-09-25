//! End-to-end flow against a mock CalDAV server: discovery, `calendar-query`, ingest items.

use chrono::{TimeZone, Utc};
use pond_adapters_caldav::{CalDavAdapter, CalDavConfig, CalDavProvider};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn xml(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(207).set_body_raw(body.to_string(), "application/xml")
}

fn config() -> CalDavConfig {
    CalDavConfig {
        provider: CalDavProvider::Custom {
            base_url: "https://replaced-by-with_base_url".into(),
        },
        username: "jerry@example.org".into(),
        password: "app-password".into(),
    }
}

async fn server() -> MockServer {
    let server = MockServer::start().await;

    // Hop 1: who are these credentials?
    Mock::given(method("PROPFIND"))
        .and(path("/"))
        .respond_with(xml(
            r#"<multistatus xmlns="DAV:"><response><href>/</href><propstat><prop>
               <current-user-principal><href>/principals/jerry/</href></current-user-principal>
               </prop></propstat></response></multistatus>"#,
        ))
        .mount(&server)
        .await;

    // Hop 2: where are their calendars?
    Mock::given(method("PROPFIND"))
        .and(path("/principals/jerry/"))
        .respond_with(xml(
            r#"<multistatus xmlns="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
               <response><href>/principals/jerry/</href><propstat><prop>
               <c:calendar-home-set><href>/calendars/jerry/</href></c:calendar-home-set>
               </prop></propstat></response></multistatus>"#,
        ))
        .mount(&server)
        .await;

    // Hop 3: which are calendars? The home collection is listed too and is NOT one.
    Mock::given(method("PROPFIND"))
        .and(path("/calendars/jerry/"))
        .respond_with(xml(
            r#"<multistatus xmlns="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"
                            xmlns:cs="http://calendarserver.org/ns/">
              <response><href>/calendars/jerry/</href><propstat><prop>
                <displayname>Home collection</displayname>
                <resourcetype><collection/></resourcetype>
              </prop></propstat></response>
              <response><href>/calendars/jerry/personal/</href><propstat><prop>
                <displayname>Personal</displayname>
                <resourcetype><collection/><c:calendar/></resourcetype>
                <cs:getctag>ctag-7</cs:getctag>
              </prop></propstat></response>
            </multistatus>"#,
        ))
        .mount(&server)
        .await;

    // The query itself, answered with a server-expanded recurrence.
    Mock::given(method("REPORT"))
        .and(path("/calendars/jerry/personal/"))
        .respond_with(xml(
            r#"<multistatus xmlns="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
              <response><href>/calendars/jerry/personal/1.ics</href><propstat><prop>
                <c:calendar-data>BEGIN:VCALENDAR
BEGIN:VEVENT
UID:dentist-1
SUMMARY:Dentist
DTSTART:20260817T093000Z
DTEND:20260817T101500Z
LOCATION:Riverside Clinic
ATTENDEE;CN="Liz Adera":mailto:liz@example.org
END:VEVENT
END:VCALENDAR</c:calendar-data>
              </prop></propstat></response>
              <response><href>/calendars/jerry/personal/2.ics</href><propstat><prop>
                <c:calendar-data>BEGIN:VCALENDAR
BEGIN:VEVENT
UID:standup
RECURRENCE-ID:20260818T060000Z
SUMMARY:Standup
DTSTART:20260818T060000Z
END:VEVENT
BEGIN:VEVENT
UID:standup
RECURRENCE-ID:20260819T060000Z
SUMMARY:Standup
DTSTART:20260819T060000Z
END:VEVENT
END:VCALENDAR</c:calendar-data>
              </prop></propstat></response>
            </multistatus>"#,
        ))
        .mount(&server)
        .await;

    server
}

#[tokio::test]
async fn discovery_walks_to_the_calendars_and_skips_the_home_collection() {
    let server = server().await;
    let adapter = CalDavAdapter::new(config())
        .unwrap()
        .with_base_url(format!("{}/", server.uri()));

    let calendars = adapter.discover_calendars().await.expect("discovery");
    assert_eq!(
        calendars.len(),
        1,
        "the home collection is not a calendar: {calendars:?}"
    );
    assert_eq!(calendars[0].display_name, "Personal");
    assert_eq!(calendars[0].ctag.as_deref(), Some("ctag-7"));
    assert!(calendars[0].url.ends_with("/calendars/jerry/personal/"));
}

#[tokio::test]
async fn a_query_becomes_items_the_pipeline_could_ingest() {
    let server = server().await;
    let adapter = CalDavAdapter::new(config())
        .unwrap()
        .with_base_url(format!("{}/", server.uri()));

    let calendars = adapter.discover_calendars().await.unwrap();
    let from = Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    let items = adapter
        .events_in_window(&calendars[0].url, from, to)
        .await
        .expect("query");

    assert_eq!(
        items.len(),
        3,
        "one dentist plus two expanded standups: {items:?}"
    );

    let dentist = items
        .iter()
        .find(|i| i.title == "Dentist")
        .expect("dentist");
    assert_eq!(dentist.external_id, "dentist-1");
    assert_eq!(dentist.participants, vec!["Liz Adera".to_string()]);
    // The body is half of `embedding_text`, so this is the retrieval surface.
    assert!(
        dentist.body.contains("Riverside Clinic"),
        "{}",
        dentist.body
    );
    assert!(
        dentist.body.contains("Monday 17 August 2026"),
        "{}",
        dentist.body
    );

    let standups: Vec<_> = items.iter().filter(|i| i.title == "Standup").collect();
    assert_eq!(standups.len(), 2);
    assert_ne!(standups[0].external_id, standups[1].external_id);
}

#[tokio::test]
async fn a_refused_password_says_what_to_do_about_it() {
    let server = MockServer::start().await;
    Mock::given(method("PROPFIND"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let adapter = CalDavAdapter::new(config())
        .unwrap()
        .with_base_url(format!("{}/", server.uri()));

    let err = adapter.discover_calendars().await.expect_err("must refuse");
    let message = err.to_string();
    assert!(message.contains("app-specific password"), "{message}");
}
