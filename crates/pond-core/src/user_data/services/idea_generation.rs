//! Ideas: which seeds a run researches, the prompts that turn a seed into a search and the pages
//! it finds into an idea, and reading the model's replies. Nothing here calls a model or the web.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};

use crate::user_data::domain::idea::{
    GooseCan, GooseKind, GoosePlan, Grounding, Idea, IdeaFact, IdeaKind, IdeaSource, IdeaState,
};
use crate::user_data::domain::interest::{topic_key, Interest, MemberPortrait};
use crate::user_data::domain::member_profile::About;
use crate::user_data::ports::ideas::TopicSeed;
use crate::user_data::services::model_json::{object_in, text_of, texts_of};

/// How often a member's ideas are thought of when they do not ask.
pub const IDEAS_EVERY: Duration = Duration::hours(24);
/// How long a topic rests after an idea built on it.
pub const TOPIC_REST: Duration = Duration::days(7);
/// How long a kind of everyday idea (a recipe, an outing, a date) rests after one was written.
pub const KIND_REST: Duration = Duration::days(3);
/// Seeds one run researches from the member's interests and work, beside one everyday idea.
pub const SEEDS_PER_RUN: usize = 2;
/// Pages read per seed.
pub const PAGES_PER_SEED: usize = 3;
/// Characters of each page the idea prompt shows.
pub const PAGE_CHARS: usize = 4_000;
/// Below this self-score an idea is not worth the member's time.
const MIN_SCORE: f64 = 0.3;
const MAX_TITLE_CHARS: usize = 90;
const MAX_HOOK_CHARS: usize = 140;
const MAX_STEP_CHARS: usize = 100;
const MAX_STEPS: usize = 4;
const MAX_FACTS: usize = 4;
const MAX_NOTES_CHARS: usize = 1_500;
/// Profile facts one prompt shows.
const PROFILE_LINES: usize = 16;

/// What the Pond knows of the member that an idea has to fit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IdeaProfile {
    /// Where the household is, as its settings name it.
    pub place: Option<String>,
    /// The member's live profile facts.
    pub facts: Vec<(About, String)>,
}

/// The facts an idea is written against, the ones that rule things out first.
const FITTING: [About; 15] = [
    About::Allergy,
    About::Diet,
    About::Health,
    About::Instruction,
    About::Dislikes,
    About::Partner,
    About::Child,
    About::Pet,
    About::Family,
    About::Home,
    About::Place,
    About::Likes,
    About::Routine,
    About::Goal,
    About::Now,
];

impl IdeaProfile {
    fn has(&self, about: About) -> bool {
        self.facts.iter().any(|(word, _)| *word == about)
    }

    /// The facts as prompt lines; empty when the Pond knows none that bear on an idea.
    pub fn as_prompt_lines(&self) -> String {
        FITTING
            .iter()
            .flat_map(|about| {
                self.facts
                    .iter()
                    .filter(move |(word, _)| word == about)
                    .map(|(_, fact)| format!("- {fact}"))
            })
            .take(PROFILE_LINES)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// What the member is allergic to, as lowercase word stems an idea must not name.
    pub fn allergens(&self) -> Vec<String> {
        self.facts
            .iter()
            .filter(|(about, _)| *about == About::Allergy)
            .filter_map(|(_, fact)| {
                let lower = fact.to_lowercase();
                let at = lower.find("allergic to ")?;
                Some(lower[at + "allergic to ".len()..].to_string())
            })
            .flat_map(|list| {
                list.trim_end_matches(['.', '!'])
                    .replace(" and ", ",")
                    .replace(" or ", ",")
                    .split(',')
                    .map(|item| {
                        let item = item.trim().trim_start_matches("the ").trim();
                        item.strip_suffix('s').unwrap_or(item).to_string()
                    })
                    .filter(|item| item.chars().count() >= 3)
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

/// Where a seed came from, so the run marks it used and the idea says what it builds on.
#[derive(Debug, Clone, PartialEq)]
pub enum SeedFrom {
    /// A topic-map row, by id.
    TopicMap(String),
    /// An interest row, by id.
    Interest(String),
    /// Something the portrait says the member is working on.
    WorkingOn,
    /// An everyday idea of the seed's kind, built on the member's profile.
    Life,
    /// A note from a screenshot the member took, by the note's id.
    Screenshot(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct IdeaSeed {
    pub from: SeedFrom,
    pub kind: IdeaKind,
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
                SeedFrom::WorkingOn | SeedFrom::Life => "memory",
                SeedFrom::Screenshot(_) => "screenshot",
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

/// The day it is in the household's zone, else in UTC.
pub fn local_day(now: DateTime<Utc>, zone: &str) -> NaiveDate {
    match zone.trim().parse::<chrono_tz::Tz>() {
        Ok(zone) => now.with_timezone(&zone).date_naive(),
        Err(_) => now.date_naive(),
    }
}

/// The order everyday ideas take turns in; the day picks where the turn starts.
const LIFE_KINDS: [IdeaKind; 5] = [
    IdeaKind::Recipe,
    IdeaKind::Outing,
    IdeaKind::Date,
    IdeaKind::Activity,
    IdeaKind::Trip,
];

/// What an everyday idea is kept under, so its kind rests after one is written.
pub fn life_label(kind: IdeaKind) -> &'static str {
    match kind {
        IdeaKind::Recipe => "A nutritious recipe",
        IdeaKind::Outing => "Getting outside",
        IdeaKind::Date => "A date",
        IdeaKind::SoloDate => "A date with yourself",
        IdeaKind::Activity => "Something fun to do",
        IdeaKind::Trip => "A holiday",
        IdeaKind::Interest | IdeaKind::Project => "",
    }
}

/// Today's everyday idea: the next kind in turn that has not been written in the last few
/// days. A date is with their partner when the profile names one, else with themselves.
pub fn life_seed(
    profile: &IdeaProfile,
    rested_kinds: &[String],
    today: NaiveDate,
) -> Option<IdeaSeed> {
    let rested: std::collections::HashSet<String> =
        rested_kinds.iter().map(|t| topic_key(t)).collect();
    let start = today.ordinal0() as usize % LIFE_KINDS.len();
    (0..LIFE_KINDS.len())
        .map(|turn| LIFE_KINDS[(start + turn) % LIFE_KINDS.len()])
        .map(|kind| match kind {
            IdeaKind::Date if !profile.has(About::Partner) => IdeaKind::SoloDate,
            other => other,
        })
        .find(|kind| {
            // Either date rests the other: one a few days is enough.
            let dates = [IdeaKind::Date, IdeaKind::SoloDate];
            if dates.contains(kind) {
                dates
                    .iter()
                    .all(|d| !rested.contains(&topic_key(life_label(*d))))
            } else {
                !rested.contains(&topic_key(life_label(*kind)))
            }
        })
        .map(|kind| IdeaSeed {
            from: SeedFrom::Life,
            kind,
            label: life_label(kind).to_string(),
            detail: String::new(),
            query: None,
        })
}

/// An idea built on what a screenshot showed the member looking at.
pub fn screenshot_seed(note_id: &str, note: &str) -> IdeaSeed {
    IdeaSeed {
        from: SeedFrom::Screenshot(note_id.to_string()),
        kind: IdeaKind::Interest,
        label: note.to_string(),
        detail: String::new(),
        query: None,
    }
}

/// What kind of idea the seed asks for, as both prompts read it.
pub fn kind_guidance(kind: IdeaKind, name: &str, place: Option<&str>) -> String {
    let near = match place.map(str::trim).filter(|p| !p.is_empty()) {
        Some(place) => format!(" in or near {place}"),
        None => String::new(),
    };
    match kind {
        IdeaKind::Outing => format!(
            "somewhere to go outdoors{near}: a walk, a hike, a park, a viewpoint, a market. \
             One named place."
        ),
        IdeaKind::Activity => format!(
            "something to do for fun{near}, alone or with others: bowling, a class, a game \
             night, live music, a match, a film. One named thing with a place or a time."
        ),
        IdeaKind::Date => format!(
            "a date for {name} and their partner{near}: one specific plan for an evening or \
             an afternoon. If the facts say {name} is not seeing anyone, make it a date with \
             themselves and answer with kind \"solo_date\"."
        ),
        IdeaKind::SoloDate => format!(
            "a date {name} takes themselves on{near}: one specific plan for an evening or an \
             afternoon alone that feels like a treat."
        ),
        IdeaKind::Trip => format!(
            "a holiday or a weekend away that suits {name} and whoever they live with: one \
             destination{}, with the season it is best in.",
            match place.map(str::trim).filter(|p| !p.is_empty()) {
                Some(place) => format!(" reachable from {place}"),
                None => String::new(),
            }
        ),
        IdeaKind::Recipe => format!(
            "one dish to cook that is nutritious and fits what {name} eats. It must respect \
             every diet, allergy and health fact below."
        ),
        IdeaKind::Interest => format!(
            "something new and specific about this interest that {name} could act on this week."
        ),
        IdeaKind::Project => format!(
            "something that moves this piece of {name}'s work forward: a tool, a guide, a \
             release, a technique."
        ),
    }
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
            kind: IdeaKind::Interest,
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
            kind: IdeaKind::Project,
            label: w,
            detail: String::new(),
            query: None,
        });
    let from_interests = interests
        .iter()
        .filter(|i| i.feeds_research() && fresh(&i.topic))
        .map(|i| IdeaSeed {
            from: SeedFrom::Interest(i.id.clone()),
            kind: IdeaKind::Interest,
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
                    kind: IdeaKind::Interest,
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
You write one web search for the Pond, which is looking for an idea for {name}. \
Below: the kind of idea, what it builds on, and what the Pond knows about {name}. \
Write the search most likely to find one specific, real thing {name} could do: \
a named place, event, route, dish or guide, not a list of general tips.

Reply with JSON only, exactly this shape:
{\"query\":\"...\",\"private\":false}

- query: 4 to 10 words. Name the place when the idea happens somewhere. Use \
what {name} eats, likes and avoids as plain search words. Never a person's \
name, an address or a health condition. No question mark, no quotes.
- private: true when what it builds on is a private matter the public web \
cannot answer, such as {name}'s own company finances, staff or health \
records; the query is then \"\".";

pub fn render_query_system(name: &str) -> String {
    QUERY_SYSTEM.replace("{name}", name)
}

/// What both prompts are told about the seed and the member.
fn brief(
    name: &str,
    profile: &IdeaProfile,
    portrait: Option<&MemberPortrait>,
    seed: &IdeaSeed,
) -> String {
    let mut out = format!(
        "Kind of idea: {}",
        kind_guidance(seed.kind, name, profile.place.as_deref())
    );
    if let SeedFrom::Screenshot(_) = seed.from {
        out.push_str(&format!(
            "\nBuilds on: a screenshot {name} took. What it showed: {}",
            seed.label
        ));
    } else if seed.from != SeedFrom::Life {
        out.push_str(&format!("\nBuilds on: {}", seed.label));
        if !seed.detail.is_empty() {
            out.push_str(&format!(" ({})", seed.detail));
        }
    }
    let facts = profile.as_prompt_lines();
    if !facts.is_empty() {
        out.push_str(&format!("\n\nAbout {name}:\n{facts}"));
    }
    if let Some(portrait) = portrait {
        out.push_str(&format!("\n\nPortrait:\n{}", portrait.as_prompt_lines()));
    }
    out
}

pub fn build_query_message(
    name: &str,
    profile: &IdeaProfile,
    portrait: Option<&MemberPortrait>,
    seed: &IdeaSeed,
) -> String {
    format!("{}\n\nJSON:", brief(name, profile, portrait, seed))
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
You write one idea for {name} from pages the Pond just read for them. It shows \
as a short card that opens to a fuller page, so write it tight and make it \
sound worth doing.

Use only what is below: the kind of idea, what it builds on, what the Pond \
knows about {name}, and the numbered pages. Every fact in the idea must come \
from the pages. Never go against a fact about {name}: nothing they are \
allergic to, do not eat, dislike, or asked not to hear about.

Reply with JSON only, exactly this shape:
{\"title\":\"...\",\"hook\":\"...\",\"pitch\":\"...\",\"why\":\"...\",\
\"facts\":[{\"label\":\"...\",\"value\":\"...\"}],\"sources\":[1],\
\"you_can\":[\"...\"],\"notes\":\"...\",\"goose_can\":{\"kind\":\"todo\",\
\"label\":\"...\",\"prompt\":\"...\",\"listing_url\":\"\"},\"score\":0.0}

- title: the idea itself in at most 8 words, naming the actual place, dish, \
event or thing the pages describe, as you would say it to a friend. Not a \
question.
- hook: one line of at most 15 words that makes {name} want to open it.
- pitch: two or three short sentences for the page that opens: what it is and \
what makes it good, with the specifics the pages give.
- why: one line to {name}, starting \"You\", naming the fact or interest of \
theirs it fits.
- facts: two to four things worth seeing at a glance, each a label of one or \
two words and a short value from the pages: where, when, how long, the cost, \
how many it serves, how hard it is. Never where you read it.
- sources: the numbers of the pages it rests on.
- you_can: two to four steps {name} could take, each a short imperative under \
80 characters.
- notes: for your own use when {name} asks about this idea later, not shown \
on the card. Every useful detail from the pages that did not fit above, as \
plain sentences: opening hours, prices, booking, the route, what to bring, \
and for a dish its ingredients with amounts and its method.
- goose_can: what the Pond can do for {name}, or null. The Pond is the \
household's own assistant, with to-dos and reminders, schedules, web research \
and watches on web pages.
  - kind: \"todo\" (put the steps on their to-do list), \"schedule\" (run a \
prompt for them on a recurring day), \"chat\" (talk it through), \"research\" \
(look into it in depth and report), or \"watch\" (watch a page that lists \
things for sale or to book).
  - label: the button's action words, under 30 characters.
  - prompt: for research or schedule, what the Pond should look up or run; \
else \"\".
  - listing_url: for watch only, the address of a numbered page that lists \
items; else \"\".
- score: 0 to 1, how new, specific and doable this is for {name}.
Write as if you know it yourself: never say \"page\", \"the pages\" or a \
page's number outside sources.
If the pages hold nothing that fits {name} and the kind of idea, or no one \
specific thing to name, reply {\"title\":\"\"}.";

pub fn render_idea_system(name: &str) -> String {
    IDEA_SYSTEM.replace("{name}", name)
}

pub fn build_idea_message(
    name: &str,
    profile: &IdeaProfile,
    portrait: Option<&MemberPortrait>,
    seed: &IdeaSeed,
    pages: &[ReadPage],
) -> String {
    let mut out = brief(name, profile, portrait, seed);
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

/// `text` cut to `max` characters at a word.
fn clipped_at_word(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    let end = cut.rfind(' ').unwrap_or(cut.len());
    cut[..end]
        .trim_end_matches([',', ';', ':', ' '])
        .to_string()
}

/// A fact that points at the reading rather than the thing: "Source: Page 1".
fn says_where_it_was_read(fact: &IdeaFact) -> bool {
    let label = fact.label.to_lowercase();
    let value = fact.value.to_lowercase();
    ["source", "sources", "page", "link", "url", "website"].contains(&label.as_str())
        || value.starts_with("page ")
        || value.starts_with("http")
}

/// The first sentence of `text`, for an idea whose reply gave no hook.
fn first_sentence(text: &str) -> &str {
    let end = text.find(". ").map(|at| at + 1).unwrap_or(text.len());
    text[..end].trim()
}

fn clipped(text: &str, max: usize) -> String {
    text.chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The idea the reply describes, or `None` when the model found nothing, wrote a question,
/// scored it too low, or named something the member is allergic to. Sources are the pages it
/// names; a watch needs a page it read.
pub fn parse_idea(
    raw: &str,
    profile_id: &str,
    profile: &IdeaProfile,
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
    let why = text_of(&map, "why").unwrap_or_else(|| match seed.from {
        SeedFrom::Life => String::new(),
        SeedFrom::Screenshot(_) => "You took a screenshot of this.".to_string(),
        _ => format!("You follow {}.", seed.label),
    });
    let hook = clipped_at_word(
        &text_of(&map, "hook").unwrap_or_else(|| first_sentence(&pitch).to_string()),
        MAX_HOOK_CHARS,
    );
    let facts: Vec<IdeaFact> = map
        .get("facts")
        .and_then(|f| f.as_array())
        .into_iter()
        .flatten()
        .filter_map(|f| f.as_object())
        .filter_map(|f| {
            Some(IdeaFact {
                label: clipped(&text_of(f, "label")?, 24),
                value: clipped_at_word(&text_of(f, "value")?, 80),
            })
        })
        .filter(|fact| !says_where_it_was_read(fact))
        .take(MAX_FACTS)
        .collect();
    let notes = clipped_at_word(&text_of(&map, "notes").unwrap_or_default(), MAX_NOTES_CHARS);
    // The one switch a reply may make: a date turns out to be with themselves.
    let kind = match (
        seed.kind,
        text_of(&map, "kind").and_then(|k| IdeaKind::parse(&k)),
    ) {
        (IdeaKind::Date, Some(IdeaKind::SoloDate)) => IdeaKind::SoloDate,
        (kind, _) => kind,
    };
    let said = format!("{title} {hook} {pitch} {notes}").to_lowercase();
    if profile
        .allergens()
        .iter()
        .any(|allergen| said.contains(allergen.as_str()))
    {
        return None;
    }

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
        kind,
        kind_label: kind.label().to_string(),
        title,
        hook,
        pitch,
        why,
        facts,
        notes,
        grounded_in: match seed.from {
            SeedFrom::Life => Vec::new(),
            _ => vec![seed.grounding()],
        },
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
            kind: IdeaKind::Interest,
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
        let idea = parse_idea(
            raw,
            "p",
            &IdeaProfile::default(),
            &seed(),
            &pages(),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(idea.sources.len(), 1);
        assert_eq!(idea.sources[0].url, "https://example.com/fielders");
        assert_eq!(idea.goose_can.unwrap().kind, GooseKind::Watch);
        assert_eq!(
            idea.goose_plan.listing_url.as_deref(),
            Some("https://example.com/fielders")
        );
        assert_eq!(idea.grounded_in[0].label, "Toyota Fielder");
        assert_eq!(idea.you_can.len(), 2);
        assert_eq!(idea.kind, IdeaKind::Interest);
        assert_eq!(idea.kind_label, "From your interests");
        // No hook in the reply: the card's line is the pitch's first sentence.
        assert_eq!(idea.hook, "Prices soften after the import rush.");
    }

    fn profile(facts: &[(About, &str)]) -> IdeaProfile {
        IdeaProfile {
            place: Some("Nairobi, Kenya".into()),
            facts: facts
                .iter()
                .map(|(about, fact)| (*about, fact.to_string()))
                .collect(),
        }
    }

    fn day(ordinal0: u32) -> NaiveDate {
        NaiveDate::from_yo_opt(2026, ordinal0 + 1).unwrap()
    }

    #[test]
    fn everyday_ideas_take_turns_and_a_kind_rests_after_one() {
        let nobody = profile(&[]);
        let kinds: Vec<IdeaKind> = (0..5)
            .map(|d| life_seed(&nobody, &[], day(d)).unwrap().kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                IdeaKind::Recipe,
                IdeaKind::Outing,
                IdeaKind::SoloDate,
                IdeaKind::Activity,
                IdeaKind::Trip
            ]
        );
        // A recipe written this week: the day's turn passes to the next kind.
        let seed = life_seed(&nobody, &["A nutritious recipe".to_string()], day(0)).unwrap();
        assert_eq!(seed.kind, IdeaKind::Outing);
        assert_eq!(seed.label, "Getting outside");
        assert_eq!(seed.from, SeedFrom::Life);
        let all: Vec<String> = [
            IdeaKind::Recipe,
            IdeaKind::Outing,
            IdeaKind::Date,
            IdeaKind::Activity,
            IdeaKind::Trip,
        ]
        .iter()
        .map(|k| life_label(*k).to_string())
        .collect();
        assert_eq!(life_seed(&nobody, &all, day(0)), None);
    }

    #[test]
    fn a_date_is_with_their_partner_only_when_the_profile_names_one() {
        let single = profile(&[]);
        let dating = profile(&[(About::Partner, "Jerry's partner is Amina.")]);
        assert_eq!(
            life_seed(&single, &[], day(2)).unwrap().kind,
            IdeaKind::SoloDate
        );
        let seed = life_seed(&dating, &[], day(2)).unwrap();
        assert_eq!(seed.kind, IdeaKind::Date);
        // A solo date written this week rests the date as well.
        let after = life_seed(&dating, &["A date with yourself".to_string()], day(2)).unwrap();
        assert_eq!(after.kind, IdeaKind::Activity);

        // The reply may say the date is with themselves; it may not change any other kind.
        let raw = r#"{"kind":"solo_date","title":"Dinner for one at the terrace","pitch":"A set menu.","sources":[1],"score":0.8}"#;
        let idea = parse_idea(raw, "p", &dating, &seed, &pages(), Utc::now()).unwrap();
        assert_eq!(idea.kind, IdeaKind::SoloDate);
        assert_eq!(idea.kind_label, "Solo date");
        assert!(
            idea.grounded_in.is_empty(),
            "an everyday idea builds on no one topic"
        );
        let recipe = life_seed(&single, &[], day(0)).unwrap();
        let idea = parse_idea(raw, "p", &single, &recipe, &pages(), Utc::now()).unwrap();
        assert_eq!(idea.kind, IdeaKind::Recipe);
    }

    #[test]
    fn the_brief_names_the_kind_the_place_and_the_facts_an_idea_must_fit() {
        let known = profile(&[
            (About::Likes, "Jerry enjoys cooking."),
            (About::Work, "Jerry runs Jarida."),
            (
                About::Allergy,
                "Jerry is allergic to peanuts and shellfish.",
            ),
            (About::Diet, "Jerry is vegetarian."),
        ]);
        let recipe = life_seed(&known, &[], day(0)).unwrap();
        let message = build_query_message("Jerry", &known, None, &recipe);
        assert!(message.starts_with("Kind of idea: one dish to cook that is nutritious"));
        assert!(!message.contains("Builds on:"));
        assert!(message.contains(
            "About Jerry:\n- Jerry is allergic to peanuts and shellfish.\n- Jerry is vegetarian.\n\
             - Jerry enjoys cooking."
        ));
        assert!(
            !message.contains("Jarida"),
            "work is not what an idea must fit"
        );

        let outing = life_seed(&known, &[], day(1)).unwrap();
        let message = build_idea_message("Jerry", &known, None, &outing, &pages());
        assert!(message.contains("somewhere to go outdoors in or near Nairobi, Kenya"));
        assert!(message.contains("[1] Used car prices in Kenya"));

        let message = build_query_message("Jerry", &IdeaProfile::default(), None, &seed());
        assert!(message.contains("Builds on: Toyota Fielder"));
        assert!(!message.contains("About Jerry"));

        let shot = screenshot_seed("n1", "Looking at the Ngong Hills hiking trail");
        let message = build_query_message("Jerry", &IdeaProfile::default(), None, &shot);
        assert!(message.contains(
            "Builds on: a screenshot Jerry took. What it showed: Looking at the Ngong Hills \
             hiking trail"
        ));
        assert_eq!(shot.grounding().kind, "screenshot");
        let raw = r#"{"title":"Walk the Ngong Hills loop","pitch":"An 11 km ridge walk.","sources":[1],"score":0.8}"#;
        let idea = parse_idea(
            raw,
            "p",
            &IdeaProfile::default(),
            &shot,
            &pages(),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(idea.why, "You took a screenshot of this.");
        assert_eq!(idea.grounded_in[0].kind, "screenshot");
    }

    #[test]
    fn an_idea_naming_something_they_are_allergic_to_is_dropped() {
        let known = profile(&[(
            About::Allergy,
            "Jerry is allergic to peanuts and shellfish.",
        )]);
        assert_eq!(known.allergens(), vec!["peanut", "shellfish"]);
        let recipe = life_seed(&known, &[], day(0)).unwrap();
        let with = r#"{"title":"Peanut noodle bowl","pitch":"Ready in 20 minutes.","sources":[1],"score":0.9}"#;
        assert!(parse_idea(with, "p", &known, &recipe, &pages(), Utc::now()).is_none());
        let in_notes = r#"{"title":"Noodle bowl","pitch":"Ready in 20 minutes.","notes":"Top with crushed peanuts.","sources":[1],"score":0.9}"#;
        assert!(parse_idea(in_notes, "p", &known, &recipe, &pages(), Utc::now()).is_none());
        let without = r#"{"title":"Lentil dal","hook":"Dinner in 35 minutes, with what is in the cupboard.",
            "pitch":"Red lentils and spinach.","facts":[{"label":"Serves","value":"4"},{"label":"","value":"x"}],
            "notes":"Soak the lentils for 20 minutes.","sources":[1],"score":0.9}"#;
        let idea = parse_idea(without, "p", &known, &recipe, &pages(), Utc::now()).unwrap();
        assert_eq!(
            idea.hook,
            "Dinner in 35 minutes, with what is in the cupboard."
        );
        assert_eq!(idea.facts.len(), 1);
        assert_eq!(idea.facts[0].label, "Serves");
        let pointing = r#"{"title":"Lentil dal","pitch":"Red lentils and spinach.",
            "facts":[{"label":"Source","value":"Page 1"},{"label":"Recipe","value":"Page 2 of the list"},
                     {"label":"Time","value":"35 minutes"}],"sources":[1],"score":0.9}"#;
        let pointed = parse_idea(pointing, "p", &known, &recipe, &pages(), Utc::now()).unwrap();
        assert_eq!(
            pointed.facts.len(),
            1,
            "facts about the reading are left out"
        );
        assert_eq!(pointed.facts[0].label, "Time");
        assert_eq!(idea.notes, "Soak the lentils for 20 minutes.");

        let card = idea.as_card();
        assert_eq!(card.kind, "Recipe");
        assert!(card.content.contains("Serves: 4"));
        assert!(card.notes.starts_with("Soak the lentils for 20 minutes."));
        assert!(card
            .notes
            .contains("Source: Used car prices in Kenya (https://example.com/prices)"));
    }

    #[test]
    fn a_watch_on_a_page_never_read_becomes_research() {
        let raw = r#"{"title":"Catch the dip","pitch":"Prices soften.","why":"You shop.","sources":[1],
            "you_can":[],"goose_can":{"kind":"watch","label":"Watch","prompt":"","listing_url":"https://elsewhere.com/x"},"score":0.7}"#;
        let idea = parse_idea(
            raw,
            "p",
            &IdeaProfile::default(),
            &seed(),
            &pages(),
            Utc::now(),
        )
        .unwrap();
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
                parse_idea(
                    raw,
                    "p",
                    &IdeaProfile::default(),
                    &seed(),
                    &pages(),
                    Utc::now()
                )
                .is_none(),
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
        assert!(idea.contains("Use only what is below"));
        assert!(idea.contains("Never go against a fact about Jerry"));
        for field in [
            "title:",
            "hook:",
            "facts:",
            "notes:",
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
        assert!(query.contains("private matter the public web cannot answer"));
        assert!(query.contains("Never a person's name, an address or a health condition"));
    }
}
