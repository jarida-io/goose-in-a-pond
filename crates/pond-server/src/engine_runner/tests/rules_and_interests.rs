use pond_core::user_data::domain::dont_list::Reason;
use pond_core::user_data::domain::interest::{
    InterestInput, InterestPatch, ObservedInterest, Origin, Signal,
};
use pond_core::user_data::domain::question::{
    QuestionPurpose, INTEREST_DROP, INTEREST_FOLLOW, INTEREST_HEADER,
};
use pond_core::user_data::ports::interests::InterestRepository;
use pond_core::user_data::services::dont_list::RULE_SYSTEM;

use super::*;

const WORDS: &str = "stop sending me recipes that are only about a colour";

impl Pond {
    /// A "never this" with the member's words, and the rule job the reject route queues for it.
    async fn turned_down(&self, now: DateTime<Utc>) -> DontRule {
        let cooking = Anchor::new(
            &self.member,
            AnchorKind::Interest,
            "Cooking",
            AnchorOrigin::Chat,
            now,
        )
        .unwrap();
        assert!(self.anchors.add(&cooking).await.unwrap());
        let mut made = Deliverable::new(
            &cooking.id,
            DeliverableKind::Digest,
            "digest",
            "Recipes for a blue week",
            now,
        )
        .unwrap();
        made.hook = "Seven dinners, each one blue.".to_string();
        assert!(self.anchors.add_deliverable(&made).await.unwrap());
        let mut rule = DontRule::new(
            &self.member,
            WORDS,
            Strength::Never,
            RuleOrigin::Reject,
            now,
        )
        .unwrap();
        rule.reasons = vec![Reason::NotRelevant];
        rule.said = Some(WORDS.to_string());
        rule.origin_ref = Some(made.id.clone());
        let rule = self.dont.add(&rule).await.unwrap();
        let job = EngineJob::new(
            JobKind::Rule,
            JobClass::Upkeep,
            Some(&self.member),
            &rule.id,
            now,
        );
        self.jobs.add(&job).await.unwrap();
        rule
    }

    async fn run_rule_job(&self, now: DateTime<Utc>) -> EngineJob {
        tick(&self.deps, now).await;
        let job = self.job_of(JobKind::Rule).await;
        self.drive(&job.task_id.clone().expect("its task opened"), now)
            .await;
        self.job(&job.id).await
    }

    fn systems_asked(&self) -> Vec<String> {
        self.model
            .asked()
            .into_iter()
            .map(|(system, _)| system)
            .collect()
    }
}

#[tokio::test]
async fn a_rule_the_check_passes_takes_the_place_of_the_members_words() {
    let pond = pond().await;
    let now = Utc::now();
    let held = pond.turned_down(now).await;
    pond.model
        .say(r#"{"rule": "recipes built on a colour preference"}"#);
    pond.model.say(PASS);

    let job = pond.run_rule_job(now).await;

    assert_eq!(pond.systems_asked(), [RULE_SYSTEM, CRITIC_SYSTEM]);
    let asked = pond.model.asked();
    assert!(
        asked[0].1.contains("Turned down: Recipes for a blue week"),
        "{}",
        asked[0].1
    );
    assert!(asked[0].1.contains("Kind: a digest"));
    assert!(asked[0]
        .1
        .contains("Opening line: Seven dinners, each one blue."));
    assert!(asked[0].1.contains(&format!("Their words: \"{WORDS}\"")));
    assert!(
        !asked[1].1.contains(&format!("Never: {WORDS}")),
        "the rule being rewritten is not on the list it is checked against"
    );

    let rules = pond.dont.list(&pond.member, None).await.unwrap();
    assert_eq!(rules.len(), 1, "{rules:?}");
    let rule = &rules[0];
    assert_eq!(rule.rule, "recipes built on a colour preference");
    assert_eq!(rule.strength, Strength::Never);
    assert_eq!(rule.said.as_deref(), Some(WORDS), "their words are kept");
    assert_eq!(rule.reasons, [Reason::NotRelevant]);
    assert_eq!(rule.origin_ref, held.origin_ref);
    assert!(pond
        .dont
        .get(&pond.member, &held.id)
        .await
        .unwrap()
        .is_none());

    assert_eq!(job.state, JobState::Passed);
    assert_eq!(
        job.outcome.as_deref(),
        Some("Passed its first check. The rule now reads: recipes built on a colour preference.")
    );
}

#[tokio::test]
async fn a_rule_the_check_keeps_failing_leaves_the_members_words_as_the_rule() {
    let pond = pond().await;
    let now = Utc::now();
    let held = pond.turned_down(now).await;
    pond.model
        .say(r#"{"rule": "recipes built on a colour preference"}"#);
    for reply in [
        FAIL,
        r#"{"rule": "recipes about colours"}"#,
        FAIL,
        r#"{"rule": "recipes about colours"}"#,
        FAIL,
    ] {
        pond.model.say(reply);
    }

    let job = pond.run_rule_job(now).await;

    assert_eq!(
        pond.systems_asked(),
        [
            RULE_SYSTEM,
            CRITIC_SYSTEM,
            REVISE_SYSTEM,
            CRITIC_SYSTEM,
            REVISE_SYSTEM,
            CRITIC_SYSTEM
        ]
    );
    let rules = pond.dont.list(&pond.member, None).await.unwrap();
    assert_eq!(rules, [held], "the rule is as the member wrote it");
    assert_eq!(job.state, JobState::Dropped);
    assert_eq!(job.rounds, 3);
    assert!(
        job.outcome
            .as_deref()
            .unwrap()
            .ends_with("Their own words stay as the rule."),
        "{:?}",
        job.outcome
    );
}

#[tokio::test]
async fn a_confirmed_interest_gets_no_anchor_and_an_avoided_ones_anchor_stops() {
    let pond = pond().await;
    let now = Utc::now();
    let followed = pond
        .interests
        .confirm(
            &pond.member,
            &InterestInput {
                topic: "Formula 1".to_string(),
                ..InterestInput::default()
            },
            Origin::You,
        )
        .await
        .unwrap();

    let first = tick(&pond.deps, now).await;
    assert_eq!(first.interests_followed, 0);
    assert!(pond
        .anchors
        .list(&pond.member, &[AnchorState::Active])
        .await
        .unwrap()
        .iter()
        .all(|anchor| anchor.kind != AnchorKind::Interest));

    let mut made = Anchor::new(
        &pond.member,
        AnchorKind::Interest,
        "Formula 1",
        AnchorOrigin::Interest,
        now,
    )
    .unwrap();
    made.origin_ref = Some(followed.id.clone());
    assert!(pond.anchors.add(&made).await.unwrap());
    let anchor = &made;
    let mut working = EngineJob::new(
        JobKind::Questions,
        JobClass::Open,
        Some(&pond.member),
        &anchor.title,
        now,
    );
    working.anchor_id = Some(anchor.id.clone());
    pond.jobs.add(&working).await.unwrap();

    let avoid = InterestPatch {
        avoid: Some(true),
        ..InterestPatch::default()
    };
    pond.interests
        .patch(&pond.member, &followed.id, &avoid)
        .await
        .unwrap();
    let stopped = tick(&pond.deps, now + Duration::minutes(2)).await;
    assert_eq!(stopped.interests_followed, 1);
    let anchor = pond.anchors.find(&anchor.id).await.unwrap().unwrap();
    assert_eq!(anchor.state, AnchorState::Stopped);
    let called_off = pond.job(&working.id).await;
    assert_eq!(called_off.state, JobState::Cancelled);
    assert_eq!(
        called_off.outcome.as_deref(),
        Some("Stopped: no longer followed.")
    );
    assert!(pond
        .jobs
        .open()
        .await
        .unwrap()
        .iter()
        .all(|job| job.anchor_id.as_deref() != Some(anchor.id.as_str())));
}

#[tokio::test]
async fn an_interest_ready_to_offer_is_asked_about_once() {
    let pond = pond().await;
    let now = Utc::now();
    let seen = ObservedInterest {
        topic: "Safari Rally".to_string(),
        category: "sport".to_string(),
        facets: Vec::new(),
        signal: Signal::Asked,
    };
    pond.interests
        .observe(&pond.member, "s1", &seen)
        .await
        .unwrap();
    let guess = pond.interests.list(&pond.member).await.unwrap()[0].clone();
    for (session, days_ago) in [("s2", 1), ("s3", 2)] {
        pond.interests
            .sight_at(&guess.id, session, now - Duration::days(days_ago));
    }

    let report = tick(&pond.deps, now).await;
    assert_eq!(report.interests_asked, 1);
    let open = pond.questions.open(&pond.member).await.unwrap();
    assert_eq!(open.len(), 1, "{open:?}");
    let asked = &open[0];
    assert_eq!(asked.purpose, QuestionPurpose::Interest);
    assert_eq!(asked.subject_ref.as_deref(), Some(guess.id.as_str()));
    assert_eq!(asked.labels(), [INTEREST_FOLLOW, INTEREST_DROP]);
    assert_eq!(asked.header, INTEREST_HEADER);
    assert!(asked.options.iter().all(|o| !o.description.is_empty()));
    assert_eq!(
        asked.text,
        "You keep coming back to Safari Rally. Should I follow it for you?"
    );
    assert_eq!(asked.asked_at, Some(now));

    assert!(pond.notifier.sent().is_empty(), "questions are not pushed");

    for later in [Duration::minutes(1), Duration::days(2)] {
        let report = tick(&pond.deps, now + later).await;
        assert_eq!(report.interests_asked, 0);
    }
    assert_eq!(pond.questions.open(&pond.member).await.unwrap().len(), 1);
}
