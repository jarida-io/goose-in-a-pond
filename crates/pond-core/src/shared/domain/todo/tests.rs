use super::*;
use crate::shared::domain::session_activity::{PresenceTransition, ProfilePresence};
use crate::shared::ports::event_bus::BusEvent;
use crate::user_data::domain::device::{DeviceId, DeviceStateChanged, DeviceStateValue};
use crate::user_data::domain::sensor::{CameraEvent, SensorReading};
use crate::user_data::domain::session::IdentificationSource;

fn utc(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("{raw}: {e}"))
        .with_timezone(&Utc)
}

fn clock(raw: &str) -> NaiveTime {
    parse_clock(raw).unwrap_or_else(|| panic!("{raw} is a clock time"))
}

const NAIROBI: Tz = chrono_tz::Africa::Nairobi;
const NEW_YORK: Tz = chrono_tz::America::New_York;
const LONDON: Tz = chrono_tz::Europe::London;

fn next(every: Every, at: &str, days: &[u32], after: &str, tz: Tz) -> String {
    rfc3339(
        next_repeat(every, clock(at), days, utc(after), tz)
            .unwrap_or_else(|| panic!("{every:?} at {at} after {after} never falls")),
    )
}

// ── Repeats ─────────────────────────────────────────────────────────────────

#[test]
fn a_daily_repeat_in_nairobi_falls_at_its_local_hour_every_day() {
    // Monday 15:00 in Nairobi: today's 18:00 is still ahead.
    assert_eq!(
        next(Every::Day, "18:00", &[], "2026-11-09T12:00:00Z", NAIROBI),
        "2026-11-09T15:00:00Z"
    );
    // Exactly at the firing: strictly after, so tomorrow, not the same instant again.
    assert_eq!(
        next(Every::Day, "18:00", &[], "2026-11-09T15:00:00Z", NAIROBI),
        "2026-11-10T15:00:00Z"
    );
}

#[test]
fn a_weekday_repeat_skips_the_weekend() {
    // Friday 19:00 in Nairobi: the next weekday 18:00 is Monday.
    assert_eq!(
        next(
            Every::Weekday,
            "18:00",
            &[],
            "2026-11-13T16:00:00Z",
            NAIROBI
        ),
        "2026-11-16T15:00:00Z"
    );
}

#[test]
fn a_weekly_repeat_falls_only_on_its_days() {
    // Monday early morning; Tuesday and Thursday at 07:30.
    assert_eq!(
        next(
            Every::Week,
            "07:30",
            &[2, 4],
            "2026-11-09T00:00:00Z",
            NAIROBI
        ),
        "2026-11-10T04:30:00Z"
    );
    assert_eq!(
        next(
            Every::Week,
            "07:30",
            &[2, 4],
            "2026-11-10T04:30:00Z",
            NAIROBI
        ),
        "2026-11-12T04:30:00Z"
    );
}

#[test]
fn a_monthly_repeat_on_the_31st_falls_on_the_last_day_of_a_short_month() {
    let tz = chrono_tz::UTC;
    assert_eq!(
        next(Every::Month, "09:00", &[31], "2026-02-01T00:00:00Z", tz),
        "2026-02-28T09:00:00Z"
    );
    assert_eq!(
        next(Every::Month, "09:00", &[31], "2026-02-28T09:00:00Z", tz),
        "2026-03-31T09:00:00Z"
    );
}

#[test]
fn a_repeat_keeps_its_local_hour_across_the_end_of_daylight_saving() {
    // 18:00 is 22:00 UTC under EDT and 23:00 UTC under EST; the clocks went back on 1 November.
    assert_eq!(
        next(Every::Day, "18:00", &[], "2026-10-31T12:00:00Z", NEW_YORK),
        "2026-10-31T22:00:00Z"
    );
    assert_eq!(
        next(Every::Day, "18:00", &[], "2026-10-31T22:00:00Z", NEW_YORK),
        "2026-11-01T23:00:00Z"
    );
}

#[test]
fn a_repeat_in_the_hour_a_spring_change_skips_falls_just_after_the_gap() {
    // 02:30 does not exist in New York on 8 March 2026; 03:00 EDT is 07:00 UTC.
    assert_eq!(
        next(Every::Day, "02:30", &[], "2026-03-08T05:00:00Z", NEW_YORK),
        "2026-03-08T07:00:00Z"
    );
}

#[test]
fn a_repeat_in_the_hour_an_autumn_change_repeats_falls_once() {
    // 01:30 happens twice on 1 November 2026 in New York; only the first counts.
    assert_eq!(
        next(Every::Day, "01:30", &[], "2026-11-01T04:00:00Z", NEW_YORK),
        "2026-11-01T05:30:00Z"
    );
    assert_eq!(
        next(Every::Day, "01:30", &[], "2026-11-01T05:30:00Z", NEW_YORK),
        "2026-11-02T06:30:00Z"
    );
}

#[test]
fn a_repeat_follows_london_into_summer_time() {
    // 09:00 GMT is 09:00 UTC; from 29 March 09:00 BST is 08:00 UTC.
    assert_eq!(
        next(Every::Day, "09:00", &[], "2026-03-27T12:00:00Z", LONDON),
        "2026-03-28T09:00:00Z"
    );
    assert_eq!(
        next(Every::Day, "09:00", &[], "2026-03-28T12:00:00Z", LONDON),
        "2026-03-29T08:00:00Z"
    );
}

#[test]
fn a_weekly_or_monthly_repeat_with_no_days_never_falls() {
    assert_eq!(
        next_repeat(
            Every::Week,
            clock("09:00"),
            &[],
            utc("2026-11-09T00:00:00Z"),
            NAIROBI
        ),
        None
    );
}

#[test]
fn clock_times_parse_as_people_write_them() {
    assert_eq!(clock("18:00"), NaiveTime::from_hms_opt(18, 0, 0).unwrap());
    assert_eq!(clock("6pm"), NaiveTime::from_hms_opt(18, 0, 0).unwrap());
    assert_eq!(clock("6:30 am"), NaiveTime::from_hms_opt(6, 30, 0).unwrap());
    assert_eq!(clock("12am"), NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    assert_eq!(parse_clock("25:00"), None);
    assert_eq!(parse_clock("evening"), None);
}

// ── Labels ──────────────────────────────────────────────────────────────────

fn repeat(every: Every, at: &str, days: &[u32]) -> HookSpec {
    HookSpec::Repeat {
        every,
        at: clock(at),
        days: days.to_vec(),
    }
}

#[test]
fn a_repeat_reads_as_a_plain_sentence() {
    let now = utc("2026-11-09T09:00:00Z");
    let label = |spec: HookSpec| clock_label(&spec, now, NAIROBI);
    assert_eq!(
        label(repeat(Every::Weekday, "18:00", &[])),
        "Every weekday at 18:00"
    );
    assert_eq!(
        label(repeat(Every::Day, "07:00", &[])),
        "Every day at 07:00"
    );
    assert_eq!(
        label(repeat(Every::Week, "18:00", &[1, 4])),
        "Every Monday and Thursday at 18:00"
    );
    assert_eq!(
        label(repeat(Every::Week, "07:30", &[1, 3, 5])),
        "Every Monday, Wednesday and Friday at 07:30"
    );
    assert_eq!(
        label(repeat(Every::Week, "18:00", &[1, 2, 3, 4, 5])),
        "Every weekday at 18:00"
    );
    assert_eq!(
        label(repeat(Every::Month, "09:00", &[1, 15])),
        "Every month on the 1st and 15th at 09:00"
    );
    assert_eq!(
        label(repeat(Every::Month, "09:00", &[2, 11, 22, 23, 31])),
        "Every month on the 2nd, 11th, 22nd, 23rd and 31st at 09:00"
    );
}

#[test]
fn a_time_reads_in_the_members_timezone_with_the_year_only_when_it_differs() {
    let now = utc("2026-11-09T09:00:00Z");
    assert_eq!(
        clock_label(
            &HookSpec::Time {
                at: utc("2026-11-10T15:00:00Z")
            },
            now,
            NAIROBI
        ),
        "On Tuesday 10 November at 18:00"
    );
    assert_eq!(
        clock_label(
            &HookSpec::Time {
                at: utc("2027-01-05T06:00:00Z")
            },
            now,
            NAIROBI
        ),
        "On Tuesday 5 January 2027 at 09:00"
    );
}

#[test]
fn a_place_reads_as_somewhere_you_go() {
    assert_eq!(
        place_sentence("Home", Transition::Arrive),
        "When you get home"
    );
    assert_eq!(
        place_sentence("home", Transition::Leave),
        "When you leave home"
    );
    assert_eq!(
        place_sentence("Gym", Transition::Arrive),
        "When you get to the gym"
    );
    assert_eq!(
        place_sentence("Work", Transition::Leave),
        "When you leave work"
    );
    assert_eq!(
        place_sentence("Kilimani flat", Transition::Arrive),
        "When you get to Kilimani flat"
    );
}

fn catalog() -> HookCatalog {
    let now = utc("2026-11-09T09:00:00Z");
    HookCatalog {
        places: vec![Place {
            id: "p-home".into(),
            owner: "jerry".into(),
            label: "Home".into(),
            lat: -1.29,
            lng: 36.82,
            radius_m: 150.0,
            created_at: now,
        }],
        sources: vec![
            HookSource::presence(),
            HookSource::camera("camera-1"),
            HookSource::device("matter-3", "Kitchen lamp"),
            HookSource::web_watch(
                "w1",
                "Used Toyota Fielder for sale in Kenya | Jiji",
                "https://jiji.co.ke/cars/toyota-fielder",
            ),
            HookSource::cart("c1", "Fielder", "kilimall.co.ke", ""),
            HookSource::cart(
                "c2",
                "Toyota Fielder 2016 Silver - Kilimall Kenya",
                "",
                "https://www.kilimall.co.ke/listing/1",
            ),
            HookSource::mail("mail:gmail:jerry", "gmail"),
            HookSource::calendar("calendar:google:jerry", "google"),
            HookSource::webhook("wh1", "Doorbell"),
        ],
    }
}

fn label_of(kind: &str, spec: Value) -> String {
    resolve_hook(
        kind,
        &spec,
        &catalog(),
        utc("2026-11-09T09:00:00Z"),
        NAIROBI,
    )
    .unwrap_or_else(|e| panic!("{kind} {spec}: {e}"))
    .label
}

#[test]
fn every_hook_kind_is_labelled_as_a_plain_sentence() {
    assert_eq!(
        label_of(
            "place",
            json!({"place_id": "p-home", "transition": "arrive"})
        ),
        "When you get home"
    );
    assert_eq!(
        label_of(
            "event",
            json!({"source_id": "presence:home", "event": "leave"})
        ),
        "When you leave home"
    );
    assert_eq!(
        label_of(
            "event",
            json!({"source_id": "camera:camera-1", "event": "person"})
        ),
        "When Camera 1 sees a person"
    );
    assert_eq!(
        label_of(
            "event",
            json!({"source_id": "device:matter-3", "event": "on"})
        ),
        "When Kitchen lamp turns on"
    );
    assert_eq!(
        label_of(
            "event",
            json!({"source_id": "web_watch:w1", "event": "match"})
        ),
        "When your watch on Jiji finds a match"
    );
    assert_eq!(
        label_of(
            "event",
            json!({"source_id": "cart:c1", "event": "price_drop"})
        ),
        "When the Fielder on Kilimall drops in price"
    );
    assert_eq!(
        label_of(
            "event",
            json!({"source_id": "cart:c2", "event": "back_in_stock"})
        ),
        "When the Toyota Fielder 2016 Silver on Kilimall is back in stock"
    );
    assert_eq!(
        label_of(
            "connector",
            json!({"connector_id": "mail:mail:gmail:jerry", "event": "new_mail", "filter": "Emmanuel"})
        ),
        "When a mail from Emmanuel arrives"
    );
    assert_eq!(
        label_of(
            "connector",
            json!({"connector_id": "mail:mail:gmail:jerry", "event": "new_mail"})
        ),
        "When a new mail arrives in Gmail"
    );
    assert_eq!(
        label_of(
            "connector",
            json!({"connector_id": "calendar:calendar:google:jerry", "event": "event_starting"})
        ),
        "When an event on your Google calendar is about to start"
    );
    assert_eq!(
        label_of(
            "connector",
            json!({"connector_id": "webhook:wh1", "event": "call"})
        ),
        "When Doorbell sends an event"
    );
    assert_eq!(
        label_of("repeat", json!({"every": "weekday", "at": "18:00"})),
        "Every weekday at 18:00"
    );
}

// ── Validation ──────────────────────────────────────────────────────────────

fn refused(kind: &str, spec: Value) -> String {
    match resolve_hook(
        kind,
        &spec,
        &catalog(),
        utc("2026-11-09T09:00:00Z"),
        NAIROBI,
    ) {
        Ok(hook) => panic!("{kind} {spec} was accepted as {hook:?}"),
        Err(HookError(why)) => why,
    }
}

#[test]
fn a_malformed_or_unknown_hook_is_refused_with_a_reason() {
    assert!(refused("alarm", json!({})).contains("not a hook kind"));
    assert!(refused("time", json!({"at": "tomorrow"})).contains("RFC 3339"));
    assert!(refused("time", json!("2026-11-10T18:00:00Z")).contains("object"));
    assert!(refused("repeat", json!({"every": "fortnight", "at": "18:00"})).contains("every"));
    assert!(refused("repeat", json!({"every": "day", "at": "dusk"})).contains("time of day"));
    assert!(refused(
        "repeat",
        json!({"every": "week", "at": "09:00", "days": [0]})
    )
    .contains("1 (Monday)"));
    assert!(refused(
        "repeat",
        json!({"every": "month", "at": "09:00", "days": [32]})
    )
    .contains("1 to 31"));
    assert!(refused("place", json!({"place_id": "someone-elses"})).contains("no such place"));
    assert!(refused(
        "place",
        json!({"place_id": "p-home", "transition": "teleport"})
    )
    .contains("arrive or leave"));
    assert!(refused(
        "event",
        json!({"source_id": "cart:not-mine", "event": "price_drop"})
    )
    .contains("no such source"));
    assert!(refused(
        "event",
        json!({"source_id": "camera:camera-1", "event": "smile"})
    )
    .contains("person, motion"));
    assert!(refused(
        "event",
        json!({"source_id": "presence:office", "event": "arrive"})
    )
    .contains("presence:home"));
    assert!(refused("event", json!({"source_id": "lamp", "event": "on"})).contains("source id"));
    assert!(refused(
        "connector",
        json!({"connector_id": "mail:someone-else", "event": "new_mail"})
    )
    .contains("no such connector"));
    assert!(refused(
        "connector",
        json!({"connector_id": "webhook:wh1", "event": "new_mail"})
    )
    .contains("call"));
}

#[test]
fn a_household_device_not_yet_listed_is_still_accepted_by_its_id() {
    assert_eq!(
        label_of(
            "event",
            json!({"source_id": "camera:back-yard", "event": "motion"})
        ),
        "When Back yard sees movement"
    );
}

#[test]
fn a_weekly_or_monthly_repeat_with_no_days_falls_on_todays() {
    let now = utc("2026-11-09T09:00:00Z");
    let hook = resolve_hook(
        "repeat",
        &json!({"every": "week", "at": "18:00"}),
        &catalog(),
        now,
        NAIROBI,
    )
    .unwrap();
    assert_eq!(hook.spec.to_json()["days"], json!([1]));
    assert_eq!(hook.label, "Every Monday at 18:00");
    assert_eq!(hook.next_fire_at, Some(utc("2026-11-09T15:00:00Z")));

    let hook = resolve_hook(
        "repeat",
        &json!({"every": "month", "at": "08:00"}),
        &catalog(),
        now,
        NAIROBI,
    )
    .unwrap();
    assert_eq!(hook.spec.to_json()["days"], json!([9]));
    // Today's 08:00 Nairobi has passed, so next month's 9th.
    assert_eq!(hook.next_fire_at, Some(utc("2026-12-09T05:00:00Z")));
}

#[test]
fn a_time_hook_fires_at_its_time_and_an_event_hook_waits() {
    let at = utc("2026-11-10T15:00:00Z");
    let hook = resolve_hook(
        "time",
        &json!({"at": "2026-11-10T18:00:00+03:00"}),
        &catalog(),
        utc("2026-11-09T09:00:00Z"),
        NAIROBI,
    )
    .unwrap();
    assert_eq!(hook.next_fire_at, Some(at));
    assert_eq!(hook.spec.to_json(), json!({"at": "2026-11-10T15:00:00Z"}));
    assert_eq!(hook.spec.next_after_firing(at, NAIROBI), None);

    let hook = resolve_hook(
        "place",
        &json!({"place_id": "p-home"}),
        &catalog(),
        utc("2026-11-09T09:00:00Z"),
        NAIROBI,
    )
    .unwrap();
    assert_eq!(hook.next_fire_at, None);
    assert_eq!(
        hook.spec.to_json(),
        json!({"place_id": "p-home", "transition": "arrive"})
    );
}

// ── Matching ────────────────────────────────────────────────────────────────

fn event(profile: Option<&str>, kind: &str, id: &str, what: &str, payload: Value) -> HookEvent {
    HookEvent::new(
        profile,
        kind,
        id,
        what,
        payload,
        utc("2026-11-09T09:00:00Z"),
    )
}

#[test]
fn a_place_hook_matches_only_its_owners_arrival_there() {
    let hook = HookSpec::Place {
        place_id: "p-home".into(),
        transition: Transition::Arrive,
    };
    assert!(hook.matches(
        &event(Some("jerry"), "place", "p-home", "arrive", json!({})),
        "jerry"
    ));
    assert!(!hook.matches(
        &event(Some("emmanuel"), "place", "p-home", "arrive", json!({})),
        "jerry"
    ));
    assert!(!hook.matches(
        &event(Some("jerry"), "place", "p-home", "leave", json!({})),
        "jerry"
    ));
    assert!(!hook.matches(
        &event(Some("jerry"), "place", "p-gym", "arrive", json!({})),
        "jerry"
    ));
}

#[test]
fn a_household_event_matches_any_members_hook_and_a_named_one_only_theirs() {
    let hook = HookSpec::Event {
        source_id: "presence:home".into(),
        event: "arrive".into(),
    };
    assert!(hook.matches(
        &event(None, "presence", "home", "arrive", json!({})),
        "jerry"
    ));
    assert!(hook.matches(
        &event(Some("jerry"), "presence", "home", "arrive", json!({})),
        "jerry"
    ));
    assert!(!hook.matches(
        &event(Some("emmanuel"), "presence", "home", "arrive", json!({})),
        "jerry"
    ));
    let camera = HookSpec::Event {
        source_id: "camera:camera-1".into(),
        event: "person".into(),
    };
    assert!(camera.matches(
        &event(None, "camera", "camera-1", "PERSON", json!({})),
        "jerry"
    ));
    assert!(!camera.matches(
        &event(None, "camera", "camera-2", "person", json!({})),
        "jerry"
    ));
    let cart = HookSpec::Event {
        source_id: "cart:c1".into(),
        event: "price_drop".into(),
    };
    assert!(cart.matches(
        &event(Some("jerry"), "cart", "c1", "price_drop", json!({})),
        "jerry"
    ));
    assert!(!cart.matches(
        &event(Some("jerry"), "cart", "c1", "price_change", json!({})),
        "jerry"
    ));
}

#[test]
fn a_mail_hook_with_a_sender_matches_only_mail_from_them() {
    let hook = HookSpec::Connector {
        connector_id: "mail:mail:gmail:jerry".into(),
        event: "new_mail".into(),
        filter: Some("emmanuel".into()),
    };
    let mail = |from: &str| {
        event(
            Some("jerry"),
            "connector",
            "mail:mail:gmail:jerry",
            "new_mail",
            json!({"from": from, "subject": "Fielder"}),
        )
    };
    assert!(hook.matches(&mail("Emmanuel Oyugi <emmanuel@example.com>"), "jerry"));
    assert!(!hook.matches(&mail("KPLC <bills@kplc.co.ke>"), "jerry"));
}

#[test]
fn a_mail_id_matches_with_or_without_the_repeated_prefix() {
    let hook = HookSpec::Connector {
        connector_id: "mail:mail:gmail:jerry".into(),
        event: "new_mail".into(),
        filter: None,
    };
    for raised in ["mail:mail:gmail:jerry", "mail:gmail:jerry"] {
        assert!(
            hook.matches(
                &event(Some("jerry"), "connector", raised, "new_mail", json!({})),
                "jerry"
            ),
            "{raised}"
        );
    }
    assert!(!hook.matches(
        &event(
            Some("jerry"),
            "connector",
            "mail:outlook:jerry",
            "new_mail",
            json!({})
        ),
        "jerry"
    ));
}

#[test]
fn a_webhook_filter_matches_anywhere_in_the_body() {
    let hook = HookSpec::Connector {
        connector_id: "webhook:wh1".into(),
        event: "call".into(),
        filter: Some("ring".into()),
    };
    assert!(hook.matches(
        &event(
            Some("jerry"),
            "connector",
            "webhook:wh1",
            "call",
            json!({"button": {"state": "RING"}})
        ),
        "jerry"
    ));
    assert!(!hook.matches(
        &event(
            Some("jerry"),
            "connector",
            "webhook:wh1",
            "call",
            json!({"button": "idle"})
        ),
        "jerry"
    ));
}

#[test]
fn a_clock_hook_never_matches_an_event() {
    assert!(!repeat(Every::Day, "18:00", &[]).matches(
        &event(None, "presence", "home", "arrive", json!({})),
        "jerry"
    ));
}

// ── Asking, snoozing and stopping ───────────────────────────────────────────

fn todo(kind: TodoKind, ask: bool, asked: u32) -> Todo {
    let now = utc("2026-11-09T09:00:00Z");
    Todo {
        id: "t1".into(),
        owner: "jerry".into(),
        kind,
        text: "call the garage about the Fielder".into(),
        notes: None,
        state: TodoState::Open,
        due_at: None,
        ask,
        source: TodoSource::You,
        source_ref: None,
        last_asked_at: None,
        asked,
        snoozed_until: None,
        created_at: now,
        updated_at: now,
        done_at: None,
        hooks: Vec::new(),
    }
}

#[test]
fn the_same_request_made_again_is_a_repeat_until_it_has_fired() {
    let at = |raw: &str| Some(utc(raw));
    let mut kept = todo(TodoKind::Reminder, false, 0);
    kept.due_at = at("2026-11-09T14:00:00Z");
    let mut again = Todo {
        id: "t2".into(),
        text: " Call the garage about the Fielder ".into(),
        created_at: utc("2026-11-09T09:20:00Z"),
        ..kept.clone()
    };
    assert!(kept.is_repeated_by(&again), "a turn re-run after a stop");

    again.due_at = at("2026-11-09T14:11:00Z");
    assert!(
        !kept.is_repeated_by(&again),
        "another time is another reminder"
    );
    again.due_at = kept.due_at;
    again.created_at = utc("2026-11-09T11:00:00Z");
    assert!(!kept.is_repeated_by(&again), "two hours on, they mean it");
    again.created_at = utc("2026-11-09T09:20:00Z");
    again.owner = "liz".into();
    assert!(!kept.is_repeated_by(&again), "somebody else's");
    again.owner = kept.owner.clone();

    kept.state = TodoState::Done;
    assert!(
        !kept.is_repeated_by(&again),
        "a done one is asked for afresh"
    );
    kept.state = TodoState::Open;
    kept.hooks = vec![Hook::new(
        HookSpec::Time {
            at: utc("2026-11-09T14:00:00Z"),
        },
        "today at 17:00".into(),
        utc("2026-11-09T09:00:00Z"),
        chrono_tz::Africa::Nairobi,
    )];
    again.hooks = kept.hooks.clone();
    assert!(kept.is_repeated_by(&again));
    kept.hooks[0].last_fired_at = at("2026-11-09T14:00:00Z");
    assert!(!kept.is_repeated_by(&again), "it already reminded them");
}

#[test]
fn goose_asks_about_a_todo_three_times_and_only_reminds_about_a_reminder() {
    assert_eq!(
        todo(TodoKind::Todo, true, 0).push_kind(),
        Some(PushKind::CheckIn)
    );
    assert_eq!(
        todo(TodoKind::Todo, true, 2).push_kind(),
        Some(PushKind::CheckIn)
    );
    assert_eq!(todo(TodoKind::Todo, true, 3).push_kind(), None);
    assert_eq!(
        todo(TodoKind::Todo, false, 5).push_kind(),
        Some(PushKind::Reminder)
    );
    assert_eq!(
        todo(TodoKind::Reminder, true, 5).push_kind(),
        Some(PushKind::Reminder)
    );
    for state in [TodoState::Done, TodoState::Dropped] {
        let mut closed = todo(TodoKind::Reminder, false, 0);
        closed.state = state;
        assert_eq!(closed.push_kind(), None, "{state:?}");
    }
}

#[test]
fn not_yet_asks_again_at_the_snooze() {
    let now = utc("2026-11-09T09:00:00Z");
    let mut t = todo(TodoKind::Todo, true, 1);
    t.answer(Answer::NotYet, Some(30), now, NAIROBI).unwrap();
    assert_eq!(t.snoozed_until, Some(utc("2026-11-09T09:30:00Z")));
    assert_eq!(t.state, TodoState::Open);
}

#[test]
fn not_yet_without_a_snooze_asks_the_next_morning_at_nine_member_time() {
    // 08:55 in Nairobi: not five minutes later, but tomorrow at 09:00.
    let mut t = todo(TodoKind::Todo, true, 1);
    t.answer(Answer::NotYet, None, utc("2026-11-09T05:55:00Z"), NAIROBI)
        .unwrap();
    assert_eq!(t.snoozed_until, Some(utc("2026-11-10T06:00:00Z")));
    // 02:00 in Nairobi: this morning is seven hours off.
    assert_eq!(
        follow_up_at(utc("2026-11-08T23:00:00Z"), NAIROBI, None),
        utc("2026-11-09T06:00:00Z")
    );
    // 21:00 in New York, in EST.
    assert_eq!(
        follow_up_at(utc("2026-11-10T02:00:00Z"), NEW_YORK, None),
        utc("2026-11-10T14:00:00Z")
    );
}

#[test]
fn after_the_third_ask_not_yet_stops_asking_and_leaves_the_todo_open() {
    let mut t = todo(TodoKind::Todo, true, MAX_ASKS);
    t.answer(
        Answer::NotYet,
        Some(30),
        utc("2026-11-09T09:00:00Z"),
        NAIROBI,
    )
    .unwrap();
    assert_eq!(t.snoozed_until, None);
    assert_eq!(t.state, TodoState::Open);
}

#[test]
fn done_and_drop_close_the_todo_and_cancel_any_follow_up() {
    let now = utc("2026-11-09T09:00:00Z");
    let mut t = todo(TodoKind::Todo, true, 1);
    t.snoozed_until = Some(utc("2026-11-10T06:00:00Z"));
    t.answer(Answer::Done, None, now, NAIROBI).unwrap();
    assert_eq!(
        (t.state, t.done_at, t.snoozed_until),
        (TodoState::Done, Some(now), None)
    );
    let mut t = todo(TodoKind::Todo, true, 1);
    t.answer(Answer::Drop, None, now, NAIROBI).unwrap();
    assert_eq!((t.state, t.done_at), (TodoState::Dropped, None));
    assert_eq!(t.push_kind(), None, "never asked again once dropped");
}

#[test]
fn not_yet_on_a_closed_todo_or_with_a_bad_snooze_is_refused() {
    let now = utc("2026-11-09T09:00:00Z");
    let mut t = todo(TodoKind::Todo, true, 0);
    t.state = TodoState::Done;
    assert_eq!(
        t.answer(Answer::NotYet, None, now, NAIROBI),
        Err(AnswerRefused::Closed(TodoState::Done))
    );
    let mut t = todo(TodoKind::Todo, true, 0);
    assert_eq!(
        t.answer(Answer::NotYet, Some(0), now, NAIROBI),
        Err(AnswerRefused::Snooze)
    );
    assert_eq!(
        t.answer(Answer::NotYet, Some(MAX_SNOOZE_MINUTES + 1), now, NAIROBI),
        Err(AnswerRefused::Snooze)
    );
}

#[test]
fn a_due_date_stands_in_for_a_time_hook_once_per_due_time() {
    let now = utc("2026-11-09T09:00:00Z");
    let mut t = todo(TodoKind::Todo, true, 0);
    t.due_at = Some(utc("2026-11-09T08:00:00Z"));
    assert!(t.due_without_hooks(now));
    t.last_asked_at = Some(utc("2026-11-09T08:00:30Z"));
    assert!(
        !t.due_without_hooks(now),
        "already pushed for this due time"
    );
    t.due_at = Some(utc("2026-11-09T08:30:00Z"));
    assert!(t.due_without_hooks(now), "moved later, so due again");
    t.due_at = Some(utc("2026-11-09T10:00:00Z"));
    assert!(!t.due_without_hooks(now), "not yet due");
}

// ── Goose's question ────────────────────────────────────────────────────────

#[test]
fn the_question_turns_the_members_words_to_face_them() {
    assert_eq!(
        question_for("call the garage about the Fielder"),
        "Did you call the garage about the Fielder?"
    );
    assert_eq!(question_for("Call my mum."), "Did you call your mum?");
    assert_eq!(
        question_for("remember to pay the KPLC bill"),
        "Did you pay the KPLC bill?"
    );
    assert_eq!(
        question_for("Email Emmanuel the MCP Dev Summit slides"),
        "Did you email Emmanuel the MCP Dev Summit slides?"
    );
    assert_eq!(
        question_for("Emmanuel's birthday present"),
        "Did you get to this: Emmanuel's birthday present?"
    );
}

// ── The bus ─────────────────────────────────────────────────────────────────

#[test]
fn the_bus_raises_presence_cameras_devices_and_boolean_sensors() {
    let now = utc("2026-11-09T09:00:00Z");
    let raised =
        |e: BusEvent| hook_event_from_bus(&e, now).map(|h| (h.profile_id.clone(), h.key()));

    let presence = BusEvent::Presence(ProfilePresence {
        profile_id: "jerry".into(),
        transition: PresenceTransition::Arrived,
        source: IdentificationSource::PairedDevice,
        confidence: None,
        session_id: "s".into(),
        at: now,
    });
    assert_eq!(
        raised(presence),
        Some((
            Some("jerry".into()),
            EventKey::new("presence", "home", "arrive")
        ))
    );

    let camera = |kind: &str| {
        BusEvent::Camera(CameraEvent {
            id: None,
            camera_id: "camera-1".into(),
            event_type: kind.into(),
            confidence: Some(0.9),
            snapshot_path: None,
            metadata: None,
            acknowledged: false,
            created_at: now,
        })
    };
    assert_eq!(
        raised(camera("Person")),
        Some((None, EventKey::new("camera", "camera-1", "person")))
    );
    assert_eq!(raised(camera("vehicle")), None);

    let device = |key: &str, value: DeviceStateValue| {
        BusEvent::Device(DeviceStateChanged {
            device_id: DeviceId::new("matter-3"),
            key: key.into(),
            value,
            changed_at: now,
        })
    };
    assert_eq!(
        raised(device("power", DeviceStateValue::Bool(false))),
        Some((None, EventKey::new("device", "matter-3", "off")))
    );
    assert_eq!(
        raised(device("brightness", DeviceStateValue::Int(40))),
        None
    );

    let sensor = |kind: &str, value: f64, unit: &str| {
        BusEvent::Sensor(SensorReading {
            device_id: "hall-pir".into(),
            sensor_type: kind.into(),
            value,
            unit: unit.into(),
            recorded_at: now,
        })
    };
    assert_eq!(
        raised(sensor("motion", 1.0, "bool")),
        Some((None, EventKey::new("sensor", "hall-pir", "motion")))
    );
    assert_eq!(raised(sensor("motion", 0.0, "bool")), None);
    assert_eq!(raised(sensor("temperature", 24.0, "C")), None);
}
