//! Adding an anchor a member asked for, by hand or in a chat: a new one, or the one they already
//! have under that title, confirmed or brought back.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::user_data::domain::anchor::{
    clean_title, Anchor, AnchorFact, AnchorKind, AnchorOrigin, AnchorState,
};
use crate::user_data::domain::dont_list::RuleOrigin;
use crate::user_data::ports::anchors::AnchorRepository;
use crate::user_data::ports::dont_list::DontListRepository;
use crate::user_data::services::engine::recipes::{article, slots_of};
use crate::user_data::services::extraction_gate::{
    anchor_from_plan, moment_said, PlanCandidate, PLAN_KINDS,
};

const MAX_WHEN_CHARS: usize = 80;
const MAX_PLACE_CHARS: usize = 120;
const MAX_DETAILS_CHARS: usize = 600;
/// The source of a fact the member said in a conversation.
const SAID_IN_CHAT: &str = "you said";

/// What adding an anchor came to; each holds the anchor as it is now stored.
#[derive(Debug, Clone, PartialEq)]
pub enum AnchorAdded {
    New(Anchor),
    /// Already active under that title, and left as it was.
    Held(Anchor),
    /// Goose had offered it; asking for it is the member's yes.
    Confirmed(Anchor),
    /// A faded or stopped one, active again.
    Revived(Anchor),
    /// One under that title has ended, so nothing was added.
    Ended(Anchor),
}

impl AnchorAdded {
    pub fn anchor(&self) -> &Anchor {
        match self {
            Self::New(anchor)
            | Self::Held(anchor)
            | Self::Confirmed(anchor)
            | Self::Revived(anchor)
            | Self::Ended(anchor) => anchor,
        }
    }
}

/// Lays what the member said just now over an anchor of theirs that already held details.
fn lay_over(held: &mut Anchor, asked: Anchor, now: DateTime<Utc>) {
    // The dates go as a pair, so an old end never sits before a new start.
    if asked.starts_at.is_some() || asked.ends_at.is_some() {
        held.starts_at = asked.starts_at;
        held.ends_at = asked.ends_at;
    }
    if asked.place.is_some() {
        held.place = asked.place;
    }
    for fact in asked.facts {
        held.set_fact(fact, now);
    }
}

async fn save(anchors: &dyn AnchorRepository, anchor: &Anchor) -> Result<()> {
    if anchors.save(anchor).await? {
        Ok(())
    } else {
        Err(anyhow!("the anchor was removed while it was being saved"))
    }
}

/// Takes back the rule that stopping this anchor left, when the member asks for it again.
async fn forget_stop_rule(
    dont: &dyn DontListRepository,
    member: &str,
    anchor_id: &str,
) -> Result<()> {
    for rule in dont.list(member, None).await? {
        if rule.origin == RuleOrigin::Stop && rule.origin_ref.as_deref() == Some(anchor_id) {
            dont.delete(member, &rule.id).await?;
        }
    }
    Ok(())
}

/// Stores `asked` for its owner, unless they have an anchor under its title: an active one is
/// answered as it is, a candidate is confirmed, a faded or stopped one is brought back, each
/// with what `asked` says laid over it.
pub async fn add_asked(
    anchors: &dyn AnchorRepository,
    dont: &dyn DontListRepository,
    asked: Anchor,
    now: DateTime<Utc>,
) -> Result<AnchorAdded> {
    let member = asked.profile_id.clone();
    let key = asked.key();
    let held = anchors.by_title(&member, &key).await?;
    if held.is_none() && anchors.add(&asked).await? {
        return Ok(AnchorAdded::New(asked));
    }
    // `add` refuses only a title taken since the read above, so the read now finds its holder.
    let held = match held {
        Some(held) => held,
        None => anchors
            .by_title(&member, &key)
            .await?
            .ok_or_else(|| anyhow!("the anchor could not be added: try again"))?,
    };
    Ok(match held.state {
        AnchorState::Active => AnchorAdded::Held(held),
        AnchorState::Candidate => {
            let mut confirmed = held;
            confirmed.confirm(now)?;
            confirmed.kind = asked.kind;
            confirmed.agent = asked.agent;
            lay_over(&mut confirmed, asked, now);
            save(anchors, &confirmed).await?;
            AnchorAdded::Confirmed(confirmed)
        }
        AnchorState::Faded | AnchorState::Stopped => {
            let id = held.id.clone();
            let mut revived = Anchor {
                id: held.id,
                starts_at: held.starts_at,
                ends_at: held.ends_at,
                place: held.place,
                facts: held.facts,
                ..asked.clone()
            };
            // What was made for a stopped one hangs off the slots of the kind it had.
            if held.state == AnchorState::Stopped {
                revived.kind = held.kind;
                revived.agent = held.agent;
            }
            lay_over(&mut revived, asked, now);
            save(anchors, &revived).await?;
            forget_stop_rule(dont, &member, &id).await?;
            AnchorAdded::Revived(revived)
        }
        AnchorState::Ended => AnchorAdded::Ended(held),
    })
}

/// What a member asked Goose to plan, prepare for or keep an eye on, as the chat model sent it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanAsked {
    pub kind: String,
    pub title: String,
    pub when: Option<String>,
    pub until: Option<String>,
    pub place: Option<String>,
    pub details: Option<String>,
}

/// Why a plan could not become an anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanRefused {
    Kind,
    Title,
    Passed,
    EndsFirst,
}

impl PlanRefused {
    /// What the chat model should do about it.
    pub fn advice(self) -> &'static str {
        match self {
            Self::Kind => "`kind` must be \"event\", \"trip\", \"purchase\" or \"project\".",
            Self::Title => "Send `title`: what it is, in a few of their words.",
            Self::Passed => "That date has already passed. Ask them which date they mean.",
            Self::EndsFirst => "`until` falls before `when`. Ask them for the two dates again.",
        }
    }
}

fn one_line(text: Option<&str>, max_chars: usize) -> String {
    text.unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

/// Parses natural-language date/time words relative to `said_at` in `tz`.
pub fn parse_when_words(words: &str, said_at: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
    moment_said(words, said_at, tz)
}

/// The active anchor `asked` is for, owned by `profile_id`, its dates read from words said at
/// `said_at` in `tz`. Words that name no date are kept as a fact.
pub fn anchor_for_plan(
    profile_id: &str,
    asked: &PlanAsked,
    said_at: DateTime<Utc>,
    tz: Tz,
) -> Result<Anchor, PlanRefused> {
    let kind = AnchorKind::parse(&asked.kind)
        .filter(|kind| PLAN_KINDS.contains(kind))
        .ok_or(PlanRefused::Kind)?;
    let plan = PlanCandidate {
        kind,
        title: clean_title(&asked.title).ok_or(PlanRefused::Title)?,
        when: one_line(asked.when.as_deref(), MAX_WHEN_CHARS),
        place: one_line(asked.place.as_deref(), MAX_PLACE_CHARS),
        details: one_line(asked.details.as_deref(), MAX_DETAILS_CHARS),
    };
    let mut anchor = anchor_from_plan(
        &plan,
        profile_id,
        AnchorOrigin::Goose,
        None,
        said_at,
        tz,
        said_at,
    )
    .ok_or(PlanRefused::Passed)?;
    let until = one_line(asked.until.as_deref(), MAX_WHEN_CHARS);
    if !until.is_empty() {
        match moment_said(&until, said_at, tz) {
            Some(end) if anchor.starts_at.is_some_and(|start| end < start) => {
                return Err(PlanRefused::EndsFirst)
            }
            Some(end) => anchor.ends_at = Some(end),
            None => anchor.set_fact(AnchorFact::new("Until", &until, SAID_IN_CHAT), said_at),
        }
    }
    anchor.why = format!(
        "You asked for it on {}",
        said_at.with_timezone(&tz).format("%-d %B")
    );
    Ok(anchor)
}

/// What Goose tells the member it will do for `added`, in one plain line; `None` for an ended one.
pub fn plan_reply(added: &AnchorAdded) -> Option<String> {
    let anchor = added.anchor();
    let work = slots_of(anchor)
        .first()
        .map_or("something", |slot| article(slot.kind));
    Some(match added {
        AnchorAdded::New(_) | AnchorAdded::Confirmed(_) | AnchorAdded::Revived(_) => format!(
            "I'll work on {work} for your {} and tell you when it's ready.",
            anchor.title
        ),
        AnchorAdded::Held(_) => format!(
            "I'm already working on your {} and will tell you when something is ready.",
            anchor.title
        ),
        AnchorAdded::Ended(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::dont_list::{DontRule, Strength};
    use crate::user_data::mocks::mock_anchors::InMemoryAnchorRepository;
    use crate::user_data::mocks::mock_dont_list::InMemoryDontListRepository;
    use chrono::TimeZone;

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 17, 7, 0, 0).unwrap()
    }

    fn asked(kind: &str, title: &str) -> PlanAsked {
        PlanAsked {
            kind: kind.to_string(),
            title: title.to_string(),
            ..PlanAsked::default()
        }
    }

    fn trip(title: &str) -> Anchor {
        anchor_for_plan("jerry", &asked("trip", title), now(), NAIROBI).unwrap()
    }

    #[test]
    fn a_plan_becomes_an_active_anchor_with_its_dates_read_from_their_words() {
        let mut plan = asked("trip", "  Diani   trip ");
        plan.when = Some("19 December".into());
        plan.until = Some("23 December".into());
        plan.place = Some("Diani\nBeach".into());
        plan.details = Some("With Liz, by the SGR".into());
        let anchor = anchor_for_plan("jerry", &plan, now(), NAIROBI).unwrap();
        assert_eq!(anchor.title, "Diani trip");
        assert_eq!(anchor.state, AnchorState::Active);
        assert_eq!(anchor.origin, AnchorOrigin::Goose);
        assert_eq!(anchor.profile_id, "jerry");
        let local = |at: Option<DateTime<Utc>>| at.unwrap().with_timezone(&NAIROBI).date_naive();
        assert_eq!(local(anchor.starts_at).to_string(), "2026-12-19");
        assert_eq!(local(anchor.ends_at).to_string(), "2026-12-23");
        assert_eq!(anchor.place.as_deref(), Some("Diani Beach"));
        assert_eq!(anchor.facts[0].value, "With Liz, by the SGR");
        assert_eq!(anchor.why, "You asked for it on 17 November");
    }

    #[test]
    fn words_that_name_no_date_are_kept_as_facts() {
        let mut plan = asked("event", "The wedding");
        plan.when = Some("sometime in the new year".into());
        plan.until = Some("when it ends".into());
        let anchor = anchor_for_plan("jerry", &plan, now(), NAIROBI).unwrap();
        assert_eq!((anchor.starts_at, anchor.ends_at), (None, None));
        let facts: Vec<(&str, &str)> = anchor
            .facts
            .iter()
            .map(|f| (f.label.as_str(), f.value.as_str()))
            .collect();
        assert_eq!(
            facts,
            [
                ("When", "sometime in the new year"),
                ("Until", "when it ends")
            ]
        );
    }

    #[test]
    fn a_plan_that_cannot_be_an_anchor_says_why() {
        for kind in ["interest", "standing", "holiday", ""] {
            assert_eq!(
                anchor_for_plan("jerry", &asked(kind, "Diani"), now(), NAIROBI),
                Err(PlanRefused::Kind),
                "{kind}"
            );
        }
        assert_eq!(
            anchor_for_plan("jerry", &asked("trip", " - "), now(), NAIROBI),
            Err(PlanRefused::Title)
        );
        let mut backwards = asked("trip", "Diani");
        backwards.when = Some("23 December".into());
        backwards.until = Some("19 December".into());
        assert_eq!(
            anchor_for_plan("jerry", &backwards, now(), NAIROBI),
            Err(PlanRefused::EndsFirst)
        );
        for refused in [
            PlanRefused::Kind,
            PlanRefused::Title,
            PlanRefused::Passed,
            PlanRefused::EndsFirst,
        ] {
            assert!(!refused.advice().is_empty());
        }
    }

    #[tokio::test]
    async fn a_new_title_is_stored_and_the_same_title_is_answered_as_held() {
        let (anchors, dont) = (
            InMemoryAnchorRepository::new(),
            InMemoryDontListRepository::default(),
        );
        let added = add_asked(&anchors, &dont, trip("Diani trip"), now())
            .await
            .unwrap();
        let AnchorAdded::New(stored) = &added else {
            panic!("{added:?}")
        };
        assert_eq!(
            plan_reply(&added).unwrap(),
            "I'll work on a plan for your Diani trip and tell you when it's ready."
        );
        let again = add_asked(&anchors, &dont, trip("diani TRIP"), now())
            .await
            .unwrap();
        assert_eq!(again, AnchorAdded::Held(stored.clone()));
        assert!(plan_reply(&again)
            .unwrap()
            .starts_with("I'm already working"));
        assert_eq!(
            anchors
                .list("jerry", &AnchorState::LIVE)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_candidate_goose_offered_is_confirmed_with_what_was_just_said() {
        let (anchors, dont) = (
            InMemoryAnchorRepository::new(),
            InMemoryDontListRepository::default(),
        );
        let offered = Anchor::new(
            "jerry",
            AnchorKind::Event,
            "Diani trip",
            AnchorOrigin::Chat,
            now(),
        )
        .unwrap();
        assert!(anchors.add(&offered).await.unwrap());
        let mut plan = asked("trip", "Diani trip");
        plan.place = Some("Diani".into());
        let asked = anchor_for_plan("jerry", &plan, now(), NAIROBI).unwrap();
        let added = add_asked(&anchors, &dont, asked, now()).await.unwrap();
        let AnchorAdded::Confirmed(confirmed) = added else {
            panic!("{added:?}")
        };
        assert_eq!(confirmed.id, offered.id);
        assert_eq!(confirmed.state, AnchorState::Active);
        assert_eq!(confirmed.kind, AnchorKind::Trip);
        assert_eq!(confirmed.place.as_deref(), Some("Diani"));
        assert_eq!(
            anchors.get("jerry", &offered.id).await.unwrap(),
            Some(confirmed)
        );
    }

    #[tokio::test]
    async fn a_stopped_anchor_comes_back_with_its_kind_and_without_its_stop_rule() {
        let (anchors, dont) = (
            InMemoryAnchorRepository::new(),
            InMemoryDontListRepository::default(),
        );
        let mut stopped = trip("Diani trip");
        stopped.stop(now()).unwrap();
        assert!(anchors.add(&stopped).await.unwrap());
        let rule = DontRule::avoid_title(
            "jerry",
            "Diani trip",
            &[],
            RuleOrigin::Stop,
            &stopped.id,
            now(),
        )
        .unwrap();
        dont.add(&rule).await.unwrap();
        let kept = DontRule::new(
            "jerry",
            "Never: lists of bars",
            Strength::Never,
            RuleOrigin::You,
            now(),
        )
        .unwrap();
        dont.add(&kept).await.unwrap();

        let added = add_asked(
            &anchors,
            &dont,
            anchor_for_plan("jerry", &asked("event", "Diani trip"), now(), NAIROBI).unwrap(),
            now(),
        )
        .await
        .unwrap();
        let AnchorAdded::Revived(revived) = &added else {
            panic!("{added:?}")
        };
        assert_eq!(revived.id, stopped.id);
        assert_eq!(revived.state, AnchorState::Active);
        assert_eq!(
            revived.kind,
            AnchorKind::Trip,
            "what was made hangs off its slots"
        );
        let rules = dont.list("jerry", None).await.unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, kept.id);
    }

    #[tokio::test]
    async fn an_ended_anchor_is_not_added_again() {
        let (anchors, dont) = (
            InMemoryAnchorRepository::new(),
            InMemoryDontListRepository::default(),
        );
        let mut ended = trip("Diani trip");
        ended.end(now()).unwrap();
        assert!(anchors.add(&ended).await.unwrap());
        let added = add_asked(&anchors, &dont, trip("Diani trip"), now())
            .await
            .unwrap();
        assert_eq!(added, AnchorAdded::Ended(ended));
        assert_eq!(plan_reply(&added), None);
    }
}
