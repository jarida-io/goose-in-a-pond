//! The member's portrait: the background model's short account of who they are and what they are
//! into, rewritten on the inference lane once a day or after their interests change.

use chrono::{DateTime, Duration, Utc};

use crate::user_data::domain::interest::{Interest, MemberPortrait, CATEGORIES};
use crate::user_data::domain::memory::MemoryFragment;
use crate::user_data::services::model_json::{object_in, text_of};

/// How often a portrait is rewritten when nothing about the member's interests changed.
pub const PORTRAIT_EVERY: Duration = Duration::hours(24);
/// The least time between two rewrites, however often the interests change.
pub const PORTRAIT_FLOOR: Duration = Duration::hours(1);
/// Memories the prompt shows, by importance and recency.
pub const PORTRAIT_MEMORIES: usize = 25;
/// Recent conversation summaries the prompt shows.
pub const PORTRAIT_SUMMARIES: usize = 6;
const MAX_ITEMS: usize = 10;
const MAX_ITEM_CHARS: usize = 80;

/// Whether the member's portrait is due: none yet, a day old, or older than an interests change.
pub fn portrait_is_due(
    portrait: Option<&MemberPortrait>,
    interests_changed: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    let Some(portrait) = portrait else {
        return true;
    };
    let age = now - portrait.updated_at;
    if age >= PORTRAIT_EVERY {
        return true;
    }
    age >= PORTRAIT_FLOOR && interests_changed.is_some_and(|at| at > portrait.updated_at)
}

/// What the suggestion and idea prompts are told about a member: the portrait, then the
/// interests that feed research. `None` when nothing is known yet.
pub fn about_member(portrait: Option<&MemberPortrait>, interests: &[Interest]) -> Option<String> {
    let mut out = String::new();
    if let Some(portrait) = portrait {
        out.push_str(&portrait.as_prompt_lines());
    }
    let followed: Vec<String> = interests
        .iter()
        .filter(|i| i.feeds_research())
        .take(12)
        .map(|i| {
            if i.facets.is_empty() {
                i.topic.clone()
            } else {
                format!("{} ({})", i.topic, i.facets.join(", "))
            }
        })
        .collect();
    if !followed.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("Confirmed interests: {}", followed.join("; ")));
    }
    let avoided: Vec<&str> = interests
        .iter()
        .filter(|i| i.avoid)
        .map(|i| i.topic.as_str())
        .collect();
    if !avoided.is_empty() {
        out.push_str(&format!("\nNever raise: {}", avoided.join("; ")));
    }
    (!out.trim().is_empty()).then_some(out)
}

/// The memories worth a portrait: most important first, newer first within a level.
pub fn top_memories(mut memories: Vec<MemoryFragment>, limit: usize) -> Vec<MemoryFragment> {
    memories.sort_by(|a, b| {
        let importance = |m: &MemoryFragment| (m.importance.unwrap_or(0.5) * 10.0).round() as i32;
        importance(b)
            .cmp(&importance(a))
            .then(b.created_at.cmp(&a.created_at))
    });
    memories.truncate(limit);
    memories
}

const SYSTEM: &str = "\
You write the Pond's working portrait of {name}, one member of the household. \
The Pond's news, ideas and suggestions read this portrait instead of every \
memory, so it must be specific to {name} and true to the sources.

Use only these sources, given below: {name}'s confirmed interests, the \
interests their conversations keep showing (with how many conversations show \
each), their memories (most important first), and summaries of their recent \
conversations. One question asked once is curiosity, not an interest: never \
build on something only one conversation shows.

Reply with JSON only, exactly this shape:
{\"summary\":\"...\",\"into\":[{\"item\":\"...\",\"score\":0.0}],\
\"working_on\":[{\"item\":\"...\",\"score\":0.0}],\"likes\":[{\"item\":\"...\",\
\"score\":0.0}]}

- summary: two or three plain sentences in the third person, naming {name}: \
who they are, and what they are doing and following now.
- into: up to 10 subjects {name} follows, each by its own specific name: a \
team, a title, a person's work, a field, a technique. Granular and technical, \
never a category word like {categories}.
- working_on: up to 10 things {name} is doing or preparing now: projects, \
purchases, events, plans, each with the detail that makes it specific (a \
budget, a date, a place, a name).
- likes: up to 10 tastes and ways {name} likes things done: styles, foods, \
colours, formats.
- item: a short phrase, under 80 characters.
- score: 0 to 1, how strongly the sources show it: confirmed or shown in \
several conversations scores high, a memory once mentioned scores low.
- Prioritise what is current over what is past, and confirmed over guessed. \
Sort each list by score, highest first.
- Never: the assistant itself, a famous person's life story, anything the \
sources do not say.";

pub fn render_system(name: &str) -> String {
    // Category words as the prompt's "never" list, so `into` stays specific.
    SYSTEM
        .replace(
            "{categories}",
            &CATEGORIES
                .iter()
                .filter(|c| **c != "other")
                .take(6)
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(" or "),
        )
        .replace("{name}", name)
}

/// The sources, as the prompt reads them.
pub fn build_user_message(
    confirmed: &[Interest],
    guesses: &[Interest],
    memories: &[MemoryFragment],
    summaries: &[String],
) -> String {
    let mut out = String::from("Confirmed interests:\n");
    if confirmed.is_empty() {
        out.push_str("None yet.\n");
    }
    for i in confirmed {
        out.push_str(&format!("- {} ({}", i.topic, i.category));
        if !i.facets.is_empty() {
            out.push_str(&format!("; {}", i.facets.join(", ")));
        }
        out.push_str(&format!("; {})\n", i.depth.as_str()));
    }
    out.push_str("\nInterests their conversations keep showing:\n");
    if guesses.is_empty() {
        out.push_str("None yet.\n");
    }
    for i in guesses {
        out.push_str(&format!("- {} ({}", i.topic, i.category));
        if !i.facets.is_empty() {
            out.push_str(&format!("; {}", i.facets.join(", ")));
        }
        out.push_str(&format!("; {} conversations)\n", i.evidence));
    }
    out.push_str("\nMemories, most important first:\n");
    if memories.is_empty() {
        out.push_str("None yet.\n");
    }
    for m in memories {
        out.push_str(&format!("- {}\n", m.content.trim()));
    }
    out.push_str("\nRecent conversations:\n");
    if summaries.is_empty() {
        out.push_str("None yet.\n");
    }
    for s in summaries {
        out.push_str(&format!("- {}\n", s.trim()));
    }
    out.push_str("\nJSON:");
    out
}

/// A list of `{item, score}` (or bare strings), sorted by score, cleaned and bounded.
fn scored_items(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    let Some(items) = map.get(key).and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut scored: Vec<(f64, String)> = items
        .iter()
        .filter_map(|item| {
            let (text, score) = match item {
                serde_json::Value::String(s) => (s.clone(), 0.5),
                serde_json::Value::Object(o) => (
                    o.get("item")
                        .or_else(|| o.get("topic"))
                        .and_then(|t| t.as_str())?
                        .to_string(),
                    o.get("score").and_then(|s| s.as_f64()).unwrap_or(0.5),
                ),
                _ => return None,
            };
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (!text.is_empty() && text != "..." && text.chars().count() <= MAX_ITEM_CHARS)
                .then_some((score, text))
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut seen = std::collections::HashSet::new();
    scored
        .into_iter()
        .map(|(_, text)| text)
        .filter(|text| seen.insert(text.to_lowercase()))
        .take(MAX_ITEMS)
        .collect()
}

/// `None` when the reply holds no usable summary: the old portrait stays.
pub fn parse_reply(raw: &str, now: DateTime<Utc>) -> Option<MemberPortrait> {
    let map = object_in(raw)?;
    let summary = text_of(&map, "summary")?;
    if summary.chars().count() < 20 {
        return None;
    }
    Some(MemberPortrait {
        summary,
        into: scored_items(&map, "into"),
        working_on: scored_items(&map, "working_on"),
        likes: scored_items(&map, "likes"),
        updated_at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portrait_at(at: DateTime<Utc>) -> MemberPortrait {
        MemberPortrait {
            summary: "x".into(),
            into: vec![],
            working_on: vec![],
            likes: vec![],
            updated_at: at,
        }
    }

    #[test]
    fn a_portrait_is_due_daily_or_after_the_interests_change_but_not_more_than_hourly() {
        let now = Utc::now();
        assert!(portrait_is_due(None, None, now));
        let fresh = portrait_at(now - Duration::minutes(10));
        assert!(!portrait_is_due(Some(&fresh), Some(now), now), "the floor holds");
        let hour_old = portrait_at(now - Duration::minutes(90));
        assert!(portrait_is_due(Some(&hour_old), Some(now), now));
        assert!(!portrait_is_due(
            Some(&hour_old),
            Some(now - Duration::hours(3)),
            now
        ));
        let day_old = portrait_at(now - Duration::hours(25));
        assert!(portrait_is_due(Some(&day_old), None, now));
    }

    #[test]
    fn the_lists_come_back_sorted_by_score_without_scores() {
        let raw = r#"{"summary":"Jerry runs a company that builds an offline home AI.",
            "into":[{"item":"GTA V","score":0.4},{"item":"The Boys","score":0.9},{"item":"...","score":1}],
            "working_on":["MCP Dev Summit Nairobi talk"],
            "likes":[{"item":"teal","score":0.7},{"item":"Teal","score":0.6}]}"#;
        let portrait = parse_reply(raw, Utc::now()).unwrap();
        assert_eq!(portrait.into, vec!["The Boys", "GTA V"]);
        assert_eq!(portrait.working_on, vec!["MCP Dev Summit Nairobi talk"]);
        assert_eq!(portrait.likes, vec!["teal"]);
    }

    #[test]
    fn a_reply_with_no_summary_keeps_the_old_portrait() {
        assert!(parse_reply(r#"{"summary":"","into":[]}"#, Utc::now()).is_none());
        assert!(parse_reply("I cannot help", Utc::now()).is_none());
    }

    #[test]
    fn the_member_is_described_by_portrait_and_research_worthy_interests_only() {
        use crate::user_data::domain::interest::{Depth, NewsMode, Origin};
        let row = |topic: &str, confirmed: bool, evidence: u32, avoid: bool| Interest {
            id: topic.into(),
            profile_id: "p".into(),
            topic: topic.into(),
            category: "other".into(),
            facets: vec![],
            depth: Depth::Keen,
            news: NewsMode::Both,
            avoid,
            origin: Origin::Chat,
            evidence,
            confirmed,
            picture: None,
            updated_at: Utc::now(),
        };
        let about = about_member(
            Some(&portrait_at(Utc::now())),
            &[
                row("The Boys", true, 1, false),
                row("Primetime", false, 1, false),
                row("Celebrity gossip", true, 1, true),
            ],
        )
        .unwrap();
        assert!(about.contains("Confirmed interests: The Boys"));
        assert!(!about.contains("Primetime"), "one conversation is curiosity");
        assert!(about.contains("Never raise: Celebrity gossip"));
        assert_eq!(about_member(None, &[]), None);
    }

    #[test]
    fn the_prompt_names_its_sources_and_asks_for_scored_sorted_items() {
        let prompt = render_system("Jerry");
        assert!(prompt.contains("Use only these sources"));
        assert!(prompt.contains("Sort each list by score"));
        assert!(prompt.contains("One question asked once is curiosity"));
        assert!(!prompt.contains("{name}") && !prompt.contains("{categories}"));
    }
}
