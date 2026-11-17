//! The details each kind of plan needs before its work fits the member (section 3 of
//! docs/architecture/structured-questions.md): code picks the open gaps, the maker only words
//! them, and a chain of batches runs until none is left or the member says enough.

use chrono::{DateTime, Utc};

use crate::user_data::domain::anchor::{Anchor, AnchorFact, AnchorKind, AnchorState};
use crate::user_data::domain::engine_job::{EngineJob, JobClass, JobKind};
use crate::user_data::domain::fact_sheet::{FactCategory, FactLine};
use crate::user_data::domain::interest::topic_key;
use crate::user_data::domain::question::{
    is_soft_limit_round, Question, QuestionState, KEEP_ASKING_GAP, QUESTIONS_PER_BATCH,
};
use crate::user_data::ports::engine_jobs::EngineJobRepository;

/// The plan fact an "enough" from the member leaves, which every later round reads as done.
pub const ENOUGH_LABEL: &str = "Questions";
pub const ENOUGH_VALUE: &str = "Go ahead with what you have";
const ENOUGH_SOURCE: &str = "you";

/// One detail a plan's work needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub id: &'static str,
    /// The header a question about it carries when its maker gave none that fits.
    pub header: &'static str,
    /// What the maker is told it means.
    pub means: &'static str,
    /// The fact-sheet lines the maker sees for it.
    pub categories: &'static [FactCategory],
}

use FactCategory as C;

const TRIP: &[Gap] = &[
    Gap {
        id: "dates",
        header: "Dates",
        means: "when the trip starts and ends",
        categories: &[C::Routine, C::Work, C::Plan],
    },
    Gap {
        id: "who",
        header: "Who",
        means: "who is going",
        categories: &[C::People],
    },
    Gap {
        id: "budget",
        header: "Budget",
        means: "how much they want to spend in all",
        categories: &[C::Hunt, C::Plan],
    },
    Gap {
        id: "stay",
        header: "Stay",
        means: "where to stay, or the base to work from",
        categories: &[C::Style, C::Health, C::Avoid],
    },
    Gap {
        id: "getting_there",
        header: "Travel",
        means: "how they get there and back",
        categories: &[C::Home, C::Health, C::Routine],
    },
];

const EVENT: &[Gap] = &[
    Gap {
        id: "when",
        header: "When",
        means: "the date and time",
        categories: &[C::Routine, C::Work],
    },
    Gap {
        id: "who",
        header: "Who",
        means: "who is coming",
        categories: &[C::People],
    },
    Gap {
        id: "budget",
        header: "Budget",
        means: "how much they want to spend",
        categories: &[C::Hunt, C::Plan],
    },
    Gap {
        id: "place",
        header: "Place",
        means: "where it happens",
        categories: &[C::Home, C::Style, C::Avoid],
    },
];

const PURCHASE: &[Gap] = &[
    Gap {
        id: "budget",
        header: "Budget",
        means: "the most they want to pay",
        categories: &[C::Hunt, C::Plan],
    },
    Gap {
        id: "must_haves",
        header: "Must-haves",
        means: "what it must have or do",
        categories: &[C::Hunt, C::Style, C::Health],
    },
    Gap {
        id: "by_when",
        header: "By when",
        means: "when they need it",
        categories: &[C::Plan, C::Routine],
    },
    Gap {
        id: "where_to_buy",
        header: "Where",
        means: "where they would buy it: a shop, online, second hand",
        categories: &[C::Hunt, C::Home],
    },
];

const PROJECT: &[Gap] = &[
    Gap {
        id: "done_looks_like",
        header: "Done means",
        means: "what finished looks like",
        categories: &[C::Plan, C::Work, C::Interest],
    },
    Gap {
        id: "deadline",
        header: "Deadline",
        means: "when it must be done",
        categories: &[C::Plan, C::Work, C::Routine],
    },
    Gap {
        id: "budget",
        header: "Budget",
        means: "how much they want to spend on it",
        categories: &[C::Hunt, C::Plan],
    },
    Gap {
        id: "help",
        header: "Help",
        means: "who or what helps with it",
        categories: &[C::People, C::Work],
    },
];

/// The gaps a plan of `kind` lists, in the order they are asked; none for an interest or a
/// standing plan, which are never asked about.
pub fn gaps_of(kind: AnchorKind) -> &'static [Gap] {
    match kind {
        AnchorKind::Trip => TRIP,
        AnchorKind::Event => EVENT,
        AnchorKind::Purchase => PURCHASE,
        AnchorKind::Project => PROJECT,
        AnchorKind::Interest | AnchorKind::Standing => &[],
    }
}

pub fn gap_named(kind: AnchorKind, id: &str) -> Option<&'static Gap> {
    gaps_of(kind).iter().find(|gap| gap.id == id.trim())
}

/// Words that make a place a lodging rather than a town; only a lodging settles `stay`.
const LODGINGS: &[&str] = &[
    "hotel",
    "lodge",
    "resort",
    "villa",
    "airbnb",
    "guest house",
    "guesthouse",
    "hostel",
    "apartment",
    "apartments",
    "cottage",
    "camp",
    "inn",
    "bnb",
    "suites",
    "banda",
];

fn names_a_lodging(place: &str) -> bool {
    let key = format!(" {} ", topic_key(place));
    LODGINGS
        .iter()
        .any(|word| key.contains(&format!(" {word} ")))
}

/// Whether a field the plan already holds settles `gap`: a date, a place, or a fact the plan
/// keeps under the gap's own name.
fn plan_settles(anchor: &Anchor, gap: &Gap) -> bool {
    let dated = anchor.starts_at.is_some() || anchor.ends_at.is_some();
    let held = match (anchor.kind, gap.id) {
        (AnchorKind::Trip, "dates") | (AnchorKind::Event, "when") => anchor.starts_at.is_some(),
        (AnchorKind::Purchase, "by_when") | (AnchorKind::Project, "deadline") => dated,
        (AnchorKind::Event, "place") => anchor
            .place
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty()),
        (AnchorKind::Trip, "stay") => anchor.place.as_deref().is_some_and(names_a_lodging),
        _ => false,
    };
    let named = [topic_key(gap.id), topic_key(gap.header)];
    held || anchor
        .facts
        .iter()
        .any(|fact| !fact.value.trim().is_empty() && named.contains(&topic_key(&fact.label)))
}

/// Whether the member told Goose to go ahead with what it has.
pub fn said_enough(anchor: &Anchor) -> bool {
    anchor
        .facts
        .iter()
        .any(|fact| topic_key(&fact.label) == topic_key(ENOUGH_LABEL) && !fact.value.is_empty())
}

/// Marks the plan as having enough: its remaining gaps become assumptions.
pub fn mark_enough(anchor: &mut Anchor, now: DateTime<Utc>) {
    anchor.set_fact(
        AnchorFact::new(ENOUGH_LABEL, ENOUGH_VALUE, ENOUGH_SOURCE),
        now,
    );
}

/// The gaps of `anchor` no field, stored answer or "enough" settles yet, at most one batch's
/// worth. `asked` is every question asked for it; one the member answered or passed on settles
/// its gap.
pub fn open_gaps(anchor: &Anchor, asked: &[Question]) -> Vec<&'static Gap> {
    if said_enough(anchor) {
        return Vec::new();
    }
    gaps_of(anchor.kind)
        .iter()
        .filter(|gap| !plan_settles(anchor, gap))
        .filter(|gap| {
            !asked.iter().any(|q| {
                q.gap.as_deref() == Some(gap.id)
                    && matches!(q.state, QuestionState::Answered | QuestionState::Skipped)
            })
        })
        .take(QUESTIONS_PER_BATCH)
        .collect()
}

/// The fact-sheet lines in the categories of `gaps`, so the maker sees only what bears on them.
pub fn lines_for<'a>(sheet: &'a [FactLine], gaps: &[&Gap]) -> Vec<&'a FactLine> {
    sheet
        .iter()
        .filter(|line| {
            gaps.iter()
                .any(|gap| gap.categories.contains(&line.category))
        })
        .collect()
}

/// The round a new batch for `asked`'s chain takes, from 1.
pub fn next_round_number(asked: &[Question]) -> u32 {
    asked.iter().map(|q| q.round).max().unwrap_or(0) + 1
}

/// What follows once a plan's batch is settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextRound {
    /// Nothing: no gap is left, a question is still open, or the member said enough.
    Stop,
    /// The single "keep answering?" question, as round `round`.
    KeepAsking { round: u32 },
    /// The questions job, for round `round`.
    Batch { round: u32 },
}

/// What a plan's chain does next, from every question asked for it.
pub fn next_round(anchor: &Anchor, asked: &[Question]) -> NextRound {
    if anchor.state != AnchorState::Active || asked.iter().any(|q| q.state.is_open()) {
        return NextRound::Stop;
    }
    let stopped = asked
        .iter()
        .filter(|q| q.gap.as_deref() == Some(KEEP_ASKING_GAP))
        .max_by_key(|q| q.round)
        .is_some_and(|q| q.state != QuestionState::Answered);
    if stopped || open_gaps(anchor, asked).is_empty() {
        return NextRound::Stop;
    }
    let round = next_round_number(asked);
    if is_soft_limit_round(round) {
        NextRound::KeepAsking { round }
    } else {
        NextRound::Batch { round }
    }
}

/// Queues the questions job for `anchor`, unless one is already open for it; returns the job
/// it queued.
pub async fn request_questions(
    jobs: &dyn EngineJobRepository,
    anchor: &Anchor,
    class: JobClass,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<EngineJob>> {
    if gaps_of(anchor.kind).is_empty() {
        return Ok(None);
    }
    let open = jobs
        .open_of_kind(&anchor.profile_id, JobKind::Questions)
        .await?;
    if open
        .iter()
        .any(|job| job.anchor_id.as_deref() == Some(anchor.id.as_str()))
    {
        return Ok(None);
    }
    let mut job = EngineJob::new(
        JobKind::Questions,
        class,
        Some(&anchor.profile_id),
        &anchor.title,
        now,
    );
    job.anchor_id = Some(anchor.id.clone());
    job.due_at = anchor.starts_at.or(anchor.ends_at);
    jobs.add(&job).await?;
    Ok(Some(job))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::AnchorOrigin;
    use crate::user_data::domain::fact_sheet::FactSource;
    use crate::user_data::domain::question::{
        keep_asking_question, Answer, QuestionPurpose, ASK_MORE, START_NOW,
    };
    use crate::user_data::mocks::mock_engine_jobs::InMemoryEngineJobRepository;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn plan(kind: AnchorKind, title: &str) -> Anchor {
        Anchor::new("jerry", kind, title, AnchorOrigin::You, now()).unwrap()
    }

    fn ids(gaps: &[&Gap]) -> Vec<&'static str> {
        gaps.iter().map(|gap| gap.id).collect()
    }

    fn asked(gap: &str, round: u32, state: QuestionState) -> Question {
        let mut q = Question::new(
            "jerry",
            &format!("b{round}"),
            QuestionPurpose::Anchor,
            &format!("About {gap} in round {round}?"),
            &["One", "Two"],
            false,
            now(),
        )
        .unwrap();
        q.gap = Some(gap.into());
        q.round = round;
        q.state = state;
        q
    }

    #[test]
    fn each_kind_lists_its_gaps_and_interests_and_standing_plans_none() {
        assert_eq!(
            ids(&gaps_of(AnchorKind::Trip).iter().collect::<Vec<_>>()),
            ["dates", "who", "budget", "stay", "getting_there"]
        );
        assert_eq!(gaps_of(AnchorKind::Event).len(), 4);
        assert_eq!(gaps_of(AnchorKind::Purchase)[1].id, "must_haves");
        assert_eq!(gaps_of(AnchorKind::Project)[0].id, "done_looks_like");
        assert!(gaps_of(AnchorKind::Interest).is_empty());
        assert!(gaps_of(AnchorKind::Standing).is_empty());
        for kind in [
            AnchorKind::Trip,
            AnchorKind::Event,
            AnchorKind::Purchase,
            AnchorKind::Project,
        ] {
            assert!(gaps_of(kind).iter().all(|g| g.header.chars().count() <= 12));
        }
    }

    #[test]
    fn a_plans_fields_settle_its_gaps() {
        let mut trip = plan(AnchorKind::Trip, "Diani in April");
        assert_eq!(open_gaps(&trip, &[]).len(), 4, "a batch's worth");
        trip.starts_at = Some(now());
        trip.place = Some("Diani".into());
        let open = ids(&open_gaps(&trip, &[]));
        assert_eq!(open, ["who", "budget", "stay", "getting_there"]);
        trip.place = Some("Leopard Beach Resort, Diani".into());
        trip.set_fact(AnchorFact::new("Budget", "KSh 80,000", "you"), now());
        assert_eq!(ids(&open_gaps(&trip, &[])), ["who", "getting_there"]);

        let mut summit = plan(AnchorKind::Event, "AI Summit");
        summit.starts_at = Some(now());
        summit.place = Some("KICC".into());
        assert_eq!(ids(&open_gaps(&summit, &[])), ["who", "budget"]);
        let mut bike = plan(AnchorKind::Purchase, "A bike");
        bike.ends_at = Some(now());
        assert!(!ids(&open_gaps(&bike, &[])).contains(&"by_when"));
    }

    #[test]
    fn an_answer_or_a_pass_settles_a_gap_and_enough_settles_them_all() {
        let mut trip = plan(AnchorKind::Trip, "Diani in April");
        let asked = [
            asked("dates", 1, QuestionState::Answered),
            asked("who", 1, QuestionState::Skipped),
            asked("budget", 1, QuestionState::Expired),
        ];
        assert_eq!(
            ids(&open_gaps(&trip, &asked)),
            ["budget", "stay", "getting_there"]
        );
        mark_enough(&mut trip, now());
        assert!(said_enough(&trip));
        assert!(open_gaps(&trip, &asked).is_empty());
    }

    #[test]
    fn the_maker_sees_only_the_lines_of_its_gaps_categories() {
        let line = |category: FactCategory, fact: &str| {
            FactLine::new(
                "jerry",
                category,
                fact,
                Default::default(),
                FactSource::you(),
                now(),
            )
            .unwrap()
        };
        let sheet = vec![
            line(FactCategory::People, "Lives with Amina and two children"),
            line(FactCategory::Interest, "Follows physics podcasts"),
            line(FactCategory::Work, "Works on audio tech"),
        ];
        let who = gap_named(AnchorKind::Trip, "who").unwrap();
        let shown: Vec<&str> = lines_for(&sheet, &[who])
            .iter()
            .map(|l| l.fact.as_str())
            .collect();
        assert_eq!(shown, ["Lives with Amina and two children"]);
    }

    #[test]
    fn a_chain_runs_to_the_soft_limit_then_asks_whether_to_go_on() {
        let mut trip = plan(AnchorKind::Trip, "Diani in April");
        trip.state = AnchorState::Active;
        assert_eq!(next_round(&trip, &[]), NextRound::Batch { round: 1 });
        let mut history = vec![asked("dates", 1, QuestionState::Open)];
        assert_eq!(next_round(&trip, &history), NextRound::Stop, "still open");
        history[0].state = QuestionState::Answered;
        assert_eq!(next_round(&trip, &history), NextRound::Batch { round: 2 });
        for round in 2..=5 {
            history.push(asked("budget", round, QuestionState::Expired));
        }
        assert_eq!(
            next_round(&trip, &history),
            NextRound::KeepAsking { round: 6 }
        );

        let mut keep = keep_asking_question("jerry", "b6", &trip.title, now()).unwrap();
        keep.round = 6;
        keep.answer(
            Answer {
                choices: vec![ASK_MORE.into()],
                text: None,
            },
            now(),
        )
        .unwrap();
        history.push(keep.clone());
        assert_eq!(next_round(&trip, &history), NextRound::Batch { round: 7 });

        let mut passed = keep.clone();
        passed.state = QuestionState::Skipped;
        *history.last_mut().unwrap() = passed;
        assert_eq!(
            next_round(&trip, &history),
            NextRound::Stop,
            "passed on going on"
        );

        let mut start = keep_asking_question("jerry", "b6", &trip.title, now()).unwrap();
        start
            .answer(
                Answer {
                    choices: vec![START_NOW.into()],
                    text: None,
                },
                now(),
            )
            .unwrap();
        mark_enough(&mut trip, now());
        assert_eq!(next_round(&trip, &[start]), NextRound::Stop);
    }

    #[test]
    fn a_chain_stops_when_no_gap_is_left_or_the_plan_is_not_active() {
        let mut summit = plan(AnchorKind::Event, "AI Summit");
        summit.state = AnchorState::Active;
        summit.starts_at = Some(now());
        summit.place = Some("KICC".into());
        let history = [
            asked("who", 1, QuestionState::Answered),
            asked("budget", 1, QuestionState::Answered),
        ];
        assert_eq!(next_round(&summit, &history), NextRound::Stop);
        let mut everyday = plan(AnchorKind::Standing, "Everyday");
        everyday.state = AnchorState::Active;
        assert_eq!(next_round(&everyday, &[]), NextRound::Stop);
        let mut over = plan(AnchorKind::Trip, "Kisumu");
        over.state = AnchorState::Ended;
        assert_eq!(next_round(&over, &[]), NextRound::Stop);
    }

    #[tokio::test]
    async fn the_questions_job_is_queued_once_per_plan() {
        let jobs = InMemoryEngineJobRepository::new();
        let trip = plan(AnchorKind::Trip, "Diani in April");
        let queued = request_questions(&jobs, &trip, JobClass::Asked, now())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(queued.kind, JobKind::Questions);
        assert_eq!(queued.anchor_id.as_deref(), Some(trip.id.as_str()));
        assert!(request_questions(&jobs, &trip, JobClass::Asked, now())
            .await
            .unwrap()
            .is_none());
        let interest = plan(AnchorKind::Interest, "Formula 1");
        assert!(request_questions(&jobs, &interest, JobClass::Asked, now())
            .await
            .unwrap()
            .is_none());
    }
}
