use super::*;
use crate::user_data::domain::anchor::AnchorOrigin;
use crate::user_data::domain::fact_sheet::{Confidence, FactSource, FactSourceKind};
use crate::user_data::domain::question::Answer;

const NAIROBI: &str = "Africa/Nairobi";

const ENGINE_IDS: [&str; 6] = [
    "anchor_soon",
    "questions_waiting",
    "plan_to_confirm",
    "changed_since",
    "ready_deliverable",
    "fact_hint",
];

fn at(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .with_timezone(&Utc)
}

/// Tuesday 17 November 2026, 10:00 in Nairobi.
fn now() -> DateTime<Utc> {
    at("2026-11-17T07:00:00Z")
}

fn anchor(owner: &str, kind: AnchorKind, title: &str, starts: Option<&str>) -> Anchor {
    let mut anchor = Anchor::new(owner, kind, title, AnchorOrigin::You, now()).unwrap();
    anchor.starts_at = starts.map(at);
    anchor
}

/// A plan Goose noticed in a chat, waiting for the member's yes.
fn candidate(owner: &str, kind: AnchorKind, title: &str, starts: Option<&str>) -> Anchor {
    let mut anchor = Anchor::new(owner, kind, title, AnchorOrigin::Chat, now()).unwrap();
    assert_eq!(anchor.state, AnchorState::Candidate);
    anchor.starts_at = starts.map(at);
    anchor.why = "You mentioned it on 12 November".to_string();
    anchor
}

/// A first version Goose finished for `anchor`, not yet opened.
fn work(anchor: &Anchor, kind: DeliverableKind, slot: &str, title: &str) -> UnseenWork {
    let mut deliverable = Deliverable::new(&anchor.id, kind, slot, title, now()).unwrap();
    deliverable.state = DeliverableState::Ready;
    deliverable.version = 1;
    deliverable.hook = "Four days, the SGR both ways, KSh 62,000 in all.".to_string();
    deliverable.body = "## Day 1\nThe 08:00 SGR from Nairobi.".to_string();
    UnseenWork {
        anchor: anchor.clone(),
        deliverable,
    }
}

/// `work` after a refresh that changed something.
fn refreshed(mut work: UnseenWork) -> UnseenWork {
    work.deliverable.version = 2;
    work.deliverable.changed =
        "- The SGR fare rose to KSh 3,000.\n- A new stay in Galu is on the list.".to_string();
    work
}

fn question(owner: &str, about: &Anchor, text: &str) -> Question {
    let mut asked = Question::new(
        owner,
        "b1",
        QuestionPurpose::Anchor,
        text,
        &["Just me".to_string(), "Two of us".to_string()],
        false,
        now(),
    )
    .unwrap();
    asked.anchor_id = Some(about.id.clone());
    asked
}

fn fact(category: FactCategory, text: &str, tools: &[&str], score: f64) -> FactLine {
    let mut line = FactLine::new(
        "jerry",
        category,
        text,
        Confidence::Likely,
        FactSource::new(FactSourceKind::Memory, Some("m1")),
        now(),
    )
    .unwrap();
    line.tools = tools.iter().map(|t| t.to_string()).collect();
    line.score = score;
    line
}

/// A plan Goose finished for a trip, not yet opened.
fn diani_plan() -> UnseenWork {
    let trip = anchor("jerry", AnchorKind::Trip, "Diani trip", None);
    work(&trip, DeliverableKind::Plan, "plan", "Plan for Diani trip")
}

/// A pond with everything connected, so a test can take things away.
fn full() -> SuggestionSnapshot {
    SuggestionSnapshot {
        audience: Audience::Personal,
        groups: GroupsKnown::These(
            [
                GROUP_CONTEXT,
                GROUP_SCHEDULE,
                GROUP_MEMORY,
                GROUP_DEVICE,
                GROUP_WEATHER,
                GROUP_KNOWLEDGE,
            ]
            .iter()
            .map(|g| (*g).to_string())
            .collect(),
        ),
        muted: BTreeSet::new(),
        calendar_events_today: Some(3),
        mail_items_this_week: Some(237),
        schedules_before_midnight: 2,
        next_schedule_label: Some("Porch lights".to_string()),
        active_memories: 379,
        routine_memories: 4,
        devices_registered: 19,
        weather_ready: true,
        place: Some("Nairobi".to_string()),
        member: Some("jerry".to_string()),
        now: now(),
        timezone: NAIROBI.to_string(),
        unseen_work: vec![diani_plan()],
        ..Default::default()
    }
}

/// `full`, with the engine holding something for every suggestor that reads it: an event on
/// Thursday, a December trip with questions and a refreshed plan, a January trip Goose offered,
/// a shortlist for an undated purchase and a home line with a weather hint.
fn engine() -> SuggestionSnapshot {
    let mut s = full();
    let mut summit = anchor(
        "jerry",
        AnchorKind::Event,
        "MCP Dev Summit",
        Some("2026-11-19T06:00:00Z"),
    );
    summit.place = Some("KICC".to_string());
    let diani = anchor(
        "jerry",
        AnchorKind::Trip,
        "Diani trip",
        Some("2026-12-19T06:00:00Z"),
    );
    let lamu = candidate(
        "jerry",
        AnchorKind::Trip,
        "Lamu trip",
        Some("2027-01-10T06:00:00Z"),
    );
    let fielder = anchor("jerry", AnchorKind::Purchase, "Used Fielder", None);
    s.unseen_work = vec![
        refreshed(work(
            &diani,
            DeliverableKind::Plan,
            "plan",
            "Plan for Diani trip",
        )),
        work(
            &fielder,
            DeliverableKind::Shortlist,
            "shortlist",
            "Shortlist for Used Fielder",
        ),
    ];
    s.open_questions = [
        "How many of you are going?",
        "Where would you like to stay?",
        "Will you take the SGR?",
    ]
    .iter()
    .map(|text| question("jerry", &diani, text))
    .collect();
    s.anchors = vec![summit, diani, lamu, fielder];
    s.facts = vec![fact(
        FactCategory::Home,
        "Lives in Ndenderu with family",
        &[TOOL_WEATHER_FORECAST],
        40.0,
    )];
    s.home_place = Some("Ndenderu".to_string());
    s.tools_ready = tools_ready(true, true, true);
    s
}

fn ids(set: &SuggestionSet) -> Vec<String> {
    set.offered.iter().map(|s| s.id.clone()).collect()
}

fn silence(set: &SuggestionSet, id: &str) -> Option<String> {
    set.considered
        .iter()
        .find(|c| c.id == id)
        .and_then(|c| c.silent_because.clone())
}

/// One suggestor's card, whatever the cap would do with it.
fn card(s: &SuggestionSnapshot, id: &str) -> Suggestion {
    match run_one(id, s) {
        Ok(built) => built.suggestion,
        Err(reason) => panic!("{id} was silent: {reason}"),
    }
}

fn grounding_of(card: &Suggestion) -> (String, String) {
    match &card.context {
        Some(SuggestionContext::PersonalMemory {
            source_memory_id,
            fact,
        }) => (source_memory_id.clone(), fact.clone()),
        other => panic!("{} has no personal grounding: {other:?}", card.id),
    }
}

// ── The vacuity control ─────────────────────────────────────────────────
// Each suggestor gets a fires test and a silent test; either alone passes on a broken one.

#[test]
fn a_fully_connected_pond_offers_the_cap() {
    let set = suggest(&full());
    assert_eq!(set.offered.len(), MAX_SUGGESTIONS);
    // Ordered by how specific the answer is to today.
    assert_eq!(
        ids(&set),
        vec![
            "ready_deliverable",
            "calendar_today",
            "upcoming_schedule",
            "inbox_recent"
        ]
    );
}

#[test]
fn a_pond_with_no_engine_data_offers_what_it_did_before() {
    let mut s = full();
    s.unseen_work.clear();
    let set = suggest(&s);
    assert_eq!(
        ids(&set),
        vec![
            "calendar_today",
            "upcoming_schedule",
            "inbox_recent",
            "routine_recall"
        ]
    );
    for id in ENGINE_IDS {
        assert!(
            silence(&set, id).is_some(),
            "{id} spoke with no engine data"
        );
    }
}

#[test]
fn a_bare_pond_offers_nothing_and_says_why_for_every_suggestor() {
    let set = suggest(&SuggestionSnapshot {
        audience: Audience::Personal,
        groups: GroupsKnown::These(BTreeSet::new()),
        ..Default::default()
    });
    assert!(set.offered.is_empty());
    // The whole point of `considered`: silence is enumerated, not implied.
    assert_eq!(set.considered.len(), SUGGESTOR_IDS.len());
    for c in &set.considered {
        let reason = c
            .silent_because
            .as_ref()
            .unwrap_or_else(|| panic!("{} offered nothing and gave no reason", c.id));
        assert!(
            !reason.trim().is_empty(),
            "{} gave a blank reason, which tells a reader nothing",
            c.id
        );
    }
}

#[test]
fn calendar_tells_no_account_apart_from_an_empty_day() {
    let mut s = full();
    s.calendar_events_today = None;
    assert_eq!(
        silence(&suggest(&s), "calendar_today").as_deref(),
        Some("no calendar account is connected")
    );

    s.calendar_events_today = Some(0);
    assert_eq!(
        silence(&suggest(&s), "calendar_today").as_deref(),
        Some("a calendar is connected and today is empty")
    );
}

#[test]
fn calendar_fires_when_today_has_something_in_it() {
    let mut s = full();
    s.calendar_events_today = Some(1);
    let set = suggest(&s);
    let c = set
        .offered
        .iter()
        .find(|x| x.id == "calendar_today")
        .unwrap();
    assert_eq!(c.prompt, "What's on my calendar today?");
    assert_eq!(c.because, "1 event between now and midnight.");
}

#[test]
fn a_shared_pond_is_offered_nothing_personal() {
    let mut s = engine();
    s.audience = Audience::Shared;
    let set = suggest(&s);
    for personal in ENGINE_IDS.iter().chain(&[
        "calendar_today",
        "inbox_recent",
        "memory_recall",
        "routine_recall",
    ]) {
        assert!(
            !ids(&set).contains(&personal.to_string()),
            "{personal} was offered to an unidentified speaker on a multi-member pond"
        );
    }
    // Control: house facts still show, so the rule narrows rather than empties.
    assert!(ids(&set).contains(&"devices_online".to_string()));
    assert!(ids(&set).contains(&"upcoming_schedule".to_string()));
}

#[test]
fn a_suggestion_nothing_can_answer_is_not_offered() {
    let mut s = full();
    let GroupsKnown::These(ref mut set) = s.groups else {
        unreachable!("the fixture names its groups explicitly")
    };
    set.remove(GROUP_WEATHER);
    let set = suggest(&s);
    assert!(!ids(&set).contains(&"weather_today".to_string()));
    assert_eq!(
        silence(&set, "weather_today").as_deref(),
        Some("nothing on this pond could answer it: giap-weather is not installed")
    );
}

#[test]
fn an_unanswerable_report_is_permissive_and_a_real_one_is_not() {
    let mut s = full();

    s.groups = GroupsKnown::Unknown;
    assert!(
        !suggest(&s).offered.is_empty(),
        "an unconfirmed extension list silenced the whole column"
    );

    // A populated answer IS evidence, and is trusted completely.
    s.groups = GroupsKnown::These([GROUP_MEMORY.to_string()].into_iter().collect());
    let set = suggest(&s);
    assert_eq!(
        ids(&set),
        vec!["ready_deliverable", "routine_recall", "memory_recall"],
        "a real extension list was not trusted"
    );
}

#[test]
fn an_empty_report_is_unknown_rather_than_nothing() {
    assert_eq!(GroupsKnown::from_report(None), GroupsKnown::Unknown);
    assert_eq!(GroupsKnown::from_report(Some(vec![])), GroupsKnown::Unknown);
    assert_eq!(
        GroupsKnown::from_report(Some(vec![GROUP_MEMORY.to_string()])),
        GroupsKnown::These([GROUP_MEMORY.to_string()].into_iter().collect())
    );
    assert_eq!(GroupsKnown::default(), GroupsKnown::Unknown);
}

#[test]
fn muting_silences_one_suggestor_and_only_that_one() {
    let mut s = engine();
    for id in ["memory_recall", "questions_waiting"] {
        s.muted.insert(id.to_string());
    }
    let set = suggest(&s);
    for id in ["memory_recall", "questions_waiting"] {
        assert!(!ids(&set).contains(&id.to_string()));
        assert_eq!(
            silence(&set, id).as_deref(),
            Some("the household muted this suggestion")
        );
    }
    assert!(silence(&set, "devices_online").is_none());
    assert!(silence(&set, "changed_since").is_none());
}

#[test]
fn the_cap_shortens_the_offer_and_never_the_record() {
    for (snapshot, past_cap) in [
        (
            full(),
            vec![
                "routine_recall",
                "memory_recall",
                "devices_online",
                "weather_today",
            ],
        ),
        (
            engine(),
            vec![
                "ready_deliverable",
                "fact_hint",
                "calendar_today",
                "weather_today",
            ],
        ),
    ] {
        let set = suggest(&snapshot);
        assert_eq!(set.offered.len(), MAX_SUGGESTIONS);
        assert_eq!(
            set.considered.len(),
            SUGGESTOR_IDS.len(),
            "the cap hid a suggestor that fired, so nothing can tell that the pond had more to say"
        );
        // The ones past the cap fired; they are simply not shown.
        for id in past_cap {
            assert!(!ids(&set).contains(&id.to_string()), "{id} beat the cap");
            assert!(
                silence(&set, id).is_none(),
                "{id} was recorded as silent when it had actually produced something"
            );
        }
    }
}

#[test]
fn every_suggestor_id_is_reachable() {
    let s = engine();
    for id in SUGGESTOR_IDS {
        if let Err(reason) = run_one(id, &s) {
            assert!(
                !reason.starts_with("no suggestor is registered"),
                "{id} is listed but has no arm in run_one"
            );
        }
    }
}

#[test]
fn nothing_claims_a_device_is_on() {
    for snapshot in [full(), engine()] {
        for s in &suggest(&snapshot).offered {
            let text = format!("{} {}", s.prompt, s.because).to_lowercase();
            for claim in [
                " is on",
                " are on",
                "switched on",
                "turned on",
                " is off",
                " are off",
            ] {
                assert!(
                    !text.contains(claim),
                    "{} asserts device state the pond cannot read: {text}",
                    s.id
                );
            }
        }
    }
}

#[test]
fn nothing_claims_a_habit_the_pond_has_not_counted() {
    for snapshot in [full(), engine()] {
        for s in &suggest(&snapshot).offered {
            let text = format!("{} {}", s.prompt, s.because).to_lowercase();
            for claim in [
                "you usually",
                "you always",
                "you often",
                "every day",
                "as usual",
            ] {
                assert!(
                    !text.contains(claim),
                    "{} claims a pattern nothing counted: {text}",
                    s.id
                );
            }
        }
    }
}

#[test]
fn every_offered_suggestion_carries_a_number_or_a_name() {
    let s = engine();
    let mut cards: Vec<Suggestion> = suggest(&full()).offered;
    cards.extend(ENGINE_IDS.iter().map(|id| card(&s, id)));
    for s in &cards {
        assert!(
            s.because.chars().any(|c| c.is_ascii_digit())
                || ["Nairobi", "Diani", "Ndenderu", "Fielder"]
                    .iter()
                    .any(|name| s.because.contains(name)),
            "{}'s reason quotes nothing the pond measured: {}",
            s.id,
            s.because
        );
        assert!(!s.because.trim().is_empty(), "{} has a blank reason", s.id);
    }
}

#[test]
fn the_prompt_is_the_sentence_the_household_reads() {
    let s = engine();
    let mut cards: Vec<Suggestion> = suggest(&full()).offered;
    cards.extend(ENGINE_IDS.iter().map(|id| card(&s, id)));
    for s in &cards {
        assert!(!s.prompt.ends_with('.'), "{} reads as a statement", s.id);
        assert!(
            s.prompt.chars().next().is_some_and(char::is_uppercase),
            "{} does not start a sentence: {}",
            s.id,
            s.prompt
        );
        assert!(
            s.prompt.chars().count() <= MAX_PROMPT_CHARS,
            "{} would be truncated on the card",
            s.id
        );
    }
}

// ── Work Goose finished ─────────────────────────────────────────────────

#[test]
fn work_goose_finished_is_offered_in_the_members_words() {
    let set = suggest(&full());
    let ready = set
        .offered
        .iter()
        .find(|s| s.id == "ready_deliverable")
        .unwrap();
    assert_eq!(ready.prompt, "Tell me about my plan for Diani trip");
    assert_eq!(ready.because, "Ready: Plan for Diani trip.");
    let (source, fact) = grounding_of(ready);
    assert!(source.starts_with("deliverable:"));
    assert!(fact.contains("Plan for Diani trip"), "{fact}");
    assert!(fact.contains("KSh 62,000"), "{fact}");
    assert!(fact.contains("The 08:00 SGR"), "{fact}");
    assert!(!fact.contains("The rest is in My Goose"), "{fact}");
}

#[test]
fn nothing_unread_and_a_mute_each_silence_the_finished_work() {
    let mut s = full();
    s.unseen_work.clear();
    assert_eq!(
        silence(&suggest(&s), "ready_deliverable").as_deref(),
        Some("nothing Goose made is waiting to be read")
    );
    let mut s = full();
    s.muted.insert("ready_deliverable".to_string());
    let set = suggest(&s);
    assert!(!ids(&set).contains(&"ready_deliverable".to_string()));
    assert_eq!(
        set.offered.len(),
        MAX_SUGGESTIONS,
        "the next one takes its place"
    );
}

#[test]
fn a_long_deliverable_rides_the_card_cut_and_says_so() {
    let mut s = full();
    s.unseen_work[0].deliverable.body = "Day by day. ".repeat(400);
    let set = suggest(&s);
    let (_, fact) = grounding_of(&set.offered[0]);
    assert!(fact.chars().count() < WORK_CHARS + 300, "{}", fact.len());
    assert!(fact.ends_with("(The rest is in My Goose.)"), "{fact}");
}

#[test]
fn a_refresh_that_changed_something_is_asked_about_as_a_change() {
    let changed = card(&engine(), "changed_since");
    assert_eq!(changed.prompt, "What changed in my plan for Diani trip?");
    assert_eq!(
        changed.because,
        "Updated: Plan for Diani trip. The SGR fare rose to KSh 3,000."
    );
    assert_eq!(changed.answered_by, GROUP_MEMORY);
    let (source, fact) = grounding_of(&changed);
    assert!(source.starts_with("deliverable:"), "{source}");
    assert!(
        fact.contains("What changed:\n- The SGR fare rose"),
        "{fact}"
    );
    assert!(fact.contains("A new stay in Galu"), "{fact}");
    assert!(fact.contains("It now begins:\n## Day 1"), "{fact}");

    let mut s = engine();
    s.unseen_work[0].deliverable.seen_at = Some(now());
    assert_eq!(
        run_one("changed_since", &s).err().as_deref(),
        Some("nothing Goose refreshed has changed since it was last read")
    );
}

#[test]
fn ready_and_changed_never_offer_the_same_deliverable() {
    let trip = anchor("jerry", AnchorKind::Trip, "Diani trip", None);
    let first = work(&trip, DeliverableKind::Plan, "plan", "Plan for Diani trip");
    let change = refreshed(work(
        &trip,
        DeliverableKind::Checklist,
        "packing",
        "Packing list for Diani trip",
    ));
    let mut unsure = refreshed(work(
        &trip,
        DeliverableKind::Shortlist,
        "stays",
        "Places to stay for Diani trip",
    ));
    unsure.deliverable.state = DeliverableState::Unsure;
    unsure.deliverable.changed.clear();

    let mut s = full();
    s.unseen_work = vec![change.clone()];
    assert!(run_one("changed_since", &s).is_ok());
    assert_eq!(
        run_one("ready_deliverable", &s).err().as_deref(),
        Some("what waits to be read is a refresh, which changed_since offers")
    );

    s.unseen_work = vec![first.clone()];
    assert!(run_one("ready_deliverable", &s).is_ok());
    assert!(run_one("changed_since", &s).is_err());

    s.unseen_work = vec![unsure.clone()];
    assert!(
        run_one("ready_deliverable", &s).is_ok(),
        "an unsure version is shown as ready work"
    );
    assert!(run_one("changed_since", &s).is_err());

    s.unseen_work = vec![change.clone(), first.clone(), unsure];
    let set = suggest(&s);
    let offered: Vec<String> = set
        .offered
        .iter()
        .filter(|c| c.id == "ready_deliverable" || c.id == "changed_since")
        .map(|c| grounding_of(c).0)
        .collect();
    assert_eq!(offered.len(), 2, "{:?}", ids(&set));
    assert_ne!(offered[0], offered[1], "one deliverable offered twice");
    assert_eq!(offered[0], format!("deliverable:{}", change.deliverable.id));
    assert_eq!(offered[1], format!("deliverable:{}", first.deliverable.id));
}

// ── Anchors ─────────────────────────────────────────────────────────────

#[test]
fn something_starting_within_three_days_asks_what_the_member_needs() {
    let soon = card(&engine(), "anchor_soon");
    assert_eq!(
        soon.prompt,
        "What do I need for MCP Dev Summit on Thursday?"
    );
    assert_eq!(
        soon.because,
        "MCP Dev Summit starts on Thursday, 19 November."
    );
    assert_eq!(soon.answered_by, GROUP_MEMORY);
    let Some(SuggestionContext::Temporal {
        event_title,
        timing_summary,
    }) = &soon.context
    else {
        panic!("{:?}", soon.context)
    };
    assert_eq!(event_title, "MCP Dev Summit");
    assert!(
        timing_summary.contains("an event on Thursday 19 November 2026 at 09:00, at KICC"),
        "{timing_summary}"
    );
}

#[test]
fn the_day_is_named_on_the_household_clock() {
    let starting = |rfc3339: &str| {
        let mut s = engine();
        s.anchors[0].starts_at = Some(at(rfc3339));
        run_one("anchor_soon", &s).map(|b| b.suggestion.prompt)
    };
    // 08:00 today in Nairobi, before the snapshot was taken.
    assert_eq!(
        starting("2026-11-17T05:00:00Z").unwrap(),
        "What do I need for MCP Dev Summit today?"
    );
    // Still the 17th in UTC, already the 18th in Nairobi.
    assert_eq!(
        starting("2026-11-17T22:00:00Z").unwrap(),
        "What do I need for MCP Dev Summit tomorrow?"
    );
    assert_eq!(
        starting("2026-11-20T20:00:00Z").unwrap(),
        "What do I need for MCP Dev Summit on Friday?"
    );
    for out_of_range in ["2026-11-20T21:30:00Z", "2026-11-16T09:00:00Z"] {
        assert_eq!(
            starting(out_of_range).err().as_deref(),
            Some("nothing they are doing starts in the next three days"),
            "{out_of_range}"
        );
    }
}

#[test]
fn a_trip_soon_is_the_members_own() {
    let mut s = engine();
    s.anchors[0] = anchor(
        "jerry",
        AnchorKind::Trip,
        "Naivasha weekend",
        Some("2026-11-18T05:00:00Z"),
    );
    assert_eq!(
        card(&s, "anchor_soon").prompt,
        "What do I need for my Naivasha weekend tomorrow?"
    );
    s.anchors[0].state = AnchorState::Candidate;
    assert!(
        run_one("anchor_soon", &s).is_err(),
        "only a plan the member said yes to is coming up"
    );
}

#[test]
fn open_questions_are_offered_with_the_questions_in_their_grounding() {
    let waiting = card(&engine(), "questions_waiting");
    assert_eq!(waiting.prompt, "Answer 3 questions about my Diani trip");
    assert_eq!(
        waiting.because,
        "3 questions about Diani trip are waiting for an answer."
    );
    let (source, fact) = grounding_of(&waiting);
    assert!(source.starts_with("anchor:"), "{source}");
    assert!(fact.contains("Ask them here, one at a time"), "{fact}");
    assert!(
        fact.contains("1. How many of you are going? Options: Just me; Two of us."),
        "{fact}"
    );
    assert!(fact.contains("3. Will you take the SGR?"), "{fact}");

    let mut s = engine();
    s.open_questions[0]
        .answer(
            Answer {
                choices: vec!["Just me".to_string()],
                text: None,
            },
            now(),
        )
        .unwrap();
    s.open_questions[1].expires_at = Some(now() - chrono::Duration::hours(1));
    assert_eq!(
        card(&s, "questions_waiting").prompt,
        "Answer 1 question about my Diani trip"
    );
    s.open_questions[2].purpose = QuestionPurpose::Profile;
    assert_eq!(
        run_one("questions_waiting", &s).err().as_deref(),
        Some("Goose has no open questions about anything they are doing")
    );
}

#[test]
fn questions_for_an_anchor_that_is_over_are_not_offered() {
    let mut s = engine();
    s.anchors[1].state = AnchorState::Ended;
    assert!(run_one("questions_waiting", &s).is_err());
}

#[test]
fn a_plan_goose_noticed_is_offered_for_the_members_yes() {
    let offered = card(&engine(), "plan_to_confirm");
    assert_eq!(offered.prompt, "Plan my Lamu trip");
    assert_eq!(offered.because, "You mentioned it on 12 November.");
    assert_eq!(offered.answered_by, GROUP_SCHEDULE);
    let (source, fact) = grounding_of(&offered);
    assert!(source.starts_with("anchor:"), "{source}");
    assert!(
        fact.contains("call plan_for with kind \"trip\" and title \"Lamu trip\""),
        "{fact}"
    );
    assert!(fact.contains("Sunday 10 January 2027"), "{fact}");

    let mut s = engine();
    s.anchors[2].kind = AnchorKind::Event;
    s.anchors[2].why.clear();
    let event = card(&s, "plan_to_confirm");
    assert_eq!(event.prompt, "Help me prepare for Lamu trip");
    assert_eq!(
        event.because,
        "Goose noticed Lamu trip and waits for your yes."
    );

    let mut s = engine();
    s.anchors[2].confirm(now()).unwrap();
    assert_eq!(
        run_one("plan_to_confirm", &s).err().as_deref(),
        Some("Goose has offered to plan nothing they mentioned")
    );

    let mut s = engine();
    s.groups = GroupsKnown::These([GROUP_MEMORY.to_string()].into_iter().collect());
    assert_eq!(
        run_one("plan_to_confirm", &s).err().as_deref(),
        Some("nothing on this pond could answer it: giap-schedule is not installed"),
        "without plan_for the tap could not say yes"
    );
}

#[test]
fn dated_work_leads_nearest_first_then_the_fixed_order() {
    assert_eq!(
        ids(&suggest(&engine())),
        vec![
            "anchor_soon",
            "questions_waiting",
            "changed_since",
            "plan_to_confirm"
        ],
        "the event in two days, the trip in a month, the trip Goose offered in two"
    );

    let mut s = engine();
    for anchor in &mut s.anchors {
        anchor.starts_at = None;
    }
    for work in &mut s.unseen_work {
        work.anchor.starts_at = None;
    }
    assert_eq!(
        ids(&suggest(&s)),
        vec![
            "questions_waiting",
            "plan_to_confirm",
            "changed_since",
            "ready_deliverable"
        ]
    );

    s.unseen_work[1].anchor.starts_at = Some(at("2026-12-01T06:00:00Z"));
    assert_eq!(
        ids(&suggest(&s)),
        vec![
            "ready_deliverable",
            "questions_waiting",
            "plan_to_confirm",
            "changed_since"
        ],
        "a dated anchor goes before every undated one"
    );

    for id in [
        "questions_waiting",
        "plan_to_confirm",
        "changed_since",
        "ready_deliverable",
    ] {
        s.muted.insert(id.to_string());
    }
    assert_eq!(
        ids(&suggest(&s)),
        vec![
            "fact_hint",
            "calendar_today",
            "upcoming_schedule",
            "inbox_recent"
        ]
    );
}

#[test]
fn a_shared_anchor_gives_only_its_questions() {
    let mut s = full();
    let mut naivasha = anchor(
        "liz",
        AnchorKind::Trip,
        "Naivasha trip",
        Some("2026-11-18T06:00:00Z"),
    );
    naivasha.shared_with = vec!["jerry".to_string()];
    let mut offered = candidate("liz", AnchorKind::Trip, "Lamu trip", None);
    offered.shared_with = vec!["jerry".to_string()];
    s.unseen_work = vec![
        refreshed(work(
            &naivasha,
            DeliverableKind::Plan,
            "plan",
            "Plan for Naivasha trip",
        )),
        work(
            &naivasha,
            DeliverableKind::Shortlist,
            "stays",
            "Places to stay for Naivasha trip",
        ),
    ];
    s.open_questions = vec![
        question("liz", &naivasha, "Which weekend suits you?"),
        question("liz", &naivasha, "Shall we drive?"),
    ];
    s.anchors = vec![naivasha, offered];

    let set = suggest(&s);
    let engine_offered: Vec<String> = ids(&set)
        .into_iter()
        .filter(|id| ENGINE_IDS.contains(&id.as_str()))
        .collect();
    assert_eq!(engine_offered, vec!["questions_waiting"]);
    assert_eq!(
        card(&s, "questions_waiting").prompt,
        "Answer 2 questions about the Naivasha trip"
    );
    for id in [
        "anchor_soon",
        "plan_to_confirm",
        "changed_since",
        "ready_deliverable",
    ] {
        assert!(silence(&set, id).is_some(), "{id} spoke for Liz's trip");
    }

    s.anchors[0].shared_with.clear();
    assert!(
        run_one("questions_waiting", &s).is_err(),
        "an anchor not shared with them asks them nothing"
    );
}

// ── The fact sheet ──────────────────────────────────────────────────────

#[test]
fn a_home_line_with_a_weather_hint_asks_about_the_weekend_there() {
    let s = engine();
    let hint = card(&s, "fact_hint");
    assert_eq!(hint.prompt, "What's the weather in Ndenderu this weekend?");
    assert_eq!(
        hint.because,
        "On your fact sheet: Lives in Ndenderu with family."
    );
    assert_eq!(hint.answered_by, GROUP_WEATHER);
    let (source, fact) = grounding_of(&hint);
    assert_eq!(source, format!("fact:{}", s.facts[0].id));
    assert_eq!(fact, "Lives in Ndenderu with family");

    let mut s = engine();
    s.home_place = None;
    assert_eq!(
        card(&s, "fact_hint").prompt,
        "What's the weather at home this weekend?"
    );
}

#[test]
fn a_fact_hint_never_offers_a_tool_the_pond_lacks() {
    let mut s = engine();
    s.tools_ready = tools_ready(false, true, true);
    assert_eq!(
        run_one("fact_hint", &s).err().as_deref(),
        Some("no line of their fact sheet names a tool this pond can use now")
    );

    let mut s = engine();
    let GroupsKnown::These(ref mut groups) = s.groups else {
        unreachable!("the fixture names its groups explicitly")
    };
    groups.remove(GROUP_WEATHER);
    assert_eq!(
        run_one("fact_hint", &s).err().as_deref(),
        Some("nothing on this pond could answer it: giap-weather is not installed")
    );

    let mut s = engine();
    s.facts = vec![fact(
        FactCategory::Work,
        "Runs Jarida, a software studio",
        &[TOOL_SEARCH_WEB],
        30.0,
    )];
    s.tools_ready = tools_ready(true, true, false);
    assert!(
        run_one("fact_hint", &s).is_err(),
        "search without its key only answers with a setup message"
    );
    s.tools_ready = tools_ready(true, true, true);
    let work = card(&s, "fact_hint");
    assert_eq!(work.prompt, "What's new in my line of work this week?");
    assert_eq!(work.answered_by, GROUP_KNOWLEDGE);
}

#[test]
fn a_fact_hint_comes_only_from_the_members_active_lines_with_a_fixed_ask() {
    let mut s = engine();
    s.facts.clear();
    assert_eq!(
        run_one("fact_hint", &s).err().as_deref(),
        Some("Goose has no fact sheet for them yet")
    );

    let fits = || {
        fact(
            FactCategory::Home,
            "Lives in Ndenderu with family",
            &[TOOL_WEATHER_FORECAST],
            40.0,
        )
    };
    let no_ask = fact(
        FactCategory::Interest,
        "Follows Formula 1",
        &[TOOL_SEARCH_WEB],
        50.0,
    );
    let mut retired = fits();
    retired.state = FactState::Retired;
    let mut theirs = fits();
    theirs.profile_id = "liz".to_string();
    s.facts = vec![no_ask, retired, theirs];
    assert!(run_one("fact_hint", &s).is_err());
}

#[test]
fn the_fact_hint_takes_turns_among_the_best_lines_by_day() {
    let mut s = engine();
    s.facts = vec![
        fact(FactCategory::Food, "Eats no pork", &[TOOL_SEARCH_WEB], 20.0),
        fact(
            FactCategory::Home,
            "Lives in Ndenderu with family",
            &[TOOL_WEATHER_FORECAST],
            40.0,
        ),
        fact(
            FactCategory::Hunt,
            "Looking for a used Toyota Fielder",
            &[TOOL_WEB_WATCH],
            10.0,
        ),
        fact(
            FactCategory::Work,
            "Runs Jarida, a software studio",
            &[TOOL_SEARCH_WEB],
            30.0,
        ),
    ];
    let on = |day: i64| {
        let mut s = s.clone();
        s.now = now() + chrono::Duration::days(day);
        card(&s, "fact_hint").prompt
    };
    let week: Vec<String> = (0..6).map(&on).collect();
    assert_eq!(week[0], on(0), "one day offers one line");
    let three: BTreeSet<&String> = week[..3].iter().collect();
    assert_eq!(three.len(), 3, "three days, three lines: {week:?}");
    assert_eq!(week[..3], week[3..], "{week:?}");
    assert!(
        !week.contains(&"Watch the web for what I'm looking to buy".to_string()),
        "the fourth best line took a turn: {week:?}"
    );
}

#[test]
fn hinted_tools_follow_the_switches_and_the_search_key() {
    assert!(tools_ready(false, false, true).is_empty());
    let weather_only = tools_ready(true, false, true);
    assert!(
        weather_only.contains(TOOL_WEATHER_FORECAST) && weather_only.contains(TOOL_WEATHER_NOW)
    );
    assert!(!weather_only.contains(TOOL_SEARCH_WEB));
    let keyless = tools_ready(false, true, false);
    assert!(keyless.contains(TOOL_WEB_WATCH) && !keyless.contains(TOOL_SEARCH_WEB));
    assert!(tools_ready(false, true, true).contains(TOOL_SEARCH_WEB));
}

// ── The chip's words ────────────────────────────────────────────────────

#[test]
fn chip_text_is_short_plain_and_carries_no_ids() {
    let long = "Diani trip with Liz and the children over the long weekend by the SGR from \
                Nairobi and back again on the Monday";
    let mut long_titles = engine();
    for anchor in &mut long_titles.anchors {
        anchor.title = long.to_string();
    }
    for work in &mut long_titles.unseen_work {
        work.anchor.title = long.to_string();
    }
    long_titles.home_place = Some(long.to_string());
    let mut long_titles_undated = long_titles.clone();
    for anchor in &mut long_titles_undated.anchors {
        anchor.starts_at = None;
    }
    for s in [engine(), long_titles, long_titles_undated] {
        let mut ids_held: Vec<String> = s.anchors.iter().map(|a| a.id.clone()).collect();
        ids_held.extend(s.unseen_work.iter().map(|w| w.deliverable.id.clone()));
        ids_held.extend(s.facts.iter().map(|f| f.id.clone()));
        for id in SUGGESTOR_IDS {
            let Ok(built) = run_one(id, &s) else { continue };
            let prompt = &built.suggestion.prompt;
            assert!(
                prompt.chars().count() <= MAX_PROMPT_CHARS,
                "{id}: {} characters: {prompt}",
                prompt.chars().count()
            );
            assert!(
                !prompt.contains(['{', '}', '\n', '"']),
                "{id} is not plain: {prompt}"
            );
            assert!(
                !ids_held.iter().any(|held| prompt.contains(held.as_str())),
                "{id} shows an id: {prompt}"
            );
        }
    }
    let cut_card = card(
        &{
            let mut s = engine();
            s.anchors[0].title = long.to_string();
            s
        },
        "anchor_soon",
    );
    assert!(
        cut_card.prompt.ends_with("... on Thursday?"),
        "{}",
        cut_card.prompt
    );
}

#[test]
fn a_change_is_asked_about_in_the_words_of_its_slot() {
    let asked = |slot, kind| change_question(slot, kind, "Diani trip");
    assert_eq!(
        asked("stays", DeliverableKind::Shortlist),
        "What changed in the places to stay for Diani trip?"
    );
    assert_eq!(
        asked("costs", DeliverableKind::Plan),
        "What changed in what Diani trip will cost?"
    );
    assert_eq!(
        asked("out", DeliverableKind::Shortlist),
        "What changed in what's on near me this week?"
    );
    assert_eq!(
        asked("unheard_of", DeliverableKind::Digest),
        "What changed in my digest for Diani trip?"
    );
}

#[test]
fn the_snapshot_defaults_to_showing_less() {
    assert_eq!(Audience::default(), Audience::Shared);
    assert!(!Audience::default().may_see_personal());
}
