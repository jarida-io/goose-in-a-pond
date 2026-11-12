//! Ideas: which seeds a run researches, the prompts that turn a seed into a search and the pages
//! it finds into an idea, and reading the model's replies. Nothing here calls a model or the web.

use chrono::{DateTime, Duration, Utc};

use crate::user_data::domain::idea::{
    GooseCan, GooseKind, GoosePlan, Grounding, Idea, IdeaSource, IdeaState,
};
use crate::user_data::domain::interest::{topic_key, Interest, MemberPortrait};
use crate::user_data::ports::ideas::TopicSeed;
use crate::user_data::services::model_json::{object_in, text_of, texts_of};

/// How often a member's ideas are thought of when they do not ask.
pub const IDEAS_EVERY: Duration = Duration::hours(24);
/// How long a topic rests after an idea built on it.
pub const TOPIC_REST: Duration = Duration::days(7);
/// Seeds one run researches.
pub const SEEDS_PER_RUN: usize = 2;
/// Pages read per seed.
pub const PAGES_PER_SEED: usize = 3;
/// Characters of each page the idea prompt shows.
pub const PAGE_CHARS: usize = 4_000;
/// Below this self-score an idea is not worth the member's time.
const MIN_SCORE: f64 = 0.3;
const MAX_TITLE_CHARS: usize = 90;
const MAX_STEP_CHARS: usize = 100;
const MAX_STEPS: usize = 4;

/// Where a seed came from, so the run marks it used and the idea says what it builds on.
#[derive(Debug, Clone, PartialEq)]
pub enum SeedFrom {
    /// A topic-map row, by id.
    TopicMap(String),
    /// An interest row, by id.
    Interest(String),
    /// Something the portrait says the member is working on.
    WorkingOn,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IdeaSeed {
    pub from: SeedFrom,
    /// What the idea builds on, as the member would name it.
    pub label: String,
    /// The detail that makes it theirs: facets, the topic map's reason.
    pub detail: String,
    /// The topic map's own search, which needs no model call.
    pub query: Option<String>,
}

impl IdeaSeed {
    pub fn grounding(&self) -> Grounding {
        Grounding {
            kind: match self.from {
                SeedFrom::TopicMap(_) | SeedFrom::Interest(_) => "interest",
                SeedFrom::WorkingOn => "memory",
            }
            .to_string(),
            label: self.label.clone(),
        }
    }
}

/// Whether a member's ideas are due: they asked, or a day has passed since the last run.
pub fn ideas_are_due(
    requested_after_finish: bool,
    last_finished: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    requested_after_finish || last_finished.is_none_or(|at| now - at >= IDEAS_EVERY)
}

/// The seeds for one run: the topic map's best unused topic and something the member is working
/// on; with no topic map yet, what they are working on and their research-worthy interests. A
/// topic an idea built on in the last week rests.
pub fn pick_seeds(
    topic_map: &[TopicSeed],
    interests: &[Interest],
    portrait: Option<&MemberPortrait>,
    recently_used: &[String],
) -> Vec<IdeaSeed> {
    let rested: std::collections::HashSet<String> =
        recently_used.iter().map(|t| topic_key(t)).collect();
    let fresh = |label: &str| !rested.contains(&topic_key(label));

    let from_map = topic_map
        .iter()
        .filter(|t| fresh(&t.subject))
        .map(|t| IdeaSeed {
            from: SeedFrom::TopicMap(t.id.clone()),
            label: t.subject.clone(),
            detail: [t.area.as_str(), t.why.as_str()]
                .iter()
                .filter(|s| !s.trim().is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("; "),
            query: Some(t.query.clone()).filter(|q| !q.trim().is_empty()),
        });
    let working_on = portrait
        .map(|p| p.working_on.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|w| fresh(w))
        .map(|w| IdeaSeed {
            from: SeedFrom::WorkingOn,
            label: w,
            detail: String::new(),
            query: None,
        });
    let from_interests = interests
        .iter()
        .filter(|i| i.feeds_research() && fresh(&i.topic))
        .map(|i| IdeaSeed {
            from: SeedFrom::Interest(i.id.clone()),
            label: i.topic.clone(),
            detail: i.facets.join(", "),
            query: None,
        });

    let mut seeds: Vec<IdeaSeed> = if topic_map.is_empty() {
        let working: Vec<IdeaSeed> = working_on.collect();
        let interests: Vec<IdeaSeed> = from_interests.collect();
        // Alternate, so one run does not spend both seeds on one kind.
        let mut mixed = Vec::new();
        let (mut w, mut i) = (working.into_iter(), interests.into_iter());
        loop {
            let (a, b) = (w.next(), i.next());
            if a.is_none() && b.is_none() {
                break;
            }
            mixed.extend(a);
            mixed.extend(b);
        }
        mixed
    } else {
        let mut mixed: Vec<IdeaSeed> = from_map.take(1).collect();
        mixed.extend(working_on.take(1));
        mixed.extend(
            topic_map
                .iter()
                .filter(|t| fresh(&t.subject))
                .skip(1)
                .map(|t| IdeaSeed {
                    from: SeedFrom::TopicMap(t.id.clone()),
                    label: t.subject.clone(),
                    detail: t.why.clone(),
                    query: Some(t.query.clone()).filter(|q| !q.trim().is_empty()),
                }),
        );
        mixed
    };
    let mut seen = std::collections::HashSet::new();
    seeds.retain(|s| seen.insert(topic_key(&s.label)));
    seeds.truncate(SEEDS_PER_RUN);
    seeds
}

const QUERY_SYSTEM: &str = "\
You write one web search for {name}. Using the portrait of {name} and the \
seed below, write the search most likely to find something new, specific and \
useful that {name} could act on this week about the seed: an authentic recipe or \
culinary technique, an outdoor trail or adventure guide, a tool or library release, \
a hardware build, a tutorial, an event, or a concrete hands-on project guide.

Do NOT search for generic academic papers, theoretical surveys, textbook definitions, \
or broad science abstracts. Focus on practical guides, authentic recipes, trail profiles, \
concrete announcements, hands-on implementations, and actionable steps.

Reply with JSON only, exactly this shape:
{\"query\":\"...\",\"private\":false}

- query: 4 to 10 words, specific and technical: the exact names, libraries, \
hardware models, versions or terms that narrow it, with {name}'s city or country where \
it matters. No question mark, no quotes.
- private: true when the seed is a private matter the public web cannot \
answer, such as {name}'s own company finances, staff or health records; the \
query is then \"\".";

pub fn render_query_system(name: &str) -> String {
    QUERY_SYSTEM.replace("{name}", name)
}

pub fn build_query_message(portrait: Option<&MemberPortrait>, seed: &IdeaSeed) -> String {
    let mut out = String::from("Portrait:\n");
    match portrait {
        Some(p) => out.push_str(&p.as_prompt_lines()),
        None => out.push_str("None yet."),
    }
    out.push_str(&format!("\n\nSeed: {}", seed.label));
    if !seed.detail.is_empty() {
        out.push_str(&format!(" ({})", seed.detail));
    }
    out.push_str("\n\nJSON:");
    out
}

/// The search to run, or `None` for a private seed or an unusable reply.
pub fn parse_query(raw: &str) -> Option<String> {
    let map = object_in(raw)?;
    if map.get("private").and_then(|p| p.as_bool()) == Some(true) {
        return None;
    }
    let query = text_of(&map, "query")?;
    let query = query
        .trim_matches(|c: char| c == '"' || c == '?')
        .trim()
        .to_string();
    let words = query.split_whitespace().count();
    (2..=16).contains(&words).then_some(query)
}

/// One page read for a seed, as the idea prompt shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadPage {
    pub title: String,
    pub url: String,
    pub site: String,
    pub text: String,
}

const IDEA_SYSTEM: &str = "\
You write one practical, high-utility idea for {name} from pages the Pond just read for them.

Use only these sources, given below: the portrait of {name}, the seed (what the \
idea builds on), and the text of the numbered pages. Every fact in the idea \
must come from the pages.

A good idea is:
- rooted in {name}'s genuine personal interests across all areas of their life: culinary arts & recipes (e.g. authentic regional dishes like Nigerian Egusi Soup, pan-seared fillet mignon, artisan sourdough), outdoor adventures & travel (e.g. hiking trails like Mt. Longonot, cycling, gear packing), home, garden & DIY craft, or creative & technical projects;
- actionable and tangible: propose real-world steps that {name} can act on (e.g. buy ingredients, schedule trail dates, pack gear, configure, build, try);
- NEVER suggest merely reading an article, studying a paper, or learning abstract theory without practical action;
- new to {name}: not something the portrait shows they already know or do;
- specific to {name}: tied to the seed and to their details in the portrait \
(their places, interests, tech stack, hardware, dates, names);
- sourced: built on what the pages say;
- doable: something {name} or the Pond can act on soon. Never a question.
Keep your thinking concise, practical, and focused on tangible real-world utility.

Reply with JSON only, exactly this shape:
{\"title\":\"...\",\"pitch\":\"...\",\"why\":\"...\",\"sources\":[1],\
\"you_can\":[\"...\"],\"goose_can\":{\"kind\":\"research\",\"label\":\"...\",\
\"prompt\":\"...\",\"listing_url\":\"\"},\"score\":0.0}

- title: the idea as a concrete suggestion or project, under 80 characters, not a question.
- pitch: two or three sentences: what the pages say, with specific tools, ingredients, \
temperatures, trail numbers, benchmarks, or steps, and what practical task {name} could accomplish with it.
- why: one line to {name}, starting \"You\", naming the specific thing about \
them or their interests it builds on.
- sources: the numbers of the pages it rests on.
- you_can: two to four actionable steps {name} could take (e.g. buy ingredients, \
pack gear, schedule, clone, wire, deploy), each a short imperative under 80 characters.
- goose_can: what the Pond can do for {name}, or null. The Pond is an on-premise domestic hardware hub (Goose In A Pond) with live tools for smart home devices (Matter), hyper-local weather, calendars, household to-dos, and local research.
  - kind: \"todo\" (stage actionable prep steps like ingredients or gear on their to-do list), \"schedule\" (plan check-in dates, recipe prep times, or recurring reminders on their schedule), \"chat\" (discuss and customize the idea), \"research\" (conduct an autonomous deep research investigation for this household), or \"watch\" (monitor a web listings page for price drops or new items).
  - label: the button's action words, under 30 characters.
  - prompt: for research or schedule, what the Pond should look up or schedule; else \"\".
  - listing_url: for watch only, the address of a numbered page that lists \
items for sale or rent; else \"\".
- score: 0 to 1, how new, specific and doable this is for {name}.
If the pages hold nothing new and doable for {name}, reply {\"title\":\"\"}.";

pub fn render_idea_system(name: &str) -> String {
    IDEA_SYSTEM.replace("{name}", name)
}

pub fn build_idea_message(
    portrait: Option<&MemberPortrait>,
    seed: &IdeaSeed,
    pages: &[ReadPage],
) -> String {
    let mut out = String::from("Portrait:\n");
    match portrait {
        Some(p) => out.push_str(&p.as_prompt_lines()),
        None => out.push_str("None yet."),
    }
    out.push_str(&format!("\n\nSeed: {}", seed.label));
    if !seed.detail.is_empty() {
        out.push_str(&format!(" ({})", seed.detail));
    }
    out.push_str("\n\nPages:\n");
    for (i, page) in pages.iter().enumerate() {
        let text: String = page.text.chars().take(PAGE_CHARS).collect();
        out.push_str(&format!(
            "[{}] {} ({})\n{}\n\n",
            i + 1,
            page.title,
            page.url,
            text.trim()
        ));
    }
    out.push_str("JSON:");
    out
}

fn clipped(text: &str, max: usize) -> String {
    text.chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The idea the reply describes, or `None` when the model found nothing, wrote a question, or
/// scored it too low. Sources are the pages it names; a watch needs a page it read.
pub fn parse_idea(
    raw: &str,
    profile_id: &str,
    seed: &IdeaSeed,
    pages: &[ReadPage],
    now: DateTime<Utc>,
) -> Option<Idea> {
    let map = object_in(raw)?;
    let title = text_of(&map, "title")?;
    if title.ends_with('?') || title.chars().count() > MAX_TITLE_CHARS {
        return None;
    }
    let pitch = text_of(&map, "pitch")?;
    let score = map.get("score").and_then(|s| s.as_f64()).unwrap_or(0.5);
    if score < MIN_SCORE {
        return None;
    }
    let why = text_of(&map, "why").unwrap_or_else(|| format!("You follow {}.", seed.label));

    let named: Vec<usize> = map
        .get("sources")
        .and_then(|s| s.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|n| n.as_u64())
                .filter_map(|n| usize::try_from(n).ok())
                .filter(|n| (1..=pages.len()).contains(n))
                .collect()
        })
        .unwrap_or_default();
    // An idea always rests on pages: the ones it names, else every page it was written from.
    let cited: Vec<&ReadPage> = if named.is_empty() {
        pages.iter().collect()
    } else {
        let mut seen = std::collections::HashSet::new();
        named
            .into_iter()
            .filter(|n| seen.insert(*n))
            .map(|n| &pages[n - 1])
            .collect()
    };
    if cited.is_empty() {
        return None;
    }

    let you_can: Vec<String> = texts_of(&map, "you_can")
        .into_iter()
        .filter(|s| s.chars().count() <= MAX_STEP_CHARS && !s.ends_with('?'))
        .take(MAX_STEPS)
        .collect();

    let (goose_can, goose_plan) = goose_part(&map, pages, &title);

    Some(Idea {
        id: uuid::Uuid::new_v4().to_string(),
        profile_id: profile_id.to_string(),
        topic: seed.label.clone(),
        title,
        pitch,
        why,
        grounded_in: vec![seed.grounding()],
        sources: cited
            .into_iter()
            .map(|p| IdeaSource {
                title: clipped(&p.title, 120),
                url: p.url.clone(),
                site: p.site.clone(),
            })
            .collect(),
        picture: None,
        goose_can,
        goose_plan,
        you_can,
        state: IdeaState::New,
        progress: None,
        created_at: now,
    })
}

/// What the Pond may do about it. A watch needs a listings page it read; without one, the offer
/// becomes a research report on the idea.
fn goose_part(
    map: &serde_json::Map<String, serde_json::Value>,
    pages: &[ReadPage],
    title: &str,
) -> (Option<GooseCan>, GoosePlan) {
    let Some(part) = map.get("goose_can").and_then(|g| g.as_object()) else {
        return (None, GoosePlan::default());
    };
    let Some(mut kind) = text_of(part, "kind").and_then(|k| GooseKind::parse(&k)) else {
        return (None, GoosePlan::default());
    };
    let mut label = text_of(part, "label")
        .map(|l| clipped(&l, 40))
        .unwrap_or_default();
    let prompt = text_of(part, "prompt").map(|p| clipped(&p, 400));
    let listing_url =
        text_of(part, "listing_url").filter(|url| pages.iter().any(|p| &p.url == url));
    if kind == GooseKind::Watch && listing_url.is_none() {
        kind = GooseKind::Research;
        label = "Research it for me".to_string();
    }
    if label.is_empty() {
        label = match kind {
            GooseKind::Research => "Research it for me",
            GooseKind::Watch => "Watch the listings",
            GooseKind::Schedule => "Check weekly",
            GooseKind::Todo => "Add to my to-dos",
            GooseKind::Chat => "Talk it through",
        }
        .to_string();
    }
    let prompt = match kind {
        GooseKind::Research | GooseKind::Schedule => {
            Some(prompt.unwrap_or_else(|| title.to_string()))
        }
        _ => None,
    };
    (
        Some(GooseCan { label, kind }),
        GoosePlan {
            prompt,
            listing_url: if kind == GooseKind::Watch {
                listing_url
            } else {
                None
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::interest::{Depth, NewsMode, Origin};

    fn interest(id: &str, topic: &str, confirmed: bool, evidence: u32) -> Interest {
        Interest {
            id: id.into(),
            profile_id: "p".into(),
            topic: topic.into(),
            category: "cars".into(),
            facets: vec!["Kilimall".into()],
            depth: Depth::Keen,
            news: NewsMode::Both,
            avoid: false,
            origin: Origin::Chat,
            evidence,
            confirmed,
            picture: None,
            updated_at: Utc::now(),
        }
    }

    fn portrait(working_on: &[&str]) -> MemberPortrait {
        MemberPortrait {
            summary: "Jerry runs Jarida.".into(),
            into: vec![],
            working_on: working_on.iter().map(|w| w.to_string()).collect(),
            likes: vec![],
            updated_at: Utc::now(),
        }
    }

    fn topic(id: &str, subject: &str, score: f64) -> TopicSeed {
        TopicSeed {
            id: id.into(),
            area: "cars".into(),
            subject: subject.into(),
            query: format!("{subject} prices Nairobi"),
            score,
            why: "shopping".into(),
        }
    }

    #[test]
    fn the_topic_map_leads_with_what_the_member_is_working_on_beside_it() {
        let seeds = pick_seeds(
            &[
                topic("t1", "Toyota Fielder", 0.9),
                topic("t2", "P vs NP", 0.8),
            ],
            &[interest("i1", "GTA V", true, 1)],
            Some(&portrait(&["MCP Dev Summit Nairobi talk"])),
            &[],
        );
        assert_eq!(seeds.len(), 2);
        assert_eq!(seeds[0].from, SeedFrom::TopicMap("t1".into()));
        assert_eq!(
            seeds[0].query.as_deref(),
            Some("Toyota Fielder prices Nairobi")
        );
        assert_eq!(seeds[1].from, SeedFrom::WorkingOn);
    }

    #[test]
    fn with_no_topic_map_the_interests_and_portrait_seed_the_run() {
        let seeds = pick_seeds(
            &[],
            &[
                interest("i1", "Primetime", false, 1),
                interest("i2", "Toyota Fielder", false, 4),
            ],
            Some(&portrait(&["MCP Dev Summit Nairobi talk"])),
            &[],
        );
        let labels: Vec<&str> = seeds.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["MCP Dev Summit Nairobi talk", "Toyota Fielder"]
        );
        assert!(
            !labels.contains(&"Primetime"),
            "one conversation is curiosity, never a seed"
        );
    }

    #[test]
    fn a_topic_used_this_week_rests() {
        let seeds = pick_seeds(
            &[],
            &[interest("i2", "Toyota Fielder", true, 1)],
            Some(&portrait(&["MCP talk"])),
            &["toyota fielder".to_string(), "MCP talk".to_string()],
        );
        assert!(seeds.is_empty());
    }

    #[test]
    fn a_private_seed_gets_no_search() {
        assert_eq!(parse_query(r#"{"query":"","private":true}"#), None);
        assert_eq!(
            parse_query(r#"{"query":"Toyota Fielder price Kenya January","private":false}"#)
                .as_deref(),
            Some("Toyota Fielder price Kenya January")
        );
    }

    fn pages() -> Vec<ReadPage> {
        vec![
            ReadPage {
                title: "Used car prices in Kenya".into(),
                url: "https://example.com/prices".into(),
                site: "example.com".into(),
                text: "Prices soften in January.".into(),
            },
            ReadPage {
                title: "Toyota Fielder for sale".into(),
                url: "https://example.com/fielders".into(),
                site: "example.com".into(),
                text: "Listings.".into(),
            },
        ]
    }

    fn seed() -> IdeaSeed {
        IdeaSeed {
            from: SeedFrom::Interest("i2".into()),
            label: "Toyota Fielder".into(),
            detail: String::new(),
            query: None,
        }
    }

    #[test]
    fn an_idea_rests_on_the_pages_it_names_and_a_watch_on_a_page_it_read() {
        let raw = r#"{"title":"Catch the January dip on Fielders","pitch":"Prices soften after the import rush.",
            "why":"You are shopping for a Fielder under KSh 1.2M","sources":[2,2,9],
            "you_can":["Set a ceiling of KSh 1.1M","Ask for CVT service history"],
            "goose_can":{"kind":"watch","label":"Watch Fielder listings","prompt":"","listing_url":"https://example.com/fielders"},
            "score":0.8}"#;
        let idea = parse_idea(raw, "p", &seed(), &pages(), Utc::now()).unwrap();
        assert_eq!(idea.sources.len(), 1);
        assert_eq!(idea.sources[0].url, "https://example.com/fielders");
        assert_eq!(idea.goose_can.unwrap().kind, GooseKind::Watch);
        assert_eq!(
            idea.goose_plan.listing_url.as_deref(),
            Some("https://example.com/fielders")
        );
        assert_eq!(idea.grounded_in[0].label, "Toyota Fielder");
        assert_eq!(idea.you_can.len(), 2);
    }

    #[test]
    fn a_watch_on_a_page_never_read_becomes_research() {
        let raw = r#"{"title":"Catch the dip","pitch":"Prices soften.","why":"You shop.","sources":[1],
            "you_can":[],"goose_can":{"kind":"watch","label":"Watch","prompt":"","listing_url":"https://elsewhere.com/x"},"score":0.7}"#;
        let idea = parse_idea(raw, "p", &seed(), &pages(), Utc::now()).unwrap();
        assert_eq!(idea.goose_can.unwrap().kind, GooseKind::Research);
        assert_eq!(idea.goose_plan.listing_url, None);
        assert_eq!(idea.goose_plan.prompt.as_deref(), Some("Catch the dip"));
    }

    #[test]
    fn nothing_found_a_question_or_a_weak_idea_is_no_idea() {
        for raw in [
            r#"{"title":""}"#,
            r#"{"title":"Should you buy a Fielder?","pitch":"x","score":0.9}"#,
            r#"{"title":"A thing","pitch":"x","score":0.1}"#,
            "no json",
        ] {
            assert!(
                parse_idea(raw, "p", &seed(), &pages(), Utc::now()).is_none(),
                "{raw}"
            );
        }
    }

    #[test]
    fn ideas_are_due_daily_or_when_asked() {
        let now = Utc::now();
        assert!(ideas_are_due(false, None, now));
        assert!(!ideas_are_due(false, Some(now - Duration::hours(2)), now));
        assert!(ideas_are_due(true, Some(now - Duration::hours(2)), now));
        assert!(ideas_are_due(false, Some(now - Duration::hours(25)), now));
    }

    #[test]
    fn the_prompts_name_their_sources_and_define_every_field() {
        let idea = render_idea_system("Jerry");
        assert!(idea.contains("Use only these sources"));
        for field in [
            "title:",
            "pitch:",
            "why:",
            "sources:",
            "you_can:",
            "goose_can:",
            "score:",
        ] {
            assert!(idea.contains(field), "{field}");
        }
        let query = render_query_system("Jerry");
        assert!(query.contains("private matter the public web cannot"));
    }
}
