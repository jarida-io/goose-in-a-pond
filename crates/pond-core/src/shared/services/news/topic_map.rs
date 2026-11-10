//! A member's topic map: about forty scored web-search topics written from their interests,
//! portrait, memories and scheduled tasks, in batches by area. The news searches its newsworthy
//! topics; ideas and long reads take the rest.

use std::collections::HashMap;

use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::coverage::queries_for;
use crate::models::services::thought_filter::strip_reasoning;
use crate::shared::domain::news::{InterestRef, InterestRow, Portrait, Story, Topic, Vote};

/// Topics a map holds, about.
pub const TARGET_TOPICS: usize = 40;
/// Topics one subject may have. Gemma pads a large count by repeating one subject, so the code
/// holds the line the prompt asks for.
pub const MAX_PER_SUBJECT: usize = 4;
const MIN_AREAS: usize = 3;
const MAX_AREAS: usize = 7;
const MIN_PER_AREA: usize = 3;
const MAX_PER_AREA: usize = 12;
/// Memories a map is written from at most, the most important first.
pub const MAP_MEMORIES: usize = 25;
/// Scheduled tasks a map is written from at most.
pub const MAP_ROUTINES: usize = 10;
/// A change to the interests is let settle this long before the map is rebuilt for it.
pub const INTERESTS_SETTLE: chrono::Duration = chrono::Duration::minutes(10);
const MAX_QUERY_WORDS: usize = 10;
const MAX_FIELD_CHARS: usize = 120;

/// Everything a map is written from.
#[derive(Debug, Clone, Default)]
pub struct MapSources {
    pub name: String,
    pub today: String,
    /// The interests the news covers.
    pub interests: Vec<InterestRow>,
    /// Topics the member asked never to hear about.
    pub avoid: Vec<String>,
    pub portrait: Option<Portrait>,
    /// The member's memories, most important first.
    pub memories: Vec<String>,
    /// The prompts of the household's scheduled tasks, which show the member's routines.
    pub routines: Vec<String>,
    /// Stories the member voted up or down lately.
    pub votes: Vec<Story>,
}

impl MapSources {
    /// The sources as both prompts quote them.
    pub fn block(&self) -> String {
        let mut out = format!("Member: {}\nToday: {}\n", self.name, self.today);
        out.push_str("\nINTERESTS (confirmed by them, or seen in at least three conversations):\n");
        for (i, interest) in self.interests.iter().enumerate() {
            out.push_str(&format!(
                "{}. {} [{}; {}]",
                i + 1,
                interest.topic.trim(),
                interest.category,
                interest.depth
            ));
            if !interest.facets.is_empty() {
                out.push_str(&format!(" - follows: {}", interest.facets.join(", ")));
            }
            out.push('\n');
        }
        if !self.avoid.is_empty() {
            out.push_str(&format!(
                "\nAVOID (never raise these): {}\n",
                self.avoid.join("; ")
            ));
        }
        if let Some(p) = &self.portrait {
            out.push_str("\nPORTRAIT (the pond's account of them):\n");
            if !p.summary.trim().is_empty() {
                out.push_str(p.summary.trim());
                out.push('\n');
            }
            for (label, items) in [
                ("Into", &p.into),
                ("Working on", &p.working_on),
                ("Likes", &p.likes),
            ] {
                if !items.is_empty() {
                    out.push_str(&format!("{label}: {}\n", items.join("; ")));
                }
            }
        }
        if !self.memories.is_empty() {
            out.push_str("\nMEMORIES (most important first):\n");
            for memory in &self.memories {
                out.push_str(&format!("- {}\n", one_line(memory)));
            }
        }
        if !self.routines.is_empty() {
            out.push_str("\nSCHEDULED TASKS (their routines):\n");
            for routine in &self.routines {
                out.push_str(&format!("- {}\n", one_line(routine)));
            }
        }
        if !self.votes.is_empty() {
            out.push_str(
                "\nVOTES ON RECENT STORIES (score what they liked higher, what they disliked \
                 lower or not at all):\n",
            );
            for story in &self.votes {
                let vote = match story.vote {
                    Some(Vote::Up) => "liked",
                    Some(Vote::Down) => "disliked",
                    _ => continue,
                };
                out.push_str(&format!(
                    "- {vote}: \"{}\" [{}]",
                    story.headline, story.interest.topic
                ));
                if let Some(query) = &story.found_by {
                    out.push_str(&format!(" found by \"{query}\""));
                }
                out.push('\n');
            }
        }
        out.trim_end().to_string()
    }

    /// What the topics prompt prioritises, from this member's own data: what they are working
    /// on, then what they are devoted to, then what they are keen on.
    pub fn priorities(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = &self.portrait {
            if !p.working_on.is_empty() {
                parts.push(format!(
                    "what they are working on ({})",
                    p.working_on.join(", ")
                ));
            }
        }
        for (depth, label) in [
            ("devoted", "what they are devoted to"),
            ("keen", "what they are keen on"),
        ] {
            let names: Vec<&str> = self
                .interests
                .iter()
                .filter(|i| i.depth == depth)
                .map(|i| i.topic.trim())
                .collect();
            if !names.is_empty() {
                parts.push(format!("{label} ({})", names.join(", ")));
            }
        }
        if parts.is_empty() {
            "what the sources show they keep coming back to".to_string()
        } else {
            parts.join(", then ")
        }
    }
}

/// The areas prompt's system text.
pub fn areas_system(target: usize) -> String {
    format!(
        "You map out what one household member should be kept informed about: the first step of \
their personal news. Use only the sources given: their INTERESTS, the PORTRAIT, their MEMORIES and \
their SCHEDULED TASKS.

Reply with a JSON array and nothing else:
[{{\"area\": \"...\", \"score\": 0, \"topics\": 0, \"why\": \"...\"}}]

- area: one part of THIS member's life and interests, in 2 to 5 words specific to them, derived \
from the sources (\"Kenyan used-car market\", not \"Cars\"; \"MCP and on-device AI\", not \
\"Technology\"). No generic newspaper sections.
- score: 0 to 100, how much they care about this area, from how often and how deeply the sources \
show it, how recent it is, and the portrait.
- topics: how many web-search topics this area deserves, {MIN_PER_AREA} to {MAX_PER_AREA}; all \
areas together about {target}.
- why: one line naming the sources that show it.

Give {MIN_AREAS} to {MAX_AREAS} areas. Prioritise what they keep coming back to and what they are \
working on now. A scheduled task is evidence for the subjects it names, never an area of its own. \
One question asked once is curiosity, not an interest: a memory that only says they asked or \
inquired about something is never an area. Name each area after its subjects, not after a memory's \
wording. An area is something the public web reports on: never their own company's finances or \
staff, their family, or what the pond already tells them another way (weather, their calendar). \
Leave out everything under AVOID. Sort descending by score. JSON only."
    )
}

pub fn areas_prompt(sources: &MapSources) -> String {
    sources.block()
}

/// One area of the map, as the first stage wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct Area {
    pub name: String,
    pub score: f64,
    pub topics: usize,
}

#[derive(Deserialize)]
struct RawArea {
    #[serde(default)]
    area: Option<String>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    topics: Option<f64>,
}

/// The areas in `reply`, best first, their topic counts scaled to about `target` together.
pub fn parse_areas(reply: &str, target: usize) -> Vec<Area> {
    let mut areas: Vec<Area> = json_array(reply)
        .into_iter()
        .filter_map(|v| serde_json::from_value::<RawArea>(v).ok())
        .filter_map(|raw| {
            let name = one_line(raw.area.as_deref()?);
            (!name.is_empty() && name.chars().count() <= MAX_FIELD_CHARS).then(|| Area {
                name,
                score: raw.score.unwrap_or(50.0).clamp(0.0, 100.0),
                topics: raw.topics.unwrap_or(0.0).max(0.0) as usize,
            })
        })
        .collect();
    let mut seen: Vec<String> = Vec::new();
    areas.retain(|a| {
        let key = a.name.to_lowercase();
        let new = !seen.contains(&key);
        seen.push(key);
        new
    });
    areas.sort_by(|a, b| b.score.total_cmp(&a.score));
    areas.truncate(MAX_AREAS);
    let asked: usize = areas.iter().map(|a| a.topics.max(1)).sum();
    for area in &mut areas {
        let share = (area.topics.max(1) * target) as f64 / asked.max(1) as f64;
        area.topics = (share.round() as usize).clamp(MIN_PER_AREA, MAX_PER_AREA);
    }
    areas
}

/// The topics prompt's system text, for `n` topics in one area.
pub fn topics_system(n: usize, priorities: &str) -> String {
    format!(
        "You write web-search topics for one area of a household member's personal news map. Use \
only the sources given: their INTERESTS, the PORTRAIT, their MEMORIES and their SCHEDULED TASKS.

Reply with a JSON array of {n} objects and nothing else:
[{{\"subject\": \"...\", \"query\": \"...\", \"newsworthy\": true, \"findable\": true, \
\"score\": 0, \"why\": \"...\"}}]

- subject: the precise thing, as a proper noun or technical term: a person, product, team, \
place, event, technology or standard (\"Toyota Fielder\", \"on-device inference\", \"P_vs_NP\").
- query: a specific string optimised for web search: 3 to 8 words, the names and technical terms \
a good article would use. Not a question.
- newsworthy: true when the subject has news that changes week to week (releases, prices, \
results, events, statements, episodes); false when it is evergreen and better served by a \
deeper read.
- findable: false when the public web cannot answer it: their own company's finances or staff, \
their family, their private plans.
- score: 0 to 100, how much they care, from the evidence in the sources, the depth of their \
interest, how recent it is, and the portrait.
- why: one line naming the source that shows they care.

Be highly granular and technical: one named subject per object, never a whole field (not \
\"superhero news\" or \"AI best practices\"). At most {MAX_PER_SUBJECT} objects may share a \
subject; cover more subjects instead. A newsworthy query names the subject and the kind of news \
(results, prices, releases, statements, events), never \"guide\" or \"best practices\". Never put a \
season, version, model or year number in a query unless the sources give it: what you know of \
the latest may be out of date. Prioritise {priorities}. One question asked once is curiosity, not \
an interest: a memory that only says they asked or inquired about something gives no topic. \
Leave out everything under AVOID. Sort descending by score. JSON only."
    )
}

pub fn topics_prompt(sources: &MapSources, area: &Area, taken: &[String]) -> String {
    let mut text = format!("{}\n\nAREA: {}", sources.block(), area.name);
    if !taken.is_empty() {
        text.push_str(&format!(
            "\nAlready covered in other areas, do not repeat: {}",
            taken.join("; ")
        ));
    }
    text
}

/// One topic as the second stage wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct RawTopic {
    pub subject: String,
    pub query: String,
    pub newsworthy: bool,
    pub findable: bool,
    pub score: f64,
    pub why: String,
}

#[derive(Deserialize)]
struct RawTopicJson {
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    websearch_query: Option<String>,
    #[serde(default)]
    newsworthy: Option<Value>,
    #[serde(default)]
    findable: Option<Value>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    why: Option<String>,
}

fn truthy(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.trim().to_lowercase().as_str(), "true" | "yes"),
        _ => default,
    }
}

/// The topics in `reply`; one missing a subject or a query is dropped.
pub fn parse_topics(reply: &str) -> Vec<RawTopic> {
    json_array(reply)
        .into_iter()
        .filter_map(|v| serde_json::from_value::<RawTopicJson>(v).ok())
        .filter_map(|raw| {
            let subject = one_line(raw.subject.as_deref()?);
            let query = one_line(raw.query.as_deref().or(raw.websearch_query.as_deref())?);
            let query: String = query
                .split_whitespace()
                .take(MAX_QUERY_WORDS)
                .collect::<Vec<_>>()
                .join(" ")
                .trim_end_matches('?')
                .to_string();
            if subject.is_empty() || query.is_empty() || subject.chars().count() > MAX_FIELD_CHARS {
                return None;
            }
            Some(RawTopic {
                subject,
                query,
                newsworthy: truthy(raw.newsworthy.as_ref(), true),
                findable: truthy(raw.findable.as_ref(), true),
                score: raw.score.unwrap_or(0.0).clamp(0.0, 100.0),
                why: one_line(raw.why.as_deref().unwrap_or_default())
                    .chars()
                    .take(MAX_FIELD_CHARS * 2)
                    .collect(),
            })
        })
        .collect()
}

/// A subject as one comparable key: "P vs NP", "p_vs_np" and "P-vs-NP" are one subject.
pub fn subject_key(subject: &str) -> String {
    subject
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The map, from each area's topics: what the public web cannot answer and what the member
/// avoids dropped, a query asked twice kept once, at most [`MAX_PER_SUBJECT`] per subject,
/// best first, about [`TARGET_TOPICS`] in all.
pub fn clean(
    member: &str,
    by_area: Vec<(String, Vec<RawTopic>)>,
    avoid: &[String],
    built_at: DateTime<Utc>,
) -> Vec<Topic> {
    let avoid: Vec<String> = avoid
        .iter()
        .map(|a| subject_key(a))
        .filter(|a| !a.is_empty())
        .collect();
    let mut all: Vec<(String, RawTopic)> = by_area
        .into_iter()
        .flat_map(|(area, topics)| topics.into_iter().map(move |t| (area.clone(), t)))
        .filter(|(_, t)| t.findable)
        .filter(|(area, t)| {
            let text = subject_key(&format!("{area} {} {}", t.subject, t.query));
            !avoid.iter().any(|a| contains_phrase(&text, a))
        })
        .collect();
    all.sort_by(|a, b| b.1.score.total_cmp(&a.1.score));
    let mut per_subject: HashMap<String, usize> = HashMap::new();
    let mut queries: Vec<String> = Vec::new();
    let mut kept = Vec::new();
    for (area, t) in all {
        let query_key = subject_key(&t.query);
        if queries.contains(&query_key) {
            continue;
        }
        let count = per_subject.entry(subject_key(&t.subject)).or_default();
        if *count >= MAX_PER_SUBJECT {
            continue;
        }
        *count += 1;
        queries.push(query_key);
        kept.push(Topic {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: member.to_string(),
            area,
            subject: t.subject,
            query: t.query,
            newsworthy: t.newsworthy,
            score: t.score,
            why: t.why,
            built_at,
            used_at: None,
        });
        if kept.len() >= TARGET_TOPICS + TARGET_TOPICS / 4 {
            break;
        }
    }
    kept
}

/// Points a vote moves a topic under the voted story's interest, and one with its query.
const INTEREST_VOTE_POINTS: f64 = 8.0;
const QUERY_VOTE_POINTS: f64 = 15.0;

/// The member's votes applied to the map: a topic under a liked interest or with a liked query
/// scores higher, a disliked one lower; a query disliked twice, or a topic brought to nothing,
/// is dropped. Best first again after.
pub fn apply_votes(topics: Vec<Topic>, votes: &[Story], interests: &[InterestRow]) -> Vec<Topic> {
    let weight = |s: &Story| match s.vote {
        Some(Vote::Up) => 1.0,
        Some(Vote::Down) => -1.0,
        _ => 0.0,
    };
    let mut kept: Vec<Topic> = topics
        .into_iter()
        .filter_map(|mut topic| {
            let group = route(&topic, interests).map(|r| r.interest.id);
            let by_interest: f64 = votes
                .iter()
                .filter(|s| Some(&s.interest.id) == group.as_ref())
                .map(weight)
                .sum();
            let by_query: f64 = votes
                .iter()
                .filter(|s| {
                    s.found_by
                        .as_deref()
                        .is_some_and(|q| subject_key(q) == subject_key(&topic.query))
                })
                .map(weight)
                .sum();
            if by_query <= -2.0 {
                return None;
            }
            topic.score =
                (topic.score + INTEREST_VOTE_POINTS * by_interest + QUERY_VOTE_POINTS * by_query)
                    .clamp(0.0, 100.0);
            (topic.score > 0.0).then_some(topic)
        })
        .collect();
    kept.sort_by(|a, b| b.score.total_cmp(&a.score));
    kept
}

/// A map made from the interests alone, for when the model wrote none: each interest's searches,
/// scored by depth.
pub fn fallback_topics(
    member: &str,
    interests: &[InterestRow],
    built_at: DateTime<Utc>,
) -> Vec<Topic> {
    interests
        .iter()
        .flat_map(|interest| {
            let score = match interest.depth.as_str() {
                "devoted" => 90.0,
                "keen" => 70.0,
                _ => 50.0,
            };
            let count = interest.facets.len().max(1);
            let queries: Vec<String> = (0..count)
                .flat_map(|turn| queries_for(interest, turn))
                .collect();
            let mut unique: Vec<String> = Vec::new();
            for q in queries {
                if !unique.contains(&q) {
                    unique.push(q);
                }
            }
            unique
                .into_iter()
                .take(MAX_PER_SUBJECT)
                .map(|query| Topic {
                    id: uuid::Uuid::new_v4().to_string(),
                    profile_id: member.to_string(),
                    area: interest.topic.clone(),
                    subject: interest.topic.clone(),
                    query,
                    newsworthy: true,
                    score,
                    why: "from their interests".to_string(),
                    built_at,
                    used_at: None,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Where a topic's stories go: under which interest, and whether into the tiles and the digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub interest: InterestRef,
    pub tiles: bool,
    pub digest: bool,
}

/// The interest a topic serves: the one whose topic or a facet it names, else the one it shares
/// most words with; a topic serving none goes under its area, into both. A topic under an
/// interest the member avoids or switched off goes nowhere.
pub fn route(topic: &Topic, all_interests: &[InterestRow]) -> Option<Route> {
    let text = subject_key(&format!("{} {} {}", topic.area, topic.subject, topic.query));
    let named = |i: &InterestRow| {
        std::iter::once(&i.topic)
            .chain(i.facets.iter())
            .map(|p| subject_key(p))
            .filter(|p| !p.is_empty())
            .any(|p| contains_phrase(&text, &p))
    };
    let overlap = |i: &InterestRow| {
        let words: Vec<String> = std::iter::once(&i.topic)
            .chain(i.facets.iter())
            .flat_map(|p| significant_words(p))
            .collect();
        let mine = significant_words(&text);
        words.iter().filter(|w| mine.contains(w)).count()
    };
    let best = all_interests.iter().find(|i| named(i)).or_else(|| {
        all_interests
            .iter()
            .map(|i| (overlap(i), i))
            .filter(|(n, _)| *n >= 2)
            .max_by_key(|(n, _)| *n)
            .map(|(_, i)| i)
    });
    match best {
        Some(i) if i.avoid => None,
        Some(i) if !(i.news.tiles() || i.news.digest()) => None,
        Some(i) => Some(Route {
            interest: InterestRef {
                id: i.id.clone(),
                topic: i.topic.clone(),
            },
            tiles: i.news.tiles(),
            digest: i.news.digest(),
        }),
        None => Some(Route {
            interest: InterestRef {
                id: format!("area:{}", subject_key(&topic.area).replace(' ', "-")),
                topic: topic.area.clone(),
            },
            tiles: true,
            digest: true,
        }),
    }
}

/// Whether the map should be rebuilt now: none yet, built on an earlier day of the member's, or
/// the interests changed since and have settled.
pub fn map_due<Tz: TimeZone>(
    now: DateTime<Utc>,
    zone: &Tz,
    built_at: Option<DateTime<Utc>>,
    interests_changed_at: Option<DateTime<Utc>>,
) -> bool {
    let Some(built) = built_at else {
        return true;
    };
    if built.with_timezone(zone).date_naive() < now.with_timezone(zone).date_naive() {
        return true;
    }
    interests_changed_at.is_some_and(|changed| changed > built && now - changed >= INTERESTS_SETTLE)
}

const COMMON: &[&str] = &[
    "the", "and", "for", "with", "news", "latest", "update", "updates", "new", "kenya", "kenyan",
    "nairobi", "2025", "2026", "2027",
];

fn significant_words(text: &str) -> Vec<String> {
    subject_key(text)
        .split(' ')
        .filter(|w| w.len() >= 3 && !COMMON.contains(w))
        .map(str::to_string)
        .collect()
}

fn contains_phrase(text: &str, phrase: &str) -> bool {
    format!(" {text} ").contains(&format!(" {phrase} "))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first JSON array in a model's answer, or the first array inside an object it wrote.
pub fn json_array(reply: &str) -> Vec<Value> {
    let text = strip_reasoning(reply);
    let array = text
        .find('[')
        .zip(text.rfind(']'))
        .filter(|(start, end)| end > start)
        .and_then(|(start, end)| serde_json::from_str::<Value>(&text[start..=end]).ok());
    if let Some(Value::Array(items)) = array {
        return items;
    }
    let object = text
        .find('{')
        .zip(text.rfind('}'))
        .filter(|(start, end)| end > start)
        .and_then(|(start, end)| serde_json::from_str::<Value>(&text[start..=end]).ok());
    match object {
        Some(Value::Object(map)) => map
            .into_iter()
            .find_map(|(_, v)| match v {
                Value::Array(items) => Some(items),
                _ => None,
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::news::NewsUse;

    fn raw(subject: &str, query: &str, score: f64) -> RawTopic {
        RawTopic {
            subject: subject.into(),
            query: query.into(),
            newsworthy: true,
            findable: true,
            score,
            why: "interests".into(),
        }
    }

    fn interest(id: &str, topic: &str, facets: &[&str], news: NewsUse) -> InterestRow {
        InterestRow {
            id: id.into(),
            topic: topic.into(),
            category: "other".into(),
            facets: facets.iter().map(|f| f.to_string()).collect(),
            depth: "keen".into(),
            news,
            avoid: false,
            evidence: 3,
            confirmed: true,
        }
    }

    #[test]
    fn a_padded_subject_is_cut_to_four_and_private_matters_go() {
        let mut dafoe: Vec<RawTopic> = (0..6)
            .map(|i| {
                raw(
                    "Willem Dafoe",
                    &format!("Willem Dafoe new film {i}"),
                    80.0 - i as f64,
                )
            })
            .collect();
        let mut private = raw("Jarida One revenue", "Jarida One Ltd revenue 2026", 99.0);
        private.findable = false;
        dafoe.push(private);
        dafoe.push(raw("willem-dafoe", "Willem Dafoe new film 0", 10.0));
        dafoe.push(raw("Gen V", "Gen V season 2 finale", 70.0));
        let map = clean(
            "m",
            vec![("Film and actors".into(), dafoe)],
            &[],
            Utc::now(),
        );
        let subjects: Vec<&str> = map.iter().map(|t| t.subject.as_str()).collect();
        assert_eq!(
            subjects,
            vec![
                "Willem Dafoe",
                "Willem Dafoe",
                "Willem Dafoe",
                "Willem Dafoe",
                "Gen V"
            ]
        );
        assert!(map.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn what_the_member_avoids_never_reaches_the_map() {
        let topics = vec![
            raw("Primetime", "Primetime thriller release", 90.0),
            raw("Gen V", "Gen V season 2", 60.0),
        ];
        let map = clean(
            "m",
            vec![("TV".into(), topics)],
            &["Primetime".into()],
            Utc::now(),
        );
        assert_eq!(map.len(), 1);
        assert_eq!(map[0].subject, "Gen V");
    }

    #[test]
    fn areas_are_scaled_to_the_target_and_sorted() {
        let reply = r#"Here: [{"area": "Kenyan used-car market", "score": 60, "topics": 10},
            {"area": "MCP and on-device AI", "score": 95, "topics": 20},
            {"area": "mcp and on-device ai", "score": 10, "topics": 5},
            {"area": "The Boys universe", "score": 70, "topics": 10}]"#;
        let areas = parse_areas(reply, 40);
        let names: Vec<&str> = areas.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "MCP and on-device AI",
                "The Boys universe",
                "Kenyan used-car market"
            ]
        );
        assert_eq!(areas[0].topics, 12);
        assert_eq!(areas[1].topics, 10);
    }

    #[test]
    fn topics_are_read_leniently() {
        let reply = r#"```json
        {"topics": [{"subject": "P vs NP", "websearch_query": "P vs NP proof claim?", "newsworthy": "false", "score": 140},
                    {"subject": "", "query": "nothing"}]}
        ```"#;
        let topics = parse_topics(reply);
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].query, "P vs NP proof claim");
        assert!(!topics[0].newsworthy);
        assert_eq!(topics[0].score, 100.0);
    }

    #[test]
    fn a_topic_goes_under_the_interest_it_names() {
        let interests = vec![
            interest("boys", "The Boys", &["Gen V"], NewsUse::Tiles),
            interest(
                "cars",
                "Kenyan used cars",
                &["Toyota Fielder"],
                NewsUse::Both,
            ),
            interest("pv", "P vs NP", &[], NewsUse::Off),
        ];
        let topic = |area: &str, subject: &str, query: &str| Topic {
            id: "t".into(),
            profile_id: "m".into(),
            area: area.into(),
            subject: subject.into(),
            query: query.into(),
            newsworthy: true,
            score: 50.0,
            why: String::new(),
            built_at: Utc::now(),
            used_at: None,
        };
        let gen_v = route(
            &topic("Superhero TV", "Gen V", "Gen V season 2 episode"),
            &interests,
        )
        .unwrap();
        assert_eq!(gen_v.interest.id, "boys");
        assert!(gen_v.tiles && !gen_v.digest);
        let fielder = route(
            &topic("Car buying", "Fielder prices", "Toyota Fielder price Jiji"),
            &interests,
        )
        .unwrap();
        assert_eq!(fielder.interest.id, "cars");
        assert_eq!(
            route(&topic("Theory", "P vs NP", "P vs NP proof"), &interests),
            None
        );
        let summit = route(
            &topic(
                "MCP Dev Summit",
                "MCP Dev Summit Nairobi",
                "MCP Dev Summit Nairobi speakers",
            ),
            &interests,
        )
        .unwrap();
        assert_eq!(summit.interest.topic, "MCP Dev Summit");
        assert!(summit.tiles && summit.digest);
    }

    #[test]
    fn votes_move_their_interest_and_their_query() {
        let interests = vec![
            interest("boys", "The Boys", &["Gen V"], NewsUse::Both),
            interest("cars", "Toyota Fielder", &[], NewsUse::Both),
        ];
        let topic = |subject: &str, query: &str, score: f64| Topic {
            id: query.into(),
            profile_id: "m".into(),
            area: "a".into(),
            subject: subject.into(),
            query: query.into(),
            newsworthy: true,
            score,
            why: String::new(),
            built_at: Utc::now(),
            used_at: None,
        };
        let story = |interest: &str, query: &str, vote: Vote| Story {
            id: "s".into(),
            profile_id: "m".into(),
            kind: crate::shared::domain::news::StoryKind::News,
            interest: InterestRef {
                id: interest.into(),
                topic: interest.into(),
            },
            headline: "h".into(),
            dek: String::new(),
            summary: String::new(),
            why_you: String::new(),
            url: "https://x.example".into(),
            site: "x".into(),
            source_title: String::new(),
            further_sources: vec![],
            picture_key: None,
            found_by: Some(query.into()),
            published_at: None,
            written_at: Utc::now(),
            read_at: None,
            vote: Some(vote),
        };
        let votes = vec![
            story("boys", "Gen V season 2", Vote::Up),
            story("cars", "Toyota Fielder price Jiji", Vote::Down),
            story("cars", "Toyota Fielder price Jiji", Vote::Down),
        ];
        let map = apply_votes(
            vec![
                topic("Gen V", "Gen V season 2", 50.0),
                topic("Toyota Fielder", "Toyota Fielder price Jiji", 90.0),
                topic("Toyota Fielder", "Toyota Fielder fuel economy", 60.0),
            ],
            &votes,
            &interests,
        );
        let scored: Vec<(&str, f64)> = map.iter().map(|t| (t.query.as_str(), t.score)).collect();
        assert_eq!(
            scored,
            vec![
                ("Gen V season 2", 73.0),
                ("Toyota Fielder fuel economy", 44.0)
            ]
        );
    }

    #[test]
    fn the_map_is_rebuilt_each_day_and_after_the_interests_settle() {
        let zone = chrono_tz::Africa::Nairobi;
        let built = Utc.with_ymd_and_hms(2026, 11, 9, 4, 0, 0).unwrap();
        assert!(map_due(built, &zone, None, None));
        assert!(!map_due(
            built + chrono::Duration::hours(10),
            &zone,
            Some(built),
            None
        ));
        assert!(map_due(
            built + chrono::Duration::hours(20),
            &zone,
            Some(built),
            None
        ));
        let changed = built + chrono::Duration::hours(2);
        assert!(!map_due(
            changed + chrono::Duration::minutes(5),
            &zone,
            Some(built),
            Some(changed)
        ));
        assert!(map_due(
            changed + chrono::Duration::minutes(11),
            &zone,
            Some(built),
            Some(changed)
        ));
    }

    #[test]
    fn the_sources_name_each_kind_and_the_priorities_come_from_the_member() {
        let mut mcp = interest("mcp", "MCP and on-device AI", &["Gemma"], NewsUse::Both);
        mcp.depth = "devoted".into();
        let sources = MapSources {
            name: "Jerry".into(),
            today: "Monday 9 November 2026".into(),
            interests: vec![mcp],
            avoid: vec!["Primetime".into()],
            portrait: Some(Portrait {
                summary: "Runs Jarida.".into(),
                working_on: vec!["Goose In A Pond".into()],
                ..Default::default()
            }),
            memories: vec!["Speaks at MCP Dev Summit Nairobi".into()],
            routines: vec!["Daily brief at 07:00".into()],
            votes: vec![],
        };
        let block = sources.block();
        for part in [
            "INTERESTS",
            "AVOID",
            "PORTRAIT",
            "MEMORIES",
            "SCHEDULED TASKS",
        ] {
            assert!(block.contains(part), "{part} missing");
        }
        assert_eq!(
            sources.priorities(),
            "what they are working on (Goose In A Pond), then what they are devoted to (MCP and on-device AI)"
        );
    }
}
