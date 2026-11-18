//! The pure pieces of settling a job: the draft a deliverable takes, what a refresh changed, the
//! line the log keeps and the notice the member gets.

use std::collections::HashSet;

use super::critic::Verdict;
use super::{chars, one_line, CHANGED_HEADING, NO_CHANGE};
use crate::user_data::domain::anchor::{
    Anchor, AnchorKind, Deliverable, DeliverableDraft, DeliverableState, SourceLink,
};
use crate::user_data::domain::engine_job::{EngineJob, JobState};
use crate::user_data::domain::interest::topic_key;

/// Longest hook, in characters: one line under a title on a phone.
pub const HOOK_CHARS: usize = 160;
/// A full stop this early ends an abbreviation, not a sentence.
const MIN_SENTENCE_CHARS: usize = 24;
const MAX_SOURCES: usize = 40;
const NOTICE_CHARS: usize = 240;
const OUTCOME_ABOUT_CHARS: usize = 60;
const OUTCOME_OBJECTIONS: usize = 3;

enum Piece<'a> {
    Text(&'a str),
    Link {
        text: &'a str,
        url: &'a str,
        image: bool,
    },
}

/// Markdown split into its `[text](url)` links and the text between them.
fn pieces(text: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(mid) = rest.find("](") {
        let (head, tail) = rest.split_at(mid);
        let after = tail.strip_prefix("](").unwrap_or(tail);
        let open = head
            .rfind('[')
            .filter(|&open| !head.split_at(open).1.contains('\n'));
        let Some(open) = open else {
            out.push(Piece::Text(head));
            out.push(Piece::Text("]("));
            rest = after;
            continue;
        };
        let (before, label) = head.split_at(open);
        out.push(Piece::Text(before.strip_suffix('!').unwrap_or(before)));
        let (target, remaining) = after.split_at(after.find(')').unwrap_or(after.len()));
        out.push(Piece::Link {
            text: label.strip_prefix('[').unwrap_or(label),
            url: target.split_whitespace().next().unwrap_or(""),
            image: before.ends_with('!'),
        });
        rest = remaining.strip_prefix(')').unwrap_or(remaining);
    }
    out.push(Piece::Text(rest));
    out
}

/// Markdown as plain words on one line: a link as its text, emphasis marks dropped.
fn plain(text: &str) -> String {
    let words: String = pieces(text)
        .iter()
        .map(|piece| match piece {
            Piece::Text(text) | Piece::Link { text, .. } => *text,
        })
        .collect();
    one_line(&words.replace(['*', '`'], ""))
}

fn is_web(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// The web pages a Markdown body links to, in order, once each.
pub fn links_in(body: &str) -> Vec<SourceLink> {
    let mut seen = HashSet::new();
    pieces(body)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Link {
                text,
                url,
                image: false,
            } if is_web(url) && seen.insert(url) => Some(SourceLink {
                title: plain(text),
                url: url.to_string(),
            }),
            _ => None,
        })
        .collect()
}

fn shorten(text: &str, max: usize) -> String {
    if chars(text) <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(3)).collect();
    let kept = match cut.rfind(char::is_whitespace) {
        Some(at) => cut.split_at(at).0,
        None => cut.as_str(),
    };
    format!("{}...", kept.trim_end())
}

fn first_sentence(text: &str) -> &str {
    for (seen, (at, c)) in text.char_indices().enumerate() {
        if !matches!(c, '.' | '!' | '?') || seen + 1 < MIN_SENTENCE_CHARS {
            continue;
        }
        let (sentence, rest) = text.split_at(at + c.len_utf8());
        // "8 a.m. on weekdays": a stop followed by a small letter is inside the sentence.
        let ends = rest.is_empty()
            || (rest.starts_with(char::is_whitespace)
                && !rest.trim_start().starts_with(char::is_lowercase));
        if ends {
            return sentence;
        }
    }
    text
}

fn list_item(line: &str) -> Option<&str> {
    let marked = ["- ", "* ", "+ "]
        .iter()
        .find_map(|mark| line.strip_prefix(mark));
    marked.or_else(|| {
        let (number, rest) = line.split_once(". ")?;
        (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit())).then_some(rest)
    })
}

/// The first plain sentence of a Markdown body: its first paragraph's, else its first list
/// item's, else its first heading.
fn hook_of(body: &str) -> String {
    let mut in_code = false;
    let mut paragraph = String::new();
    let mut item: Option<String> = None;
    let mut heading: Option<String> = None;
    for line in body.lines().map(str::trim).chain([""]) {
        if line.starts_with("```") {
            in_code = !in_code;
        }
        let rule = !line.is_empty() && line.chars().all(|c| matches!(c, '-' | '*' | '=' | '_'));
        let prose = !(in_code
            || rule
            || line.is_empty()
            || line.starts_with(['#', '|', '>', '`'])
            || list_item(line).is_some());
        if prose {
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(line);
            continue;
        }
        let said = plain(&paragraph);
        paragraph.clear();
        // A line that ends in a colon only introduces what follows it.
        if !said.is_empty() && !said.ends_with(':') {
            return shorten(first_sentence(&said), HOOK_CHARS);
        }
        if let Some(text) = list_item(line) {
            item.get_or_insert_with(|| plain(text));
        } else if line.starts_with('#') {
            heading.get_or_insert_with(|| plain(line.trim_start_matches('#')));
        }
    }
    let fallback = item.or(heading).unwrap_or_default();
    shorten(first_sentence(&fallback), HOOK_CHARS)
}

/// What a deliverable takes from its job's result: the body, a hook from its first sentence,
/// and `sources` followed by the pages the body links. Pass a refresh's body after
/// [`split_changed`].
pub fn draft_from_result(body: &str, sources: Vec<SourceLink>) -> DeliverableDraft {
    let body = body.trim();
    let mut seen = HashSet::new();
    let sources = sources
        .into_iter()
        .map(|source| SourceLink {
            title: one_line(&source.title),
            url: source.url.trim().to_string(),
        })
        .chain(links_in(body))
        .filter(|source| is_web(&source.url) && seen.insert(source.url.clone()))
        .take(MAX_SOURCES)
        .collect();
    DeliverableDraft {
        hook: hook_of(body),
        body: body.to_string(),
        facts: Vec::new(),
        sources,
    }
}

fn is_heading(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// A refreshed document as what it says changed, and the document without that section.
/// Nothing changed when it has no such section at its top, or the section says so.
pub fn split_changed(body: &str) -> (String, String) {
    let lines: Vec<&str> = body.lines().collect();
    let wanted = topic_key(CHANGED_HEADING);
    let mut title_seen = false;
    let mut start = None;
    for (at, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if topic_key(line) == wanted {
            start = Some(at);
        } else if is_heading(line) && !title_seen {
            title_seen = true;
            continue;
        }
        break;
    }
    let Some(start) = start else {
        return (String::new(), body.trim().to_string());
    };
    let after = start + 1;
    let next_heading = lines.iter().skip(after).position(|line| is_heading(line));
    let end = match next_heading {
        Some(offset) => after + offset,
        // No heading follows: the section is its first paragraph, not the whole document.
        None => {
            let first = lines
                .iter()
                .skip(after)
                .position(|line| !line.trim().is_empty())
                .map_or(lines.len(), |offset| after + offset);
            lines
                .iter()
                .skip(first)
                .position(|line| line.trim().is_empty())
                .map_or(lines.len(), |offset| first + offset)
        }
    };
    let (head, rest) = lines.split_at(start);
    let (section, tail) = rest.split_at(end - start);
    let changed = section
        .iter()
        .skip(1)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let changed = changed.trim();
    let said_nothing = [NO_CHANGE, "No changes."]
        .iter()
        .any(|none| topic_key(changed) == topic_key(none));
    let changed = if said_nothing { "" } else { changed };
    let head = head.join("\n");
    let tail = tail.join("\n");
    let body = [head.trim(), tail.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (changed.to_string(), body)
}

fn checks(rounds: u32) -> String {
    match rounds {
        0 => "its check".to_string(),
        1 => "one check".to_string(),
        n => format!("{n} checks"),
    }
}

/// One plain line for the job log: how the job ended and what the critic still held.
pub fn job_outcome_line(job: &EngineJob, verdict: Option<&Verdict>) -> String {
    if verdict.is_some_and(|verdict| verdict.pass) {
        return match job.rounds {
            0 | 1 => "Passed its first check.".to_string(),
            n => format!("Passed after {n} checks."),
        };
    }
    let ended = match job.kind.settles_without_a_pass_as() {
        JobState::Unsure => "Shown unsure",
        _ => "Dropped",
    };
    let Some(verdict) = verdict else {
        return format!("{ended}: no check finished in its time.");
    };
    if verdict.is_unread() {
        return format!(
            "{ended} after {}: the last check could not be read.",
            checks(job.rounds)
        );
    }
    let mut held: Vec<String> = verdict
        .objections
        .iter()
        .take(OUTCOME_OBJECTIONS)
        .map(|objection| {
            let about = shorten(&objection.about, OUTCOME_ABOUT_CHARS);
            if about.is_empty() {
                objection.kind.as_str().to_string()
            } else {
                format!("{} ({about})", objection.kind.as_str())
            }
        })
        .collect();
    let more = verdict.objections.len().saturating_sub(held.len());
    if more > 0 {
        held.push(format!("{more} more"));
    }
    format!(
        "{ended} after {}; still held: {}.",
        checks(job.rounds),
        held.join(", ")
    )
}

/// The title and body of the notice a member gets when a deliverable is shown to them: what it
/// is, and one specific from it.
pub fn ready_notice(anchor: &Anchor, deliverable: &Deliverable) -> (String, String) {
    let refreshed = deliverable.version > 1;
    let lead = match deliverable.state {
        DeliverableState::Unsure => "Ready, not fully checked",
        _ if refreshed => "Updated",
        _ => "Ready",
    };
    let title = format!("{lead}: {}", deliverable.title);

    let changed = deliverable
        .changed
        .lines()
        .map(|line| plain(list_item(line.trim()).unwrap_or(line)))
        .find(|line| !line.is_empty())
        .filter(|_| refreshed);
    let mut body = changed.unwrap_or_else(|| one_line(&deliverable.hook));
    if body.is_empty() {
        body = match anchor.kind {
            AnchorKind::Standing => "Ready to read.".to_string(),
            _ => format!("Made for {}.", anchor.title),
        };
    }
    if deliverable.state == DeliverableState::Unsure {
        let doubt = match deliverable.objections.len() {
            0 => "Goose could not finish checking it.".to_string(),
            1 => "One point in it is not confirmed.".to_string(),
            n => format!("{n} points in it are not confirmed."),
        };
        body = format!(
            "{} {doubt}",
            shorten(&body, NOTICE_CHARS - chars(&doubt) - 1)
        );
    }
    (title, shorten(&body, NOTICE_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::{
        AnchorOrigin, DeliverableKind, Objection, ObjectionKind,
    };
    use crate::user_data::domain::engine_job::{JobClass, JobKind};
    use chrono::{DateTime, TimeZone, Utc};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn link(title: &str, url: &str) -> SourceLink {
        SourceLink {
            title: title.into(),
            url: url.into(),
        }
    }

    const DOSSIER: &str = "# Dossier for AI Summit Nairobi\n\n\
        **Three sessions fit you**, led by the [on-device track](https://summit.example/track) \
        at 11am. The rest is below.\n\n\
        ## Getting there\n\n\
        - Take the expressway; see the [venue map](https://kicc.example/map \"KICC\").\n\
        - ![logo](https://summit.example/logo.png)\n";

    #[test]
    fn the_hook_is_the_first_plain_sentence_of_the_body() {
        let draft = draft_from_result(DOSSIER, Vec::new());
        assert_eq!(
            draft.hook,
            "Three sessions fit you, led by the on-device track at 11am."
        );
        assert_eq!(draft.body, DOSSIER.trim());
        assert!(draft.facts.is_empty());
    }

    #[test]
    fn a_hook_skips_what_is_not_a_sentence_and_falls_back_to_a_list_item_or_the_heading() {
        let tabled = "# Costs\n\n| Item | KSh |\n|---|---|\n| Duty | 420,000 |\n\n---\n\n\
                      > quoted\n\n```\ncode. not prose.\n```\n\nIn short:\n\n\
                      Dr. Otieno's clinic is the nearest one, and it opens at 8 a.m. on weekdays.";
        assert_eq!(
            hook_of(tabled),
            "Dr. Otieno's clinic is the nearest one, and it opens at 8 a.m. on weekdays."
        );
        assert_eq!(
            hook_of("# Packing\n\n- Passport and the yellow fever card\n- A jacket"),
            "Passport and the yellow fever card"
        );
        assert_eq!(hook_of("## Out this week\n\n| a | b |"), "Out this week");
        assert_eq!(
            hook_of("1. First the visa. Then the flights."),
            "First the visa. Then the flights."
        );
        assert_eq!(hook_of("   \n"), "");
    }

    #[test]
    fn a_wrapped_paragraph_is_read_as_one_and_a_long_sentence_is_clipped_at_a_word() {
        assert_eq!(
            hook_of("The summit runs for\nthree days in December. Book early."),
            "The summit runs for three days in December."
        );
        let long = format!(
            "The plan covers {}and ends there.",
            "one more stop ".repeat(20)
        );
        let hook = hook_of(&long);
        assert!(chars(&hook) <= HOOK_CHARS, "{hook}");
        assert!(hook.ends_with("stop..."), "{hook}");
    }

    #[test]
    fn sources_are_the_ones_given_then_the_pages_the_body_links_once_each() {
        let given = vec![
            link("Programme", "https://summit.example/programme"),
            link("  The on-device\ntrack ", "https://summit.example/track"),
            link("A note", "artifact-7"),
        ];
        let draft = draft_from_result(DOSSIER, given);
        assert_eq!(
            draft.sources,
            [
                link("Programme", "https://summit.example/programme"),
                link("The on-device track", "https://summit.example/track"),
                link("venue map", "https://kicc.example/map"),
            ],
            "no image, no link twice, nothing that is not a web page"
        );
        assert_eq!(
            links_in("See [the **programme**](https://a.example/p) and [x](mailto:a@b.c)."),
            [link("the programme", "https://a.example/p")]
        );
        assert!(links_in("no links ]( here").is_empty());
    }

    #[test]
    fn what_changed_is_split_from_the_top_of_a_refreshed_document() {
        let refreshed = "# Dossier for AI Summit Nairobi\n\n## What changed\n\n\
                         - The keynote moved to 10am.\n- Two speakers were added.\n\n\
                         ## Agenda\n\nThe keynote is at 10am.";
        let (changed, body) = split_changed(refreshed);
        assert_eq!(
            changed,
            "- The keynote moved to 10am.\n- Two speakers were added."
        );
        assert_eq!(
            body,
            "# Dossier for AI Summit Nairobi\n\n## Agenda\n\nThe keynote is at 10am."
        );

        for heading in ["**What changed**", "What changed:", "### what changed"] {
            let (changed, body) = split_changed(&format!(
                "{heading}\nThe venue is new.\n\nThe plan follows."
            ));
            assert_eq!(changed, "The venue is new.", "{heading}");
            assert_eq!(body, "The plan follows.", "{heading}");
        }
    }

    #[test]
    fn a_refresh_that_changed_nothing_says_nothing() {
        for said in ["No change.", "no change", "No changes."] {
            let (changed, body) = split_changed(&format!(
                "## What changed\n\n{said}\n\n## Agenda\n\nAs before."
            ));
            assert_eq!(changed, "", "{said}");
            assert_eq!(body, "## Agenda\n\nAs before.");
        }
        let plain = "# Dossier\n\nThe keynote is at 10am.\n\n## What changed\n\nToo late here.";
        assert_eq!(
            split_changed(plain),
            (String::new(), plain.to_string()),
            "only a section at the top counts"
        );
        assert_eq!(split_changed("  "), (String::new(), String::new()));
        assert_eq!(
            split_changed("## What changed"),
            (String::new(), String::new())
        );
    }

    fn job(kind: JobKind, rounds: u32) -> EngineJob {
        let mut job = EngineJob::new(kind, JobClass::Deadline, Some("jerry"), "a job", now());
        job.rounds = rounds;
        job
    }

    #[test]
    fn the_log_line_says_how_the_job_ended_and_what_was_still_held() {
        let pass = Verdict {
            pass: true,
            objections: Vec::new(),
        };
        assert_eq!(
            job_outcome_line(&job(JobKind::Deliverable, 1), Some(&pass)),
            "Passed its first check."
        );
        assert_eq!(
            job_outcome_line(&job(JobKind::FactSheet, 3), Some(&pass)),
            "Passed after 3 checks."
        );

        let fail = Verdict {
            pass: false,
            objections: vec![
                Objection::new(ObjectionKind::Grounding, "the keynote time", "cite it"),
                Objection::new(ObjectionKind::Vague, "", "name days"),
                Objection::new(ObjectionKind::Stale, "the 2025 dates", "use 2026"),
                Objection::new(ObjectionKind::Duplicate, "the venue", "say it once"),
            ],
        };
        assert_eq!(
            job_outcome_line(&job(JobKind::Deliverable, 3), Some(&fail)),
            "Shown unsure after 3 checks; still held: grounding (the keynote time), vague, \
             stale (the 2025 dates), 1 more."
        );
        assert!(job_outcome_line(&job(JobKind::Rule, 1), Some(&fail))
            .starts_with("Dropped after one check; still held: grounding"));
        assert_eq!(
            job_outcome_line(&job(JobKind::Questions, 2), Some(&Verdict::unread())),
            "Dropped after 2 checks: the last check could not be read."
        );
        assert_eq!(
            job_outcome_line(&job(JobKind::Deliverable, 0), None),
            "Shown unsure: no check finished in its time."
        );
        assert!(!job_outcome_line(&job(JobKind::Deliverable, 3), Some(&fail)).contains('\n'));
    }

    fn summit() -> Anchor {
        Anchor::new(
            "jerry",
            AnchorKind::Event,
            "AI Summit Nairobi",
            AnchorOrigin::You,
            now(),
        )
        .unwrap()
    }

    fn dossier(anchor: &Anchor) -> Deliverable {
        let mut made = Deliverable::new(
            &anchor.id,
            DeliverableKind::Dossier,
            "dossier",
            "Dossier for AI Summit Nairobi",
            now(),
        )
        .unwrap();
        made.state = DeliverableState::Ready;
        made.version = 1;
        made.hook = "Three sessions fit you, led by the on-device track at 11am.".into();
        made
    }

    #[test]
    fn a_ready_notice_says_what_it_is_and_one_specific_with_no_ids() {
        let anchor = summit();
        let made = dossier(&anchor);
        let (title, body) = ready_notice(&anchor, &made);
        assert_eq!(title, "Ready: Dossier for AI Summit Nairobi");
        assert_eq!(
            body,
            "Three sessions fit you, led by the on-device track at 11am."
        );
        for text in [&title, &body] {
            assert!(!text.contains(&anchor.id) && !text.contains(&made.id));
        }

        let mut bare = made.clone();
        bare.hook.clear();
        assert_eq!(
            ready_notice(&anchor, &bare).1,
            "Made for AI Summit Nairobi."
        );
    }

    #[test]
    fn a_refresh_leads_with_what_changed_and_an_unsure_one_says_it_is_not_confirmed() {
        let anchor = summit();
        let mut refreshed = dossier(&anchor);
        refreshed.version = 2;
        refreshed.changed = "- The keynote moved to **10am**.\n- Two speakers were added.".into();
        assert_eq!(
            ready_notice(&anchor, &refreshed),
            (
                "Updated: Dossier for AI Summit Nairobi".to_string(),
                "The keynote moved to 10am.".to_string()
            )
        );

        let mut unsure = dossier(&anchor);
        unsure.state = DeliverableState::Unsure;
        unsure.objections = vec![
            Objection::new(ObjectionKind::Grounding, "the keynote time", "cite it"),
            Objection::new(ObjectionKind::Vague, "getting there", "name the route"),
        ];
        let (title, body) = ready_notice(&anchor, &unsure);
        assert_eq!(
            title,
            "Ready, not fully checked: Dossier for AI Summit Nairobi"
        );
        assert_eq!(
            body,
            "Three sessions fit you, led by the on-device track at 11am. 2 points in it are not \
             confirmed."
        );

        unsure.hook = "word ".repeat(200);
        let (_, long) = ready_notice(&anchor, &unsure);
        assert!(chars(&long) <= NOTICE_CHARS, "{long}");
        assert!(
            long.ends_with("... 2 points in it are not confirmed."),
            "{long}"
        );
    }
}
