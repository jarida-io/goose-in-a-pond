//! What Goose makes for each kind of anchor: the deliverable slots, in the order they are made.

use chrono::{DateTime, Timelike, Utc};

use super::{chars, CHANGED_HEADING};
use crate::user_data::domain::anchor::{
    Anchor, AnchorKind, Deliverable, DeliverableKind, DeliverableState,
};
use crate::user_data::domain::specialist::SpecialistId;

/// A deliverable title is cut to this, so the anchor's part of it is cut first.
const MAX_TITLE_CHARS: usize = 120;
const ANCHOR_MARK: &str = "{anchor}";

/// One deliverable an anchor gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub slot: &'static str,
    pub kind: DeliverableKind,
    /// The deliverable's title, with `{anchor}` where the anchor's own title goes.
    pub title: &'static str,
    pub must_contain: &'static str,
}

/// Which standing anchor one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// The one every member has: what is on near them.
    Everyday,
    /// The Budget specialist's.
    Budget,
}

const EVENT: &[Slot] = &[Slot {
    slot: "dossier",
    kind: DeliverableKind::Dossier,
    title: "Dossier for {anchor}",
    must_contain: "what it is, when and where; the weather on the day; the agenda and the \
                   sessions that fit the member; people worth meeting and why; how to get there; \
                   what to bring",
}];

const TRIP: &[Slot] = &[
    Slot {
        slot: "plan",
        kind: DeliverableKind::Plan,
        title: "Plan for {anchor}",
        must_contain: "each day with its date; transport there, around and back; where to stay; \
                       a budget table; the essentials to sort out before leaving",
    },
    Slot {
        slot: "stays",
        kind: DeliverableKind::Shortlist,
        title: "Places to stay for {anchor}",
        must_contain: "places to stay compared in one table, each with its price for the dates, \
                       what it offers, how far it is from what the member will do, and a link",
    },
    Slot {
        slot: "packing",
        kind: DeliverableKind::Checklist,
        title: "Packing list for {anchor}",
        must_contain: "what to pack for these dates, this place and its weather, in groups, one \
                       item to a line, with the documents and bookings to carry",
    },
];

const PURCHASE: &[Slot] = &[
    Slot {
        slot: "shortlist",
        kind: DeliverableKind::Shortlist,
        title: "Shortlist for {anchor}",
        must_contain: "listings worth a look, each with its price, its specifications, why it \
                       fits the member and a link",
    },
    Slot {
        slot: "costs",
        kind: DeliverableKind::Plan,
        title: "Total cost of {anchor}",
        must_contain: "the total cost beyond the price: duty, fees, delivery and running costs, \
                       each as a figure with where it comes from",
    },
];

const DIGEST: &[Slot] = &[Slot {
    slot: "digest",
    kind: DeliverableKind::Digest,
    title: "What is new in {anchor}",
    must_contain: "what is new since the last version: releases, news, people and deadlines, \
                   each with its date and why it matters to the member",
}];

const EVERYDAY: &[Slot] = &[Slot {
    slot: "out",
    kind: DeliverableKind::Shortlist,
    title: "Out this week",
    must_contain: "named places and events near the member this week, each with its day, time, \
                   place, price and a link, and why it fits them",
}];

const BUDGET: &[Slot] = &[Slot {
    slot: "month",
    kind: DeliverableKind::Digest,
    title: "Spending this month",
    must_contain: "the month's spending by category from the ledger, what changed from the \
                   month before, and each recurring payment with the date it next falls",
}];

/// Which standing anchor `anchor` is; `None` for any other kind.
pub fn standing_of(anchor: &Anchor) -> Option<Standing> {
    (anchor.kind == AnchorKind::Standing).then_some(match anchor.agent {
        Some(SpecialistId::Budget) => Standing::Budget,
        _ => Standing::Everyday,
    })
}

/// The slots of an anchor kind, in the order they are made; a standing anchor's are its owner's.
pub fn slots_for(kind: AnchorKind, standing: Option<Standing>) -> &'static [Slot] {
    match (kind, standing) {
        (AnchorKind::Event, _) => EVENT,
        (AnchorKind::Trip, _) => TRIP,
        (AnchorKind::Purchase, _) => PURCHASE,
        (AnchorKind::Project | AnchorKind::Interest, _) => DIGEST,
        (AnchorKind::Standing, Some(Standing::Everyday)) => EVERYDAY,
        (AnchorKind::Standing, Some(Standing::Budget)) => BUDGET,
        (AnchorKind::Standing, None) => &[],
    }
}

pub fn slots_of(anchor: &Anchor) -> &'static [Slot] {
    slots_for(anchor.kind, standing_of(anchor))
}

/// The anchor's slot of that name.
pub fn slot_named(anchor: &Anchor, slot: &str) -> Option<&'static Slot> {
    slots_of(anchor).iter().find(|s| s.slot == slot.trim())
}

/// The slot to queue next: the first with no deliverable, once every slot before it is ready.
pub fn next_slot(anchor: &Anchor, existing: &[Deliverable]) -> Option<&'static Slot> {
    for slot in slots_of(anchor) {
        let made = existing
            .iter()
            .find(|d| d.anchor_id == anchor.id && d.slot == slot.slot);
        match made {
            None => return Some(slot),
            Some(deliverable) if deliverable.state == DeliverableState::Ready => {}
            Some(_) => return None,
        }
    }
    None
}

pub fn deliverable_title(anchor: &Anchor, slot: &Slot) -> String {
    let fixed = chars(&slot.title.replace(ANCHOR_MARK, ""));
    let name: String = anchor
        .title
        .chars()
        .take(MAX_TITLE_CHARS.saturating_sub(fixed))
        .collect();
    slot.title.replace(ANCHOR_MARK, name.trim_end())
}

fn local(at: DateTime<Utc>, timezone: &str) -> String {
    let at = at.with_timezone(&timezone.parse().unwrap_or(chrono_tz::UTC));
    if at.hour() == 0 && at.minute() == 0 {
        at.format("%A %-d %B %Y").to_string()
    } else {
        at.format("%A %-d %B %Y at %H:%M").to_string()
    }
}

/// An anchor's dates on the household's clock, such as "from Thursday 3 December 2026 to
/// Saturday 5 December 2026"; `None` for an anchor with no date.
pub fn when_of(anchor: &Anchor, timezone: &str) -> Option<String> {
    match (anchor.starts_at, anchor.ends_at) {
        (Some(start), Some(end)) if end > start => Some(format!(
            "from {} to {}",
            local(start, timezone),
            local(end, timezone)
        )),
        (Some(start), _) => Some(format!("on {}", local(start, timezone))),
        (None, Some(end)) => Some(format!("until {}", local(end, timezone))),
        (None, None) => None,
    }
}

pub(super) fn article(kind: DeliverableKind) -> &'static str {
    match kind {
        DeliverableKind::Dossier => "a dossier",
        DeliverableKind::Plan => "a plan",
        DeliverableKind::Shortlist => "a shortlist",
        DeliverableKind::Digest => "a digest",
        DeliverableKind::Checklist => "a checklist",
    }
}

fn subject(anchor: &Anchor) -> String {
    match anchor.kind {
        AnchorKind::Event => format!("the event \"{}\"", anchor.title),
        AnchorKind::Trip => format!("the trip \"{}\"", anchor.title),
        AnchorKind::Purchase => format!("the purchase \"{}\"", anchor.title),
        AnchorKind::Project => format!("the project \"{}\"", anchor.title),
        AnchorKind::Interest => format!("the member's interest in \"{}\"", anchor.title),
        AnchorKind::Standing => match standing_of(anchor) {
            Some(Standing::Budget) => "the member's budget".to_string(),
            _ => "the member's week".to_string(),
        },
    }
}

/// The goal a deliverable's research task is given: one paragraph naming the anchor, its dates
/// and place on the household's clock, and what to produce.
pub fn goal_for(anchor: &Anchor, slot: &Slot, refresh: bool, timezone: &str) -> String {
    let verb = if refresh { "Bring up to date" } else { "Write" };
    let mut goal = format!(
        "{verb} {} titled \"{}\" for {}",
        article(slot.kind),
        deliverable_title(anchor, slot),
        subject(anchor)
    );
    if let Some(when) = when_of(anchor, timezone) {
        goal.push_str(&format!(", {when}"));
    }
    if let Some(place) = anchor
        .place
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        goal.push_str(&format!(", in {place}"));
    }
    goal.push_str(&format!(
        ". It must contain: {}. Use what the brief says about the member, give real names, \
         dates, prices and links from what you read, and state any assumption you had to make.",
        slot.must_contain
    ));
    if refresh {
        goal.push_str(&format!(
            " The brief holds the last version: check it against today's sources and open the \
             new one with a short \"{CHANGED_HEADING}\" section."
        ));
    }
    goal
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::AnchorOrigin;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn anchor(kind: AnchorKind, title: &str) -> Anchor {
        Anchor::new("jerry", kind, title, AnchorOrigin::You, now()).unwrap()
    }

    fn deliverable(anchor: &Anchor, slot: &str, state: DeliverableState) -> Deliverable {
        let recipe = slot_named(anchor, slot).unwrap();
        let mut made = Deliverable::new(
            &anchor.id,
            recipe.kind,
            recipe.slot,
            &deliverable_title(anchor, recipe),
            now(),
        )
        .unwrap();
        made.state = state;
        made
    }

    fn names(slots: &[Slot]) -> Vec<(&'static str, DeliverableKind)> {
        slots.iter().map(|s| (s.slot, s.kind)).collect()
    }

    #[test]
    fn every_anchor_kind_has_the_slots_its_recipe_lists_in_order() {
        use DeliverableKind::*;
        assert_eq!(
            names(slots_for(AnchorKind::Event, None)),
            [("dossier", Dossier)]
        );
        assert_eq!(
            names(slots_for(AnchorKind::Trip, None)),
            [("plan", Plan), ("stays", Shortlist), ("packing", Checklist)]
        );
        assert_eq!(
            names(slots_for(AnchorKind::Purchase, None)),
            [("shortlist", Shortlist), ("costs", Plan)]
        );
        assert_eq!(
            names(slots_for(AnchorKind::Project, None)),
            [("digest", Digest)]
        );
        assert_eq!(
            names(slots_for(AnchorKind::Interest, None)),
            [("digest", Digest)]
        );
        assert_eq!(
            names(slots_for(AnchorKind::Standing, Some(Standing::Everyday))),
            [("out", Shortlist)]
        );
        assert_eq!(
            names(slots_for(AnchorKind::Standing, Some(Standing::Budget))),
            [("month", Digest)]
        );
        assert!(slots_for(AnchorKind::Standing, None).is_empty());
    }

    #[test]
    fn every_slot_says_what_it_must_contain_and_has_a_title_that_fits() {
        let kinds = [
            (AnchorKind::Event, None),
            (AnchorKind::Trip, None),
            (AnchorKind::Purchase, None),
            (AnchorKind::Project, None),
            (AnchorKind::Standing, Some(Standing::Everyday)),
            (AnchorKind::Standing, Some(Standing::Budget)),
        ];
        let long = anchor(AnchorKind::Trip, &"very long title ".repeat(20));
        for (kind, standing) in kinds {
            let slots = slots_for(kind, standing);
            let mut seen = std::collections::HashSet::new();
            for slot in slots {
                assert!(seen.insert(slot.slot), "{} twice", slot.slot);
                assert!(chars(slot.must_contain) > 40, "{}", slot.slot);
                assert!(!slot.must_contain.ends_with('.'), "{}", slot.slot);
                let title = deliverable_title(&long, slot);
                assert!(chars(&title) <= MAX_TITLE_CHARS, "{title}");
                assert!(!title.contains(ANCHOR_MARK), "{title}");
            }
        }
    }

    #[test]
    fn a_standing_anchor_is_the_budgets_only_when_budget_owns_it() {
        let mut standing = anchor(AnchorKind::Standing, "Everyday");
        assert_eq!(standing_of(&standing), Some(Standing::Everyday));
        assert_eq!(
            names(slots_of(&standing)),
            [("out", DeliverableKind::Shortlist)]
        );
        standing.agent = Some(SpecialistId::Budget);
        assert_eq!(standing_of(&standing), Some(Standing::Budget));
        assert_eq!(
            names(slots_of(&standing)),
            [("month", DeliverableKind::Digest)]
        );
        assert_eq!(standing_of(&anchor(AnchorKind::Trip, "Kisumu")), None);
    }

    #[test]
    fn a_later_slot_waits_until_the_one_before_it_is_ready() {
        let trip = anchor(AnchorKind::Trip, "Kisumu in December");
        assert_eq!(next_slot(&trip, &[]).unwrap().slot, "plan");

        for waiting in [
            DeliverableState::Queued,
            DeliverableState::Making,
            DeliverableState::Unsure,
            DeliverableState::Rejected,
            DeliverableState::Archived,
        ] {
            let made = [deliverable(&trip, "plan", waiting)];
            assert_eq!(next_slot(&trip, &made), None, "{waiting:?}");
        }

        let mut made = vec![deliverable(&trip, "plan", DeliverableState::Ready)];
        assert_eq!(next_slot(&trip, &made).unwrap().slot, "stays");
        made.push(deliverable(&trip, "stays", DeliverableState::Making));
        assert_eq!(next_slot(&trip, &made), None);
        made[1].state = DeliverableState::Ready;
        assert_eq!(next_slot(&trip, &made).unwrap().slot, "packing");
        made.push(deliverable(&trip, "packing", DeliverableState::Ready));
        assert_eq!(next_slot(&trip, &made), None, "every slot is made");
    }

    #[test]
    fn another_anchors_deliverables_do_not_fill_a_slot() {
        let trip = anchor(AnchorKind::Trip, "Kisumu in December");
        let other = anchor(AnchorKind::Trip, "Diani in April");
        let theirs = [deliverable(&other, "plan", DeliverableState::Ready)];
        assert_eq!(next_slot(&trip, &theirs).unwrap().slot, "plan");
    }

    #[test]
    fn a_title_names_the_anchor_where_the_recipe_says() {
        let summit = anchor(AnchorKind::Event, "AI Summit Nairobi");
        assert_eq!(
            deliverable_title(&summit, slot_named(&summit, "dossier").unwrap()),
            "Dossier for AI Summit Nairobi"
        );
        let everyday = anchor(AnchorKind::Standing, "Everyday");
        assert_eq!(
            deliverable_title(&everyday, slot_named(&everyday, "out").unwrap()),
            "Out this week"
        );
        assert!(slot_named(&summit, "packing").is_none());
    }

    #[test]
    fn a_goal_names_the_anchor_its_dates_and_place_and_what_to_produce() {
        let mut summit = anchor(AnchorKind::Event, "AI Summit Nairobi");
        summit.starts_at = Some(Utc.with_ymd_and_hms(2026, 12, 2, 21, 0, 0).unwrap());
        summit.ends_at = Some(Utc.with_ymd_and_hms(2026, 12, 4, 14, 30, 0).unwrap());
        summit.place = Some("KICC, Nairobi".into());
        let slot = slot_named(&summit, "dossier").unwrap();
        let goal = goal_for(&summit, slot, false, "Africa/Nairobi");
        assert!(
            goal.starts_with(
                "Write a dossier titled \"Dossier for AI Summit Nairobi\" for the event \
                 \"AI Summit Nairobi\", from Thursday 3 December 2026 to Friday 4 December 2026 \
                 at 17:30, in KICC, Nairobi. It must contain: what it is, when and where;"
            ),
            "{goal}"
        );
        assert!(!goal.contains('\n'), "one paragraph");
        assert!(!goal.contains(CHANGED_HEADING));
        assert!(!goal.contains(&summit.id));

        let again = goal_for(&summit, slot, true, "Africa/Nairobi");
        assert!(again.starts_with("Bring up to date a dossier"), "{again}");
        assert!(again.contains("\"What changed\" section"), "{again}");
    }

    #[test]
    fn a_goal_for_an_anchor_with_no_date_or_place_says_neither() {
        let cars = anchor(AnchorKind::Interest, "Formula 1");
        let goal = goal_for(
            &cars,
            slot_named(&cars, "digest").unwrap(),
            false,
            "Africa/Nairobi",
        );
        assert!(
            goal.starts_with(
                "Write a digest titled \"What is new in Formula 1\" for the member's interest \
                 in \"Formula 1\". It must contain:"
            ),
            "{goal}"
        );

        let mut prado = anchor(AnchorKind::Purchase, "A used Prado");
        prado.ends_at = Some(Utc.with_ymd_and_hms(2027, 1, 31, 21, 0, 0).unwrap());
        assert_eq!(
            when_of(&prado, "Africa/Nairobi").as_deref(),
            Some("until Monday 1 February 2027")
        );
        assert_eq!(
            when_of(&prado, "not a zone").as_deref(),
            Some("until Sunday 31 January 2027 at 21:00")
        );
    }
}
