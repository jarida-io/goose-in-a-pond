//! The critic every engine job's draft meets before a member sees it: the prompt and the parse.

use super::{bullets, chars, clip, one_line, section, section_cost, Room, MIN_PART_CHARS};
use crate::user_data::domain::anchor::{Objection, ObjectionKind};
use crate::user_data::services::model_json::{object_in, text_of};

/// Objections one verdict keeps; a draft with more wrong than this is rewritten anyway.
pub const MAX_OBJECTIONS: usize = 8;
const MAX_FIELD_CHARS: usize = 240;

/// Fixed so it stays in the KV prefix. It shows no worked verdict: a small model copies one.
pub const CRITIC_SYSTEM: &str = "\
You check a draft before a member of this household sees it. You are the last \
check, so look for what is wrong with it. You never rewrite it.

You are given the brief the draft was written to, the source notes it was \
written from, what is known about the member, the member's don't-list, the \
titles of what was already made, and the draft.

Fail the draft when any of these is true:
- grounding: a name, date, price, place or link in it is not in the source \
notes, the brief or what is known about the member.
- relevance: a part of it does not serve the brief or this member.
- duplicate: it repeats something already made, or says one thing twice.
- dont_list: it breaks a rule on the don't-list.
- vague: it could be about anyone: no names, dates, prices or links where the \
brief asks for them.
- stale: it gives a date that has passed, or an old fact, as current.

Pass it when none is true. Wording you would have chosen differently is not a \
reason to fail.

Reply with JSON and nothing else:

{\"verdict\": \"pass\" or \"fail\", \"objections\": [{\"kind\": \"<one of the six words above>\", \"about\": \"<the claim or line>\", \"fix\": \"<what would make it pass>\"}]}

- At most 8 objections, the most serious first. An empty list when the verdict is pass.
- \"about\" and \"fix\": one line each, under 200 characters.
- No explanation, no preamble.";

const BRIEF_HEADING: &str = "Brief:";
const NOTES_HEADING: &str = "Source notes:";
const SHEET_HEADING: &str = "What is known about the member:";
const DONT_HEADING: &str = "The member's don't-list:";
const TITLES_HEADING: &str = "Already made:";
const DRAFT_HEADING: &str = "Draft to check:";
const REPLY_CUE: &str = "JSON:";
/// Room kept for the line that says how many notes are not shown.
const HIDDEN_LINE_CHARS: usize = 40;

/// What a step read or found, as a critic or a reviser is shown it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceNote<'a> {
    pub title: &'a str,
    pub body: &'a str,
}

/// Everything a critic is shown.
#[derive(Debug, Clone, Copy)]
pub struct CriticParts<'a> {
    pub brief: &'a str,
    pub draft: &'a str,
    pub notes: &'a [SourceNote<'a>],
    pub fact_sheet: &'a str,
    pub dont_list: &'a str,
    pub existing_titles: &'a [String],
}

/// The notes that fit in `room`, in order: the last ones go first, and the one on the edge
/// is cut at its end.
pub(super) fn fit_notes(notes: &[SourceNote<'_>], room: &mut Room) -> String {
    let Some(mut left) = room.0.checked_sub(section_cost(NOTES_HEADING)) else {
        return String::new();
    };
    if notes.len() > 1 {
        left = left.saturating_sub(HIDDEN_LINE_CHARS);
    }
    let mut shown: Vec<String> = Vec::new();
    for (number, note) in notes.iter().enumerate() {
        let head = format!("[{}] {}", number + 1, one_line(note.title));
        let whole = format!("{head}\n{}", note.body.trim());
        let cost = chars(&whole) + 2;
        if cost <= left {
            left -= cost;
            shown.push(whole);
            continue;
        }
        let for_body = left.saturating_sub(chars(&head) + 3);
        if for_body >= MIN_PART_CHARS {
            shown.push(format!("{head}\n{}", clip(note.body, for_body)));
        }
        break;
    }
    if shown.is_empty() {
        return String::new();
    }
    let hidden = notes.len() - shown.len();
    if hidden > 0 {
        shown.push(format!("[{hidden} more source notes are not shown]"));
    }
    let fitted = shown.join("\n\n");
    room.0 = room
        .0
        .saturating_sub(section_cost(NOTES_HEADING) + chars(&fitted));
    fitted
}

/// The critic's user message within `budget_chars`. The draft and the don't-list are whole
/// whatever the budget; the brief, the notes, the fact sheet and the titles fill what is left,
/// in that order.
pub fn build_critic_message(parts: &CriticParts<'_>, budget_chars: usize) -> String {
    let titles = bullets(parts.existing_titles.iter().map(String::as_str));
    let mut room = Room::new(budget_chars.saturating_sub(chars(REPLY_CUE)));
    room.keep(DRAFT_HEADING, parts.draft);
    room.keep(DONT_HEADING, parts.dont_list);
    let brief = room.text(BRIEF_HEADING, parts.brief);
    let notes = fit_notes(parts.notes, &mut room);
    let sheet = room.lines(SHEET_HEADING, parts.fact_sheet);
    let titles = room.lines(TITLES_HEADING, &titles);

    let mut out = String::new();
    section(&mut out, BRIEF_HEADING, &brief);
    section(&mut out, NOTES_HEADING, &notes);
    section(&mut out, SHEET_HEADING, &sheet);
    section(&mut out, DONT_HEADING, parts.dont_list);
    section(&mut out, TITLES_HEADING, &titles);
    // Last, so every round of one job shares the prompt before it.
    section(&mut out, DRAFT_HEADING, parts.draft);
    out.push_str(REPLY_CUE);
    out
}

/// A critic's ruling on one draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub pass: bool,
    pub objections: Vec<Objection>,
}

const UNREAD_ABOUT: &str = "the check itself";
const UNREAD_FIX: &str = "The check could not be read, so nothing in the draft is confirmed. \
     Check every name, date, price and link against the source notes and the brief.";

impl Verdict {
    /// The fail a reply that could not be read counts as.
    pub fn unread() -> Self {
        Self {
            pass: false,
            objections: vec![Objection::new(
                ObjectionKind::Grounding,
                UNREAD_ABOUT,
                UNREAD_FIX,
            )],
        }
    }

    pub fn is_unread(&self) -> bool {
        !self.pass && self.objections.iter().any(|o| o.about == UNREAD_ABOUT)
    }
}

/// A kind as a small model spells it: any case, `dont-list`, `don't list`, or two kinds at once.
fn kind_of(raw: &str) -> Option<ObjectionKind> {
    raw.split(['|', '/', ',']).find_map(|piece| {
        let spelled: String = piece
            .trim()
            .chars()
            .filter_map(|c| match c.to_ascii_lowercase() {
                c @ 'a'..='z' => Some(c),
                ' ' | '-' | '_' => Some('_'),
                _ => None,
            })
            .collect();
        ObjectionKind::parse(&spelled)
    })
}

fn objections_in(map: &serde_json::Map<String, serde_json::Value>) -> Vec<Objection> {
    let field = |item: &serde_json::Map<String, serde_json::Value>, key: &str| {
        text_of(item, key)
            .map(|text| clip(&text, MAX_FIELD_CHARS))
            .unwrap_or_default()
    };
    map.get("objections")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let item = item.as_object()?;
                    let kind = kind_of(item.get("kind")?.as_str()?)?;
                    Some(Objection::new(
                        kind,
                        &field(item, "about"),
                        &field(item, "fix"),
                    ))
                })
                .take(MAX_OBJECTIONS)
                .collect()
        })
        .unwrap_or_default()
}

/// Reads a critic's reply. Anything but a readable verdict is a fail, and so is a pass that
/// still objects to grounding or to the don't-list.
pub fn parse_verdict(raw: &str) -> Verdict {
    let Some(map) = object_in(raw) else {
        return Verdict::unread();
    };
    let objections = objections_in(&map);
    let said: String = map
        .get("verdict")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let blocks_a_pass =
        |o: &Objection| matches!(o.kind, ObjectionKind::Grounding | ObjectionKind::DontList);
    match said.as_str() {
        "pass" | "passed" | "passes" => Verdict {
            pass: !objections.iter().any(blocks_a_pass),
            objections,
        },
        _ if objections.is_empty() => Verdict::unread(),
        _ => Verdict {
            pass: false,
            objections,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>(notes: &'a [SourceNote<'a>], titles: &'a [String]) -> CriticParts<'a> {
        CriticParts {
            brief: "A dossier for the AI Summit in Nairobi on 3 December.",
            draft: "# AI Summit\nThe keynote is at 9am in the Tsavo ballroom.",
            notes,
            fact_sheet: "work: builds on-device assistants\nhome: lives in Nairobi",
            dont_list: "Never: recipes built on a colour preference",
            existing_titles: titles,
        }
    }

    fn notes() -> [SourceNote<'static>; 3] {
        [
            SourceNote {
                title: "Programme",
                body: "Keynote 9am, Tsavo ballroom. Workshops from 11am.",
            },
            SourceNote {
                title: "Venue",
                body: "KICC, Harambee Avenue. Doors open at 8am.",
            },
            SourceNote {
                title: "Speakers",
                body: "Forty speakers from twelve countries are listed on the site.",
            },
        ]
    }

    #[test]
    fn the_message_holds_every_part_and_ends_with_the_draft() {
        let notes = notes();
        let titles = vec!["Packing list for the summit".to_string()];
        let message = build_critic_message(&parts(&notes, &titles), 10_000);
        let at = |needle: &str| {
            message
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle}: {message}"))
        };
        let order = [
            at("Brief:"),
            at("Source notes:"),
            at("[3] Speakers"),
            at("What is known about the member:"),
            at("The member's don't-list:"),
            at("Already made:"),
            at("- Packing list for the summit"),
            at("Draft to check:\n# AI Summit\nThe keynote is at 9am in the Tsavo ballroom."),
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{message}");
        assert!(message.ends_with("ballroom.\n\nJSON:"), "{message}");
        assert!(chars(&message) <= 10_000);
    }

    #[test]
    fn a_tight_budget_cuts_the_titles_then_the_sheet_then_the_last_notes() {
        let notes = notes();
        let titles = vec!["Packing list for the summit".to_string()];
        let whole = chars(&build_critic_message(&parts(&notes, &titles), 10_000));

        let no_titles = build_critic_message(&parts(&notes, &titles), whole - 10);
        assert!(!no_titles.contains("Already made:"), "{no_titles}");
        assert!(no_titles.contains("home: lives in Nairobi"));
        assert!(no_titles.contains("[3] Speakers"));

        let fewer = build_critic_message(&parts(&notes, &titles), 330);
        assert!(!fewer.contains("What is known"), "{fewer}");
        assert!(fewer.contains("[1] Programme"), "{fewer}");
        assert!(!fewer.contains("[3] Speakers"), "{fewer}");
        assert!(fewer.contains("more source notes are not shown"), "{fewer}");
        assert!(chars(&fewer) <= 330, "{} chars", chars(&fewer));
    }

    #[test]
    fn the_draft_and_the_dont_list_are_whole_whatever_the_budget() {
        let notes = notes();
        let message = build_critic_message(&parts(&notes, &[]), 0);
        assert!(message.contains("The keynote is at 9am in the Tsavo ballroom."));
        assert!(message.contains("Never: recipes built on a colour preference"));
        assert!(!message.contains("Brief:"));
        assert!(!message.contains("Source notes:"));
    }

    #[test]
    fn a_plain_verdict_reads() {
        let pass = parse_verdict(r#"{"verdict": "pass", "objections": []}"#);
        assert!(pass.pass && pass.objections.is_empty());

        let fail = parse_verdict(
            r#"{"verdict": "fail", "objections": [
                {"kind": "grounding", "about": "the keynote time", "fix": "cite the programme"},
                {"kind": "vague", "about": "getting there", "fix": "name the matatu route"}]}"#,
        );
        assert!(!fail.pass);
        assert_eq!(
            fail.objections,
            [
                Objection::new(
                    ObjectionKind::Grounding,
                    "the keynote time",
                    "cite the programme"
                ),
                Objection::new(
                    ObjectionKind::Vague,
                    "getting there",
                    "name the matatu route"
                ),
            ]
        );
        assert!(!fail.is_unread());
    }

    #[test]
    fn the_shapes_a_small_model_writes_all_read() {
        let fenced = "```json\n{\"verdict\": \"pass\", \"objections\": []}\n```";
        assert!(parse_verdict(fenced).pass);

        let prefaced = "Here is my verdict:\n{\"verdict\": \"fail\", \"objections\": \
                        [{\"kind\": \"Stale\", \"about\": \"the 2025 dates\", \"fix\": \"use 2026\"}]}\nDone.";
        let read = parse_verdict(prefaced);
        assert!(!read.pass);
        assert_eq!(read.objections[0].kind, ObjectionKind::Stale);

        assert!(parse_verdict(r#"{"verdict":"PASS"}"#).pass);
        assert!(parse_verdict(r#"{"verdict":"Pass.","objections":[]}"#).pass);
        assert!(parse_verdict("<think>is it grounded?</think>{\"verdict\":\"passed\"}").pass);

        let spelled = parse_verdict(
            r#"{"verdict":"FAIL","objections":[
                {"kind":"don't-list","about":"the duck recipe","fix":"drop it"},
                {"kind":"Dont List","about":"x","fix":"y"},
                {"kind":"relevance | vague","about":"the intro","fix":"cut it"}]}"#,
        );
        let kinds: Vec<ObjectionKind> = spelled.objections.iter().map(|o| o.kind).collect();
        assert_eq!(
            kinds,
            [
                ObjectionKind::DontList,
                ObjectionKind::DontList,
                ObjectionKind::Relevance
            ]
        );
    }

    #[test]
    fn a_reply_that_does_not_parse_is_a_fail_that_says_so() {
        for raw in [
            "",
            "The draft looks good to me!",
            "{\"verdict\": \"pass\", \"objections\": [",
            r#"{"verdict": "maybe"}"#,
            r#"{"objections": []}"#,
            r#"{"verdict": "fail", "objections": []}"#,
            r#"{"verdict": "fail", "objections": ["the price is wrong"]}"#,
            r#"{"verdict": "fail", "objections": [{"kind": "<one of the six words above>", "about": "x"}]}"#,
        ] {
            let verdict = parse_verdict(raw);
            assert!(!verdict.pass, "{raw}");
            assert!(verdict.is_unread(), "{raw}");
            assert_eq!(verdict.objections.len(), 1, "{raw}");
            assert!(verdict.objections[0].fix.contains("could not be read"));
        }
    }

    #[test]
    fn a_pass_that_still_objects_to_grounding_or_the_dont_list_is_a_fail() {
        for kind in ["grounding", "dont_list"] {
            let raw = format!(
                r#"{{"verdict":"pass","objections":[{{"kind":"{kind}","about":"a","fix":"b"}}]}}"#
            );
            let verdict = parse_verdict(&raw);
            assert!(!verdict.pass, "{kind}");
            assert_eq!(verdict.objections.len(), 1);
        }
        let minor = parse_verdict(
            r#"{"verdict":"pass","objections":[{"kind":"vague","about":"a","fix":"b"}]}"#,
        );
        assert!(minor.pass, "a lesser remark does not undo a pass");
        assert_eq!(minor.objections.len(), 1);
    }

    #[test]
    fn a_missing_verdict_with_objections_is_a_fail_with_them() {
        let verdict = parse_verdict(
            r#"{"objections":[{"kind":"duplicate","about":"the venue, twice","fix":"say it once"}]}"#,
        );
        assert!(!verdict.pass);
        assert!(!verdict.is_unread());
        assert_eq!(verdict.objections[0].kind, ObjectionKind::Duplicate);
    }

    #[test]
    fn objections_are_capped_at_eight_and_their_lines_are_bounded() {
        let many: Vec<String> = (0..12)
            .map(|i| {
                format!(
                    r#"{{"kind":"vague","about":"line {i}","fix":"{}"}}"#,
                    "word ".repeat(90)
                )
            })
            .collect();
        let raw = format!(r#"{{"verdict":"fail","objections":[{}]}}"#, many.join(","));
        let verdict = parse_verdict(&raw);
        assert_eq!(verdict.objections.len(), MAX_OBJECTIONS);
        assert_eq!(verdict.objections[0].about, "line 0");
        assert!(chars(&verdict.objections[0].fix) <= MAX_FIELD_CHARS);
    }

    #[test]
    fn the_system_prompt_names_every_kind_and_shows_no_filled_in_verdict() {
        for kind in [
            ObjectionKind::Grounding,
            ObjectionKind::Relevance,
            ObjectionKind::Duplicate,
            ObjectionKind::DontList,
            ObjectionKind::Vague,
            ObjectionKind::Stale,
        ] {
            assert!(
                CRITIC_SYSTEM.contains(&format!("- {}:", kind.as_str())),
                "{kind:?}"
            );
        }
        assert!(CRITIC_SYSTEM.contains(&format!("At most {MAX_OBJECTIONS} objections")));
        assert!(
            parse_verdict(CRITIC_SYSTEM).is_unread(),
            "the schema line itself must not read as a verdict"
        );
    }
}
