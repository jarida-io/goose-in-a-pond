//! The opening brief of a deliverable's research task: what its planner and every step are told
//! before they start.

use chrono::{DateTime, Utc};

use super::recipes::{article, deliverable_title, when_of, Slot};
use super::{answer_lines, bullets, chars, one_line, section, Room, CHANGED_HEADING, NO_CHANGE};
use crate::shared::services::agent_activations::local_date_label;
use crate::user_data::domain::anchor::Anchor;

const ANCHOR_HEADING: &str = "What this is for:";
const RECIPE_HEADING: &str = "What to produce:";
const PLAYBOOK_HEADING: &str = "How to do it well:";
const SHEET_HEADING: &str = "What is known about the member:";
const DONT_HEADING: &str = "The member's don't-list (never break it):";
const ANSWERS_HEADING: &str = "What the member answered about this:";
const OTHER_ANSWERS_HEADING: &str = "What they answered about other things:";
const CAPTURES_HEADING: &str = "From their screenshots and photos:";
const OTHERS_HEADING: &str = "Already made for this (do not repeat it):";
const REFRESH_HEADING: &str = "This is a refresh:";
const LAST_HEADING: &str = "The last version:";

/// Everything a deliverable's brief is built from.
#[derive(Debug, Clone, Copy)]
pub struct BriefParts<'a> {
    pub now: DateTime<Utc>,
    /// The household's IANA timezone; dates are written on its clock.
    pub timezone: &'a str,
    /// Where the member is.
    pub member_place: Option<&'a str>,
    pub anchor: &'a Anchor,
    pub slot: &'a Slot,
    /// The playbook of the specialist that owns the anchor, when the member has it on.
    pub playbook: Option<&'a str>,
    pub fact_sheet: &'a str,
    /// Every `never` rule and the `avoid` rules that bear on the anchor.
    pub dont_list: &'a str,
    /// The member's answers for this anchor, as question and answer.
    pub answers: &'a [(String, String)],
    /// Their answers about other anchors that bear on this one.
    pub other_answers: &'a [(String, String)],
    /// What their captures that bear on the anchor show, one line each.
    pub captures: &'a [String],
    /// The anchor's other deliverables, as title and hook.
    pub other_deliverables: &'a [(String, String)],
    /// On a refresh, the body the member has now.
    pub last_version: Option<&'a str>,
}

/// An anchor as a prompt reads it: its kind, title, dates, place and facts.
pub fn anchor_block(anchor: &Anchor, timezone: &str) -> String {
    let mut lines = vec![
        format!("Kind: {}", anchor.kind.as_str()),
        format!("Title: {}", anchor.title),
    ];
    if let Some(when) = when_of(anchor, timezone) {
        lines.push(format!("When: {when}"));
    }
    if let Some(place) = anchor
        .place
        .as_deref()
        .map(one_line)
        .filter(|p| !p.is_empty())
    {
        lines.push(format!("Where: {place}"));
    }
    let facts: Vec<String> = anchor
        .facts
        .iter()
        .filter(|fact| !fact.value.is_empty())
        .map(|fact| {
            if fact.source.is_empty() {
                format!("- {}: {}", fact.label, fact.value)
            } else {
                format!("- {}: {} ({})", fact.label, fact.value, fact.source)
            }
        })
        .collect();
    if !facts.is_empty() {
        lines.push("Known so far:".to_string());
        lines.extend(facts);
    }
    lines.join("\n")
}

fn recipe_block(anchor: &Anchor, slot: &Slot) -> String {
    let mut what = article(slot.kind).to_string();
    if let Some(first) = what.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    format!(
        "{what} titled \"{}\".\nIt must contain: {}.\nWhere a detail is missing, state the \
         assumption you made. Do not stop to ask.",
        deliverable_title(anchor, slot),
        slot.must_contain
    )
}

fn refresh_instruction() -> String {
    format!(
        "Check the last version against today's sources. Open the new document with a short \
         section headed \"{CHANGED_HEADING}\" that says what is new or different since then. If \
         nothing is, write exactly \"{NO_CHANGE}\" under that heading."
    )
}

fn titled(items: &[(String, String)]) -> String {
    let lines: Vec<String> = items
        .iter()
        .map(|(title, hook)| match one_line(hook) {
            hook if hook.is_empty() => one_line(title),
            hook => format!("{}: {hook}", one_line(title)),
        })
        .collect();
    bullets(lines.iter().map(String::as_str))
}

/// The brief within `budget_chars`. The date, the anchor, the recipe and the don't-list are
/// whole whatever the budget; the rest is cut from the last part back.
pub fn deliverable_brief(parts: &BriefParts<'_>, budget_chars: usize) -> String {
    let mut today = format!("Today is {}.", local_date_label(parts.now, parts.timezone));
    if let Some(place) = parts.member_place.map(one_line).filter(|p| !p.is_empty()) {
        today.push_str(&format!(" The member is in {place}."));
    }
    let anchor = anchor_block(parts.anchor, parts.timezone);
    let recipe = recipe_block(parts.anchor, parts.slot);
    let last_version = parts.last_version.map(str::trim).filter(|v| !v.is_empty());
    let refresh = last_version
        .map(|_| refresh_instruction())
        .unwrap_or_default();

    let mut room = Room::new(budget_chars.saturating_sub(chars(&today) + 2));
    room.keep(ANCHOR_HEADING, &anchor);
    room.keep(RECIPE_HEADING, &recipe);
    room.keep(DONT_HEADING, parts.dont_list);
    room.keep(REFRESH_HEADING, &refresh);
    let playbook = room.lines(PLAYBOOK_HEADING, parts.playbook.unwrap_or_default());
    let sheet = room.lines(SHEET_HEADING, parts.fact_sheet);
    let answers = room.lines(ANSWERS_HEADING, &answer_lines(parts.answers));
    let other_answers = room.lines(OTHER_ANSWERS_HEADING, &answer_lines(parts.other_answers));
    let captures = room.lines(
        CAPTURES_HEADING,
        &bullets(parts.captures.iter().map(String::as_str)),
    );
    let others = room.lines(OTHERS_HEADING, &titled(parts.other_deliverables));
    let last = room.text(LAST_HEADING, last_version.unwrap_or_default());

    let mut out = format!("{today}\n\n");
    section(&mut out, ANCHOR_HEADING, &anchor);
    section(&mut out, RECIPE_HEADING, &recipe);
    section(&mut out, PLAYBOOK_HEADING, &playbook);
    section(&mut out, SHEET_HEADING, &sheet);
    section(&mut out, DONT_HEADING, parts.dont_list);
    section(&mut out, ANSWERS_HEADING, &answers);
    section(&mut out, OTHER_ANSWERS_HEADING, &other_answers);
    section(&mut out, CAPTURES_HEADING, &captures);
    section(&mut out, OTHERS_HEADING, &others);
    section(&mut out, REFRESH_HEADING, &refresh);
    section(&mut out, LAST_HEADING, &last);
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::{AnchorFact, AnchorKind, AnchorOrigin};
    use crate::user_data::domain::specialist::{specialist, SpecialistId};
    use crate::user_data::services::engine::recipes::slot_named;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn summit() -> Anchor {
        let mut anchor = Anchor::new(
            "jerry",
            AnchorKind::Event,
            "AI Summit Nairobi",
            AnchorOrigin::Chat,
            now(),
        )
        .unwrap();
        anchor.starts_at = Some(Utc.with_ymd_and_hms(2026, 12, 2, 21, 0, 0).unwrap());
        anchor.place = Some("KICC, Nairobi".into());
        anchor.set_fact(AnchorFact::new("Ticket", "booked", "answer"), now());
        anchor
    }

    struct Held {
        anchor: Anchor,
        answers: Vec<(String, String)>,
        other_answers: Vec<(String, String)>,
        captures: Vec<String>,
        others: Vec<(String, String)>,
    }

    fn held() -> Held {
        Held {
            anchor: summit(),
            answers: vec![(
                "Which track matters most?".into(),
                "On-device models".into(),
            )],
            other_answers: vec![("Do you drive?".into(), "Yes".into())],
            captures: vec!["A ticket confirmation for the summit, seat B12".into()],
            others: vec![(
                "Packing list for AI Summit Nairobi".into(),
                "Carry a jacket: the hall is cold".into(),
            )],
        }
    }

    fn parts<'a>(held: &'a Held, last_version: Option<&'a str>) -> BriefParts<'a> {
        BriefParts {
            now: now(),
            timezone: "Africa/Nairobi",
            member_place: Some("Nairobi"),
            anchor: &held.anchor,
            slot: slot_named(&held.anchor, "dossier").unwrap(),
            playbook: Some(specialist(SpecialistId::Trip).playbook),
            fact_sheet: "work: builds on-device assistants\nfood: does not eat pork",
            dont_list: "Never: recipes built on a colour preference",
            answers: &held.answers,
            other_answers: &held.other_answers,
            captures: &held.captures,
            other_deliverables: &held.others,
            last_version,
        }
    }

    fn places(brief: &str, needles: &[&str]) -> Vec<usize> {
        needles
            .iter()
            .map(|needle| {
                brief
                    .find(needle)
                    .unwrap_or_else(|| panic!("missing {needle}: {brief}"))
            })
            .collect()
    }

    #[test]
    fn the_brief_holds_its_parts_in_the_order_a_step_needs_them() {
        let held = held();
        let brief = deliverable_brief(
            &parts(&held, Some("# Dossier\nThe old keynote time.")),
            20_000,
        );
        let order = places(
            &brief,
            &[
                "Today is Monday 16 November 2026. The member is in Nairobi.",
                "What this is for:\nKind: event\nTitle: AI Summit Nairobi\nWhen: on Thursday 3 December 2026\nWhere: KICC, Nairobi\nKnown so far:\n- Ticket: booked (answer)",
                "What to produce:\nA dossier titled \"Dossier for AI Summit Nairobi\".\nIt must contain: what it is, when and where;",
                "How to do it well:\nGive every day its date",
                "What is known about the member:\nwork: builds on-device assistants",
                "The member's don't-list (never break it):\nNever: recipes built on a colour preference",
                "What the member answered about this:\n- Which track matters most? Answer: On-device models",
                "What they answered about other things:\n- Do you drive? Answer: Yes",
                "From their screenshots and photos:\n- A ticket confirmation",
                "Already made for this (do not repeat it):\n- Packing list for AI Summit Nairobi: Carry a jacket",
                "This is a refresh:\nCheck the last version",
                "The last version:\n# Dossier\nThe old keynote time.",
            ],
        );
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{brief}");
        assert!(brief.contains("section headed \"What changed\""));
        assert!(brief.contains("write exactly \"No change.\""));
        assert!(!brief.contains(&held.anchor.id), "no ids");
        assert!(chars(&brief) <= 20_000);
    }

    #[test]
    fn a_first_version_says_nothing_of_a_refresh() {
        let held = held();
        let brief = deliverable_brief(&parts(&held, None), 20_000);
        assert!(!brief.contains("refresh"), "{brief}");
        assert!(!brief.contains(CHANGED_HEADING));
        assert!(!deliverable_brief(&parts(&held, Some("  ")), 20_000).contains("refresh"));
    }

    #[test]
    fn a_tight_budget_cuts_from_the_last_part_back() {
        let held = held();
        let old = "# Dossier\n".to_string() + &"The old keynote time. ".repeat(60);
        let whole = deliverable_brief(&parts(&held, Some(&old)), 20_000);
        assert!(whole.ends_with("The old keynote time."), "{whole}");

        let clipped = deliverable_brief(&parts(&held, Some(&old)), chars(&whole) - 400);
        assert!(
            clipped.ends_with("[cut]"),
            "the last version goes first: {clipped}"
        );
        assert!(clipped.contains("Already made for this"));
        assert!(chars(&clipped) <= chars(&whole) - 400);

        let first = deliverable_brief(&parts(&held, None), 20_000);
        let tighter = deliverable_brief(&parts(&held, None), chars(&first) - 20);
        assert!(!tighter.contains("Already made for this"), "{tighter}");
        assert!(tighter.contains("From their screenshots and photos:"));

        let tightest = deliverable_brief(&parts(&held, None), chars(&first) - 420);
        for gone in [
            "From their screenshots",
            "answered about other things",
            "answered about this",
        ] {
            assert!(!tightest.contains(gone), "{gone}: {tightest}");
        }
        assert!(tightest.contains("How to do it well:"), "{tightest}");
    }

    #[test]
    fn the_anchor_the_recipe_and_the_dont_list_are_whole_whatever_the_budget() {
        let held = held();
        let brief = deliverable_brief(&parts(&held, Some("# Dossier\nOld.")), 0);
        for kept in [
            "Today is Monday 16 November 2026.",
            "Title: AI Summit Nairobi",
            "- Ticket: booked (answer)",
            "It must contain: what it is, when and where;",
            "what to bring.",
            "Never: recipes built on a colour preference",
            "section headed \"What changed\"",
        ] {
            assert!(brief.contains(kept), "{kept}: {brief}");
        }
        for cut in [
            "How to do it well:",
            "What is known about the member:",
            "answered",
            "Already made",
            "The last version:",
        ] {
            assert!(!brief.contains(cut), "{cut}: {brief}");
        }
    }

    #[test]
    fn a_fact_sheet_too_long_for_the_budget_keeps_its_first_lines_whole() {
        let held = held();
        let mut long = parts(&held, None);
        let sheet: String = (1..=100)
            .map(|n| format!("interest: fact number {n} about the member\n"))
            .collect();
        long.fact_sheet = &sheet;
        let brief = deliverable_brief(&long, 2_000);
        assert!(brief.contains("interest: fact number 1 about the member\n"));
        assert!(!brief.contains("fact number 100"));
        assert!(chars(&brief) <= 2_000, "{}", chars(&brief));
        let shown = brief
            .lines()
            .filter(|line| line.starts_with("interest: fact number"))
            .count();
        assert!(shown > 5, "{shown} lines");
        assert!(
            brief
                .lines()
                .filter(|line| line.starts_with("interest:"))
                .all(|line| line.ends_with("about the member")),
            "no line is cut in half"
        );
    }
}
