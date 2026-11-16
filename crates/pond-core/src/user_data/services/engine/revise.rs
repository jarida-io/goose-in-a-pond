//! The revision a failed draft gets: the prompt, and reading the corrected draft back.

use super::critic::{fit_notes, SourceNote};
use super::{chars, section, Room};
use crate::models::services::thought_filter::strip_reasoning;
use crate::user_data::domain::anchor::Objection;

/// Fixed so it stays in the KV prefix. The reply is the draft itself, never a wrapper round it.
pub const REVISE_SYSTEM: &str = "\
You correct a draft that a checker failed, before a member of this household \
sees it.

You are given the brief the draft was written to, the source notes it was \
written from, the draft, and the checker's objections.

Fix every objection and change nothing else:
- Take a fact only from the source notes or the brief. When they do not \
support a claim, remove the claim. Never invent a replacement.
- Keep the draft's headings, order, tables and links wherever no objection \
touches them.
- Keep its length. Do not summarise it, and do not shorten a part no objection \
names.
- Do not mention the checker, the objections or that anything was corrected.

Reply with the whole corrected draft and nothing else, in the form the draft \
has: a Markdown document stays Markdown, and JSON stays JSON with the same \
fields. No preamble, no code fence, no notes after it.";

const BRIEF_HEADING: &str = "Brief:";
const NOTES_HEADING: &str = "Source notes:";
const DRAFT_HEADING: &str = "Draft:";
const OBJECTIONS_HEADING: &str = "Objections to fix:";
const REPLY_CUE: &str = "The corrected draft:";

fn objection_lines(objections: &[Objection]) -> String {
    objections
        .iter()
        .enumerate()
        .map(|(at, objection)| {
            let mut line = format!("{}. {}", at + 1, objection.kind.as_str());
            if !objection.about.is_empty() {
                line.push_str(&format!(": {}", objection.about));
            }
            if !objection.fix.is_empty() {
                line.push_str(&format!("\n   Fix: {}", objection.fix));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The reviser's user message within `budget_chars`. The draft and the objections are whole
/// whatever the budget; the brief, then the notes, fill what is left.
pub fn build_revise_message(
    brief: &str,
    draft: &str,
    objections: &[Objection],
    notes: &[SourceNote<'_>],
    budget_chars: usize,
) -> String {
    let objections = objection_lines(objections);
    let mut room = Room::new(budget_chars.saturating_sub(chars(REPLY_CUE)));
    room.keep(DRAFT_HEADING, draft);
    room.keep(OBJECTIONS_HEADING, &objections);
    let brief = room.text(BRIEF_HEADING, brief);
    let notes = fit_notes(notes, &mut room);

    let mut out = String::new();
    section(&mut out, BRIEF_HEADING, &brief);
    section(&mut out, NOTES_HEADING, &notes);
    section(&mut out, DRAFT_HEADING, draft);
    section(&mut out, OBJECTIONS_HEADING, &objections);
    out.push_str(REPLY_CUE);
    out
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// The reply without a sentence that only announces it, such as "Here is the corrected draft:".
fn without_lead_in<'a>(reply: &'a str, draft: &str) -> &'a str {
    let Some((lead, rest)) = reply.split_once('\n') else {
        return reply;
    };
    let lead = lead.trim();
    let is_markdown = lead.starts_with(|c: char| {
        matches!(c, '#' | '-' | '*' | '|' | '>' | '`' | '{' | '[') || c.is_ascii_digit()
    });
    let announces = lead.ends_with(':') && !is_markdown && lead != first_line(draft);
    if announces && !rest.trim().is_empty() {
        rest.trim_start()
    } else {
        reply
    }
}

/// The reply without a code fence wrapped round the whole of it.
fn without_fence<'a>(reply: &'a str, draft: &str) -> &'a str {
    if !reply.starts_with("```") || draft.trim_start().starts_with("```") {
        return reply;
    }
    let Some((_, inside)) = reply.split_once('\n') else {
        return reply;
    };
    let mut end = inside.len();
    let mut offset = 0;
    for line in inside.split_inclusive('\n') {
        if line.trim() == "```" {
            end = offset;
        }
        offset += line.len();
    }
    inside.split_at(end).0
}

/// The corrected draft a reviser wrote, without the wrapping a small model adds. `None` when
/// the reply is under a third of `draft`: the model gave up or summarised.
pub fn clean_revision(raw: &str, draft: &str) -> Option<String> {
    let reply = strip_reasoning(raw);
    let reply = without_lead_in(reply.trim(), draft);
    let reply = without_fence(reply, draft).trim();
    (!reply.is_empty() && chars(reply) * 3 >= chars(draft.trim())).then(|| reply.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::ObjectionKind;

    const DRAFT: &str = "# AI Summit Nairobi\n\nThe keynote is at 9am in the Tsavo ballroom.\n\n\
                         ## Getting there\n\nTake the expressway and park at KICC.";

    fn objections() -> Vec<Objection> {
        vec![
            Objection::new(
                ObjectionKind::Grounding,
                "the keynote time",
                "cite the programme",
            ),
            Objection::new(ObjectionKind::Vague, "", ""),
        ]
    }

    #[test]
    fn the_message_ends_with_the_draft_and_what_to_fix_in_it() {
        let notes = [SourceNote {
            title: "Programme",
            body: "Keynote 10am, Tsavo ballroom.",
        }];
        let message = build_revise_message("A dossier.", DRAFT, &objections(), &notes, 5_000);
        let at = |needle: &str| {
            message
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle}: {message}"))
        };
        let order = [
            at("Brief:\nA dossier."),
            at("Source notes:\n[1] Programme\nKeynote 10am"),
            at("Draft:\n# AI Summit Nairobi"),
            at("Objections to fix:"),
            at("1. grounding: the keynote time\n   Fix: cite the programme"),
            at("2. vague"),
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{message}");
        assert!(message.ends_with("The corrected draft:"));
    }

    #[test]
    fn a_tight_budget_cuts_the_notes_then_the_brief_and_never_the_draft_or_the_objections() {
        let notes = [
            SourceNote {
                title: "Programme",
                body: "Keynote 10am, Tsavo ballroom.",
            },
            SourceNote {
                title: "Venue",
                body: "KICC, Harambee Avenue. Doors open at 8am for everyone.",
            },
        ];
        let whole = chars(&build_revise_message(
            "A dossier.",
            DRAFT,
            &objections(),
            &notes,
            5_000,
        ));
        let tight = build_revise_message("A dossier.", DRAFT, &objections(), &notes, whole - 80);
        assert!(tight.contains("Brief:\nA dossier."), "{tight}");
        assert!(!tight.contains("[2] Venue"), "{tight}");

        let none = build_revise_message("A dossier.", DRAFT, &objections(), &notes, 0);
        assert!(none.contains(DRAFT));
        assert!(none.contains("1. grounding: the keynote time"));
        assert!(!none.contains("Brief:"));
        assert!(!none.contains("Source notes:"));
    }

    #[test]
    fn a_plain_revision_is_taken_as_it_is() {
        let revised = DRAFT.replace("9am", "10am");
        assert_eq!(clean_revision(&revised, DRAFT), Some(revised.clone()));
        assert_eq!(
            clean_revision(&format!("<think>fix the time</think>\n{revised}\n"), DRAFT),
            Some(revised)
        );
    }

    #[test]
    fn a_wrapping_fence_and_a_line_that_announces_the_reply_are_stripped() {
        let revised = DRAFT.replace("9am", "10am");
        for wrapped in [
            format!("```markdown\n{revised}\n```"),
            format!("```\n{revised}\n```\n"),
            format!("Here is the corrected draft:\n\n{revised}"),
            format!("Sure, the corrected document follows:\n```markdown\n{revised}\n```"),
            format!("```markdown\n{revised}\n```\nLet me know if you want more changes."),
            format!("```markdown\n{revised}"),
        ] {
            assert_eq!(
                clean_revision(&wrapped, DRAFT).as_deref(),
                Some(revised.as_str()),
                "{wrapped}"
            );
        }
    }

    #[test]
    fn a_documents_own_first_line_and_inner_fences_are_kept() {
        let draft = "Costs at a glance:\n\n| Item | KSh |\n|---|---|\n| Duty | 420,000 |\n";
        assert_eq!(clean_revision(draft, draft).as_deref(), Some(draft.trim()));

        let with_code = "# Setup\n\n```sh\nmake install\n```\n\nThen restart the pond.";
        assert_eq!(
            clean_revision(with_code, with_code).as_deref(),
            Some(with_code)
        );
        let wrapped = format!("```markdown\n{with_code}\n```");
        assert_eq!(
            clean_revision(&wrapped, with_code).as_deref(),
            Some(with_code)
        );
    }

    #[test]
    fn json_drafts_come_back_as_json() {
        let draft = r#"{"description": "A boarding pass to Kisumu", "intent": "a trip"}"#;
        let reply = format!("```json\n{draft}\n```");
        assert_eq!(clean_revision(&reply, draft).as_deref(), Some(draft));
    }

    #[test]
    fn a_reply_under_a_third_of_the_draft_is_refused() {
        assert_eq!(clean_revision("The summit is on 3 December.", DRAFT), None);
        assert_eq!(clean_revision("", DRAFT), None);
        assert_eq!(clean_revision("```\n```", DRAFT), None);
        let third: String = DRAFT.chars().take(chars(DRAFT).div_ceil(3)).collect();
        assert!(clean_revision(&third, DRAFT).is_some());
    }

    #[test]
    fn the_system_prompt_asks_for_the_draft_itself() {
        assert!(REVISE_SYSTEM.contains("whole corrected draft and nothing else"));
        assert!(REVISE_SYSTEM.contains("Never invent"));
        assert!(!REVISE_SYSTEM.contains("{\""), "no JSON shape to copy");
    }
}
