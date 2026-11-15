//! The one interview a member gives the Pond: fixed sections, tap answers, at most one follow-up
//! a section, and a draft of profile facts and interests they approve at the end.
//!
//! Code asks every question, so nobody waits on a model between taps. The answers that are
//! taps become facts from sentences written here; only what the member typed in their own
//! words goes to one model call at the end. Nothing here calls a model or a store.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::user_data::domain::interest::{Interest, CATEGORIES};
use crate::user_data::domain::interest_interview::{
    InterestInterview, InterviewNote, InterviewStatus, InterviewTurn, Speaker,
    THINKING_TIMEOUT_SECS,
};
use crate::user_data::domain::member_profile::{admit, same_sentence, About};
use crate::user_data::services::model_json::{object_in, text_of, texts_of};

/// Longest answer kept from a free-text field, in characters.
const ANSWER_CHARS: usize = 300;
/// Facts one interview may draft: more than the sections can yield, a bound on a model's reply.
const MAX_FACTS: usize = 24;

/// One section: a fixed question with tap answers.
#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub id: &'static str,
    pub heading: &'static str,
    pub question: &'static str,
    pub choices: &'static [&'static str],
    pub multi: bool,
    pub allow_free: bool,
}

const EMPLOYED: &str = "Employed";
const SELF_EMPLOYED: &str = "Self-employed";
const STUDENT: &str = "Student";
const AT_HOME: &str = "At home";
const RETIRED: &str = "Retired";

const PARTNER: &str = "Partner or spouse";
const SEEING_SOMEONE: &str = "Seeing someone";
const CHILDREN: &str = "Children";
const FAMILY: &str = "Family";
const HOUSEMATES: &str = "Housemates";
const PETS: &str = "Pets";
const JUST_ME: &str = "Just me";

const NO_RESTRICTIONS: &str = "No restrictions";
const ALLERGIES: &str = "Allergies";

const NOTHING: &str = "Nothing";

pub const SECTIONS: [Section; 8] = [
    Section {
        id: "you",
        heading: "You",
        question: "What do you do most days?",
        choices: &[EMPLOYED, SELF_EMPLOYED, STUDENT, AT_HOME, RETIRED],
        multi: false,
        allow_free: true,
    },
    Section {
        id: "home",
        heading: "Your people",
        question: "Who is in your life at home?",
        choices: &[
            PARTNER,
            SEEING_SOMEONE,
            CHILDREN,
            FAMILY,
            HOUSEMATES,
            PETS,
            JUST_ME,
        ],
        multi: true,
        allow_free: false,
    },
    Section {
        id: "food",
        heading: "Food and health",
        question: "Is there anything about what you eat that I should know?",
        choices: &[
            NO_RESTRICTIONS,
            "Vegetarian",
            "Vegan",
            "Halal",
            "No pork",
            "No dairy",
            "No gluten",
            ALLERGIES,
        ],
        multi: true,
        allow_free: true,
    },
    Section {
        id: "days",
        heading: "Your days",
        question: "What are your weekdays like?",
        choices: &[
            "Early riser",
            "Night owl",
            "Nine to five",
            "Shifts",
            "Flexible hours",
        ],
        multi: true,
        allow_free: true,
    },
    Section {
        id: "fun",
        heading: "For fun",
        question: "What do you do for fun?",
        choices: &[
            "Sport",
            "Music",
            "Film and TV",
            "Games",
            "Books",
            "Cooking",
            "Travel",
            "The outdoors",
            "Tech",
            "Making things",
        ],
        multi: true,
        allow_free: true,
    },
    Section {
        id: "news",
        heading: "News",
        question: "How much news do you want from me?",
        choices: &["A daily digest", "Headlines as they come", "Both", "None"],
        multi: false,
        allow_free: false,
    },
    Section {
        id: "limits",
        heading: "Off limits",
        question: "Is there anything I should never bring up?",
        choices: &[NOTHING, "Politics", "Religion", "Work", "My health"],
        multi: true,
        allow_free: true,
    },
    Section {
        id: "style",
        heading: "How I talk to you",
        question: "How should I answer you?",
        choices: &["Short and direct", "With detail", "Casual", "Formal"],
        multi: true,
        allow_free: false,
    },
];

/// What a member answered for one section.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SectionAnswer {
    pub section: String,
    #[serde(default)]
    pub choices: Vec<String>,
    /// What they typed instead of, or beside, a tap.
    #[serde(default)]
    pub text: Option<String>,
    /// What they typed to the section's follow-up.
    #[serde(default)]
    pub more: Option<String>,
    #[serde(default)]
    pub skipped: bool,
}

impl SectionAnswer {
    fn chose(&self, choice: &str) -> bool {
        self.choices.iter().any(|c| c.eq_ignore_ascii_case(choice))
    }

    /// Whether the model reads this answer: it holds words no sentence here is written from.
    /// An allergy is written in code, so the food follow-up alone does not count.
    fn for_the_model(&self) -> bool {
        self.text.is_some() || (self.more.is_some() && self.section != "food")
    }
}

/// A fact the interview drafted, for the member to keep or drop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftFact {
    pub id: String,
    pub about: About,
    pub content: String,
}

/// How much news the member asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewsWish {
    Digest,
    Tiles,
    Both,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberInterview {
    pub status: InterviewStatus,
    pub turns: Vec<InterviewTurn>,
    /// Index into [`SECTIONS`] of the section being asked.
    pub section: usize,
    /// Whether the open question is the section's follow-up.
    #[serde(default)]
    pub follow_up: bool,
    #[serde(default)]
    pub answers: Vec<SectionAnswer>,
    /// Drafted for review: what the Pond would remember.
    #[serde(default)]
    pub facts: Vec<DraftFact>,
    /// Drafted for review: what the Pond would follow for them. Ids are `d1`, `d2`, ...
    #[serde(default)]
    pub interests: Vec<Interest>,
    #[serde(default)]
    pub news: Option<NewsWish>,
    #[serde(skip)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Default for MemberInterview {
    fn default() -> Self {
        Self {
            status: InterviewStatus::NotStarted,
            turns: Vec::new(),
            section: 0,
            follow_up: false,
            answers: Vec::new(),
            facts: Vec::new(),
            interests: Vec::new(),
            news: None,
            updated_at: None,
        }
    }
}

/// Why an interview call was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NotStarted,
    /// No question is waiting for an answer.
    NothingAsked,
    /// Neither a tap, nor words, nor a skip.
    EmptyAnswer,
    NotInReview,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "the interview has not started",
            Self::NothingAsked => "no question is waiting for an answer",
            Self::EmptyAnswer => "the answer is empty; send choices, text, or skip",
            Self::NotInReview => "the interview has nothing to review yet",
        }
    }
}

/// An answer as the phone sends it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AnswerIn {
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub skip: bool,
}

fn tidy(text: Option<&str>) -> Option<String> {
    let line = text?.split_whitespace().collect::<Vec<_>>().join(" ");
    let line: String = line.chars().take(ANSWER_CHARS).collect();
    let line = line.trim().to_string();
    (!line.is_empty()).then_some(line)
}

/// The follow-up a section asks after `answer`, if any: one, chosen in code.
pub fn follow_up_for(section: &Section, answer: &SectionAnswer) -> Option<&'static str> {
    if answer.skipped {
        return None;
    }
    match section.id {
        "you"
            if [EMPLOYED, SELF_EMPLOYED, STUDENT]
                .iter()
                .any(|c| answer.chose(c))
                && answer.text.is_none() =>
        {
            Some("What do you do, in a few words?")
        }
        "home"
            if [PARTNER, SEEING_SOMEONE, CHILDREN, FAMILY, HOUSEMATES, PETS]
                .iter()
                .any(|c| answer.chose(c)) =>
        {
            Some("What are their names?")
        }
        "food" if answer.chose(ALLERGIES) => Some("What are you allergic to?"),
        "fun" if !answer.choices.is_empty() => {
            Some("Which in particular? Teams, shows, games, artists, places.")
        }
        _ => None,
    }
}

impl MemberInterview {
    fn push(&mut self, role: Speaker, text: String, section: Option<&Section>, now: DateTime<Utc>) {
        let id = format!("t{}", self.turns.len() + 1);
        let (choices, multi, allow_free) = match section {
            Some(s) => (
                s.choices.iter().map(|c| c.to_string()).collect(),
                s.multi,
                s.allow_free,
            ),
            None => (Vec::new(), false, role == Speaker::Goose),
        };
        self.turns.push(InterviewTurn {
            id,
            role,
            text,
            choices,
            multi,
            allow_free,
            at: now,
        });
    }

    fn ask_section(&mut self, now: DateTime<Utc>) {
        let section = &SECTIONS[self.section];
        self.follow_up = false;
        self.push(
            Speaker::Goose,
            section.question.to_string(),
            Some(section),
            now,
        );
    }

    /// Move to the next section, or to the write-up once the last is answered.
    fn advance(&mut self, now: DateTime<Utc>) {
        self.section += 1;
        if self.section >= SECTIONS.len() {
            self.follow_up = false;
            self.status = InterviewStatus::Thinking;
        } else {
            self.ask_section(now);
        }
    }

    /// The question waiting for an answer, if the last turn is Goose's.
    pub fn open_question(&self) -> Option<&InterviewTurn> {
        self.turns.last().filter(|t| t.role == Speaker::Goose)
    }

    /// Questions answered or skipped so far, and how many there are.
    pub fn progress(&self) -> (usize, usize) {
        (self.section.min(SECTIONS.len()), SECTIONS.len())
    }

    /// Whether any answer holds words a model has to read.
    pub fn has_free_text(&self) -> bool {
        self.answers.iter().any(|a| !a.skipped && a.for_the_model())
    }

    /// Whether a write-up has outlived any writer that could still finish it.
    pub fn thinking_is_stale(&self, now: DateTime<Utc>) -> bool {
        self.status == InterviewStatus::Thinking
            && self
                .updated_at
                .is_none_or(|at| (now - at).num_seconds() > THINKING_TIMEOUT_SECS)
    }
}

/// Start, resume, or begin again after a finished one. The bool says whether it was started
/// fresh, which is the only case that needs saving.
pub fn begin(existing: Option<MemberInterview>, now: DateTime<Utc>) -> (MemberInterview, bool) {
    match existing {
        Some(interview)
            if matches!(
                interview.status,
                InterviewStatus::Asking | InterviewStatus::Thinking | InterviewStatus::Review
            ) =>
        {
            (interview, false)
        }
        _ => {
            let mut fresh = MemberInterview {
                status: InterviewStatus::Asking,
                ..MemberInterview::default()
            };
            fresh.ask_section(now);
            (fresh, true)
        }
    }
}

/// Record an answer and ask what comes next: the section's follow-up, the next section, or
/// nothing more (status `thinking`, the write-up being due).
pub fn answer(
    mut interview: MemberInterview,
    said: AnswerIn,
    now: DateTime<Utc>,
) -> Result<MemberInterview, Refusal> {
    if interview.status == InterviewStatus::NotStarted {
        return Err(Refusal::NotStarted);
    }
    if interview.status != InterviewStatus::Asking || interview.open_question().is_none() {
        return Err(Refusal::NothingAsked);
    }
    let section = SECTIONS[interview.section];
    let choices: Vec<String> = said
        .choices
        .iter()
        .filter_map(|c| {
            section
                .choices
                .iter()
                .find(|known| known.eq_ignore_ascii_case(c.trim()))
                .map(|known| known.to_string())
        })
        .collect();
    let text = tidy(said.text.as_deref());
    if !said.skip && choices.is_empty() && text.is_none() {
        return Err(Refusal::EmptyAnswer);
    }

    let shown = if said.skip {
        "Skip".to_string()
    } else {
        choices
            .iter()
            .cloned()
            .chain(text.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };
    interview.push(Speaker::You, shown, None, now);

    if interview.follow_up {
        if let Some(open) = interview.answers.last_mut() {
            if !said.skip {
                // A follow-up has no taps of its own; a tap sent to it is read as its words.
                open.more = text.or_else(|| tidy(Some(&said.choices.join(", "))));
            }
        }
        interview.advance(now);
        return Ok(interview);
    }

    let given = SectionAnswer {
        section: section.id.to_string(),
        choices,
        text,
        more: None,
        skipped: said.skip,
    };
    let next = follow_up_for(&section, &given);
    interview.answers.push(given);
    match next {
        Some(question) => {
            interview.follow_up = true;
            interview.push(Speaker::Goose, question.to_string(), None, now);
        }
        None => interview.advance(now),
    }
    Ok(interview)
}

/// Stop asking and write up what there is.
pub fn finish(mut interview: MemberInterview) -> Result<MemberInterview, Refusal> {
    match interview.status {
        InterviewStatus::NotStarted => Err(Refusal::NotStarted),
        InterviewStatus::Asking => {
            interview.status = InterviewStatus::Thinking;
            interview.follow_up = false;
            Ok(interview)
        }
        _ => Ok(interview),
    }
}

fn listed(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// A fact a tap establishes, as a sentence written here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Templated {
    pub about: About,
    pub sentence: String,
    /// It says only that something exists (a partner, a job), so it adds nothing where the
    /// Pond already knows more.
    pub thin: bool,
}

/// The facts the taps establish. `name` is what the member is called in them. Where the member
/// also typed, the model writes the fuller sentence and none is written here, unless
/// `taps_only`: the model's reply is not coming, so every tap is written and nothing is lost.
pub fn templated_facts(name: &str, answers: &[SectionAnswer], taps_only: bool) -> Vec<Templated> {
    let mut facts: Vec<Templated> = Vec::new();
    let mut fact = |about: About, sentence: String, thin: bool| {
        facts.push(Templated {
            about,
            sentence,
            thin,
        })
    };
    for answer in answers.iter().filter(|a| !a.skipped) {
        let model_writes = !taps_only && answer.for_the_model();
        match answer.section.as_str() {
            "you" => {
                let what = [
                    (EMPLOYED, "is employed", true),
                    (SELF_EMPLOYED, "is self-employed", true),
                    (STUDENT, "is a student", true),
                    (AT_HOME, "is at home most days", false),
                    (RETIRED, "is retired", false),
                ]
                .into_iter()
                .find(|(choice, _, _)| answer.chose(choice));
                if let (Some((_, what, thin)), false) = (what, model_writes) {
                    fact(About::Work, format!("{name} {what}."), thin);
                }
            }
            "home" => {
                if !model_writes {
                    if answer.chose(PARTNER) {
                        fact(About::Partner, format!("{name} has a partner."), true);
                    } else if answer.chose(SEEING_SOMEONE) {
                        fact(About::Partner, format!("{name} is seeing someone."), true);
                    }
                    for (choice, about, words) in [
                        (CHILDREN, About::Child, "has children"),
                        (FAMILY, About::Family, "lives with family"),
                        (HOUSEMATES, About::Friend, "lives with housemates"),
                        (PETS, About::Pet, "has pets"),
                    ] {
                        if answer.chose(choice) {
                            fact(about, format!("{name} {words}."), true);
                        }
                    }
                }
                if answer.chose(JUST_ME) && answer.choices.len() == 1 {
                    fact(About::Family, format!("{name} lives alone."), false);
                }
            }
            "food" => {
                let parts: Vec<String> = [
                    ("Vegetarian", "is vegetarian"),
                    ("Vegan", "is vegan"),
                    ("Halal", "eats halal"),
                    ("No pork", "does not eat pork"),
                    ("No dairy", "does not eat dairy"),
                    ("No gluten", "does not eat gluten"),
                ]
                .iter()
                .filter(|(choice, _)| answer.chose(choice))
                .map(|(_, words)| words.to_string())
                .collect();
                if !model_writes {
                    if !parts.is_empty() {
                        fact(About::Diet, format!("{name} {}.", listed(&parts)), false);
                    } else if answer.chose(NO_RESTRICTIONS) {
                        fact(About::Diet, format!("{name} eats everything."), false);
                    }
                }
                if let Some(allergic_to) =
                    answer.more.as_deref().filter(|_| answer.chose(ALLERGIES))
                {
                    let what = allergic_to.trim_end_matches('.');
                    fact(
                        About::Allergy,
                        format!("{name} is allergic to {what}."),
                        false,
                    );
                }
            }
            "days" => {
                for (choice, words) in [
                    ("Early riser", "is an early riser"),
                    ("Night owl", "is a night owl"),
                    ("Nine to five", "works nine to five on weekdays"),
                    ("Shifts", "works shifts"),
                    ("Flexible hours", "keeps flexible hours"),
                ] {
                    if answer.chose(choice) {
                        fact(About::Routine, format!("{name} {words}."), false);
                    }
                }
            }
            "fun" => {
                let liked: Vec<String> = answer.choices.iter().map(|c| c.to_lowercase()).collect();
                if !liked.is_empty() {
                    fact(
                        About::Likes,
                        format!("{name} enjoys {}.", listed(&liked)),
                        false,
                    );
                }
            }
            "limits" => {
                let typed = answer.text.iter().filter(|_| taps_only);
                let barred: Vec<String> = answer
                    .choices
                    .iter()
                    .filter(|c| !c.eq_ignore_ascii_case(NOTHING))
                    .map(|c| match c.as_str() {
                        "My health" => "their health".to_string(),
                        other => other.to_lowercase(),
                    })
                    .chain(typed.map(|t| t.trim_end_matches('.').to_string()))
                    .collect();
                if !barred.is_empty() {
                    fact(
                        About::Instruction,
                        format!("{name} does not want {} brought up.", listed(&barred)),
                        false,
                    );
                }
            }
            "style" => {
                let ways: Vec<String> = [
                    ("Short and direct", "short and direct"),
                    ("With detail", "detailed"),
                    ("Casual", "casual"),
                    ("Formal", "formal"),
                ]
                .iter()
                .filter(|(choice, _)| answer.chose(choice))
                .map(|(_, words)| words.to_string())
                .collect();
                if !ways.is_empty() {
                    fact(
                        About::Style,
                        format!("{name} wants answers {}.", listed(&ways)),
                        false,
                    );
                }
            }
            _ => {}
        }
    }
    facts
}

/// How much news the member asked for, if they answered that section.
pub fn news_wish(answers: &[SectionAnswer]) -> Option<NewsWish> {
    let answer = answers.iter().find(|a| a.section == "news" && !a.skipped)?;
    Some(if answer.chose("Both") {
        NewsWish::Both
    } else if answer.chose("A daily digest") {
        NewsWish::Digest
    } else if answer.chose("Headlines as they come") {
        NewsWish::Tiles
    } else if answer.chose("None") {
        NewsWish::None
    } else {
        return None;
    })
}

impl NewsWish {
    /// `(tiles, digest)` as the news settings hold them.
    pub fn tiles_and_digest(self) -> (bool, bool) {
        match self {
            Self::Digest => (false, true),
            Self::Tiles => (true, false),
            Self::Both => (true, true),
            Self::None => (false, false),
        }
    }
}

const SYSTEM: &str = "\
You turn what {name} typed in an interview into profile facts and interests. \
Use only the answers below; add nothing they did not say.

Reply with JSON only, exactly this shape:
{\"facts\":[{\"about\":\"...\",\"content\":\"...\"}],\
\"interests\":[{\"topic\":\"...\",\"category\":\"...\",\"facets\":[\"...\"]}]}

- facts: one for each thing an answer establishes about {name}.
  - content: one plain sentence in the third person that names {name} and \
every person, pet and place it mentions.
  - about: exactly one of these words: work partner child pet family friend \
diet allergy health routine likes dislikes place goal instruction.
  - One fact each for work, partner and diet at most: put what they chose and \
what they wrote into the same sentence.
- interests: only from the \"for fun\" answer: each specific thing by its own \
name (a team, a show, a game, an artist, a place), never a general word.
  - category: one of {categories}.
  - facets: the names and details given with it; may be [].
An answer that establishes nothing is left out. [] is a complete answer.";

pub fn render_system(name: &str) -> String {
    SYSTEM
        .replace("{categories}", &CATEGORIES.join(", "))
        .replace("{name}", name)
}

/// The typed answers, each with its question and taps, for the one model call, and the
/// sentences already written from taps so it repeats none; `None` when nothing was typed.
pub fn build_user_message(name: &str, answers: &[SectionAnswer]) -> Option<String> {
    let mut out = String::from("Answers:\n");
    let mut any = false;
    for answer in answers.iter().filter(|a| !a.skipped && a.for_the_model()) {
        let Some(section) = SECTIONS.iter().find(|s| s.id == answer.section) else {
            continue;
        };
        any = true;
        let label = if section.id == "fun" {
            "for fun"
        } else {
            section.id
        };
        out.push_str(&format!("- ({label}) {}", section.question));
        if !answer.choices.is_empty() {
            out.push_str(&format!(" They chose: {}.", answer.choices.join(", ")));
        }
        if let Some(text) = &answer.text {
            out.push_str(&format!(" They wrote: \"{text}\""));
        }
        if let Some(more) = answer.more.as_ref().filter(|_| section.id != "food") {
            let asked = follow_up_for(section, answer).unwrap_or("Tell me more.");
            out.push_str(&format!(" Asked \"{asked}\" they wrote: \"{more}\""));
        }
        out.push('\n');
    }
    if !any {
        return None;
    }
    let written = templated_facts(name, answers, false);
    if !written.is_empty() {
        out.push_str("\nAlready written, do not repeat:\n");
        for fact in &written {
            out.push_str(&format!("- {}\n", fact.sentence));
        }
    }
    out.push_str("\nJSON:");
    Some(out)
}

/// What the model made of the typed answers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Written {
    pub facts: Vec<(About, String)>,
    pub interests: Vec<InterviewNote>,
}

/// `None` when the reply held no JSON object, which the write-up treats as no reply at all.
pub fn parse_reply(raw: &str) -> Option<Written> {
    let map = object_in(raw)?;
    let entries = |key: &str| -> Vec<serde_json::Map<String, serde_json::Value>> {
        map.get(key)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_object().cloned())
            .collect()
    };
    let facts = entries("facts")
        .iter()
        .filter_map(|entry| {
            let about = About::parse(&text_of(entry, "about")?)?;
            Some((about, text_of(entry, "content")?))
        })
        .collect();
    let interests = entries("interests")
        .iter()
        .filter_map(|entry| {
            Some(InterviewNote {
                topic: text_of(entry, "topic")?,
                category: text_of(entry, "category"),
                facets: texts_of(entry, "facets"),
                ..InterviewNote::default()
            })
        })
        .collect();
    Some(Written { facts, interests })
}

/// The interview with its draft for review.
///
/// Every fact passes the write policy, none is drafted twice, and none repeats what the Pond
/// holds (`known`: the member's live facts). A thin fact is left out where the Pond knows
/// anything under the same word, and a one-at-a-time word gets one draft, so approving never
/// closes a fuller fact for a thinner one. `written` is `None` when no model reply came: then
/// every tap is written here. `aliases` are the member's names.
pub fn into_review(
    mut interview: MemberInterview,
    name: &str,
    aliases: &[String],
    known: &[(About, String)],
    written: Option<Written>,
    now: DateTime<Utc>,
) -> MemberInterview {
    let taps_only = written.is_none() || !interview.has_free_text();
    let written = written.unwrap_or_default();
    let drafted = templated_facts(name, &interview.answers, taps_only)
        .into_iter()
        .map(|t| (t.about, t.sentence, t.thin))
        .chain(written.facts.into_iter().map(|(a, s)| (a, s, false)));
    let mut facts: Vec<DraftFact> = Vec::new();
    for (about, sentence, thin) in drafted {
        if facts.len() >= MAX_FACTS {
            break;
        }
        let Ok(content) = admit(&sentence, Some(about), aliases) else {
            continue;
        };
        let already = |held: &str| same_sentence(held, &content);
        if thin && known.iter().any(|(word, _)| *word == about) {
            continue;
        }
        if known.iter().any(|(_, held)| already(held))
            || facts.iter().any(|f| already(&f.content))
            || (about.single_valued() && facts.iter().any(|f| f.about == about))
        {
            continue;
        }
        facts.push(DraftFact {
            id: format!("f{}", facts.len() + 1),
            about,
            content,
        });
    }
    let mut draft = InterestInterview::default();
    draft.apply_notes(&written.interests, now);
    interview.news = news_wish(&interview.answers);
    interview.facts = facts;
    interview.interests = draft.draft;
    interview.follow_up = false;
    interview.status = InterviewStatus::Review;
    interview
}

/// What the member kept of the draft.
pub fn approved<'a>(
    interview: &'a MemberInterview,
    keep_facts: &[String],
    keep_interests: &[String],
) -> Result<(Vec<&'a DraftFact>, Vec<&'a Interest>), Refusal> {
    if interview.status != InterviewStatus::Review {
        return Err(Refusal::NotInReview);
    }
    Ok((
        interview
            .facts
            .iter()
            .filter(|f| keep_facts.contains(&f.id))
            .collect(),
        interview
            .interests
            .iter()
            .filter(|i| keep_interests.contains(&i.id))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn tap(choices: &[&str]) -> AnswerIn {
        AnswerIn {
            choices: choices.iter().map(|c| c.to_string()).collect(),
            ..AnswerIn::default()
        }
    }

    fn typed(text: &str) -> AnswerIn {
        AnswerIn {
            text: Some(text.to_string()),
            ..AnswerIn::default()
        }
    }

    fn skip() -> AnswerIn {
        AnswerIn {
            skip: true,
            ..AnswerIn::default()
        }
    }

    fn started() -> MemberInterview {
        begin(None, now()).0
    }

    fn after(answers: Vec<AnswerIn>) -> MemberInterview {
        answers.into_iter().fold(started(), |interview, said| {
            answer(interview, said, now()).expect("an answer the interview takes")
        })
    }

    fn jerry() -> Vec<String> {
        vec!["Jerry".to_string()]
    }

    #[test]
    fn it_opens_on_the_first_section_with_its_taps() {
        let interview = started();
        assert_eq!(interview.status, InterviewStatus::Asking);
        let open = interview.open_question().unwrap();
        assert_eq!(open.text, "What do you do most days?");
        assert_eq!(open.choices.len(), 5);
        assert!(open.allow_free);
        assert_eq!(interview.progress(), (0, SECTIONS.len()));
    }

    #[test]
    fn every_section_asks_one_short_question_with_a_handful_of_taps() {
        for section in SECTIONS {
            assert!(section.question.chars().count() <= 70, "{}", section.id);
            assert!(section.question.ends_with('?'), "{}", section.id);
            assert!((3..=10).contains(&section.choices.len()), "{}", section.id);
        }
    }

    #[test]
    fn a_section_asks_at_most_one_follow_up_and_only_when_the_answer_calls_for_it() {
        // Self-employed asks what kind of work; the next answer moves on, whatever it says.
        let interview = after(vec![tap(&[SELF_EMPLOYED])]);
        assert!(interview.follow_up);
        assert_eq!(
            interview.open_question().unwrap().text,
            "What do you do, in a few words?"
        );
        let interview = answer(interview, typed("I build software"), now()).unwrap();
        assert!(!interview.follow_up);
        assert_eq!(interview.section, 1);
        assert_eq!(
            interview.answers[0].more.as_deref(),
            Some("I build software")
        );

        // Retired needs no follow-up, and living alone has no names to ask for.
        let interview = after(vec![tap(&[RETIRED])]);
        assert_eq!(interview.section, 1);
        let interview = answer(interview, tap(&[JUST_ME]), now()).unwrap();
        assert_eq!(interview.section, 2);
    }

    #[test]
    fn a_skip_moves_on_and_an_empty_answer_is_refused() {
        let interview = after(vec![skip()]);
        assert_eq!(interview.section, 1);
        assert!(interview.answers[0].skipped);
        assert_eq!(
            answer(interview.clone(), AnswerIn::default(), now()),
            Err(Refusal::EmptyAnswer)
        );
        // A tap that is not one of the section's is not an answer.
        assert_eq!(
            answer(interview, tap(&["Astronaut"]), now()),
            Err(Refusal::EmptyAnswer)
        );
        // A skipped follow-up keeps the section's taps.
        let interview = after(vec![tap(&[EMPLOYED]), skip()]);
        assert_eq!(interview.section, 1);
        assert_eq!(interview.answers[0].choices, vec![EMPLOYED.to_string()]);
        assert_eq!(interview.answers[0].more, None);
    }

    #[test]
    fn it_resumes_where_it_stopped_and_starts_again_once_done() {
        let midway = after(vec![skip(), tap(&[JUST_ME])]);
        let (resumed, fresh) = begin(Some(midway.clone()), now());
        assert!(!fresh);
        assert_eq!(resumed, midway);

        let done = MemberInterview {
            status: InterviewStatus::Done,
            ..midway
        };
        let (again, fresh) = begin(Some(done), now());
        assert!(fresh);
        assert_eq!(again.section, 0);
        assert!(again.answers.is_empty());
    }

    #[test]
    fn the_last_answer_ends_the_asking_and_finish_ends_it_early() {
        let all: Vec<AnswerIn> = SECTIONS.iter().map(|_| skip()).collect();
        let interview = after(all);
        assert_eq!(interview.status, InterviewStatus::Thinking);
        assert_eq!(interview.progress(), (SECTIONS.len(), SECTIONS.len()));
        assert_eq!(answer(interview, skip(), now()), Err(Refusal::NothingAsked));

        let early = finish(after(vec![tap(&[RETIRED])])).unwrap();
        assert_eq!(early.status, InterviewStatus::Thinking);
        assert_eq!(finish(MemberInterview::default()), Err(Refusal::NotStarted));
    }

    #[test]
    fn taps_alone_become_facts_without_a_model() {
        let interview = after(vec![
            tap(&[RETIRED]),
            tap(&[PARTNER, PETS]),
            skip(),
            tap(&["Vegetarian", "No dairy", ALLERGIES]),
            typed("peanuts and shellfish"),
            tap(&["Early riser"]),
            tap(&["Sport", "Cooking"]),
            skip(),
            tap(&["Both"]),
            tap(&["Politics", "My health"]),
            tap(&["Short and direct", "Casual"]),
        ]);
        assert_eq!(interview.status, InterviewStatus::Thinking);
        assert!(!interview.has_free_text(), "an allergy is written in code");
        let facts = templated_facts("Jerry", &interview.answers, false);
        let has = |about: About, sentence: &str| {
            assert!(
                facts
                    .iter()
                    .any(|f| f.about == about && f.sentence == sentence),
                "{sentence:?} is not in {facts:?}"
            );
        };
        has(About::Work, "Jerry is retired.");
        has(About::Partner, "Jerry has a partner.");
        has(About::Pet, "Jerry has pets.");
        has(About::Diet, "Jerry is vegetarian and does not eat dairy.");
        has(
            About::Allergy,
            "Jerry is allergic to peanuts and shellfish.",
        );
        has(About::Routine, "Jerry is an early riser.");
        has(About::Likes, "Jerry enjoys sport and cooking.");
        has(
            About::Instruction,
            "Jerry does not want politics and their health brought up.",
        );
        has(
            About::Style,
            "Jerry wants answers short and direct and casual.",
        );
        assert_eq!(news_wish(&interview.answers), Some(NewsWish::Both));
        assert_eq!(NewsWish::Digest.tiles_and_digest(), (false, true));

        let review = into_review(interview, "Jerry", &jerry(), &[], None, now());
        assert_eq!(review.status, InterviewStatus::Review);
        assert_eq!(review.facts.len(), facts.len(), "{:?}", review.facts);
        assert_eq!(review.news, Some(NewsWish::Both));
    }

    #[test]
    fn only_typed_answers_go_to_the_model_with_their_question_and_taps() {
        let taps_only = after(vec![tap(&[RETIRED]), tap(&[JUST_ME])]);
        assert!(!taps_only.has_free_text());
        assert_eq!(build_user_message("Jerry", &taps_only.answers), None);

        let interview = after(vec![
            tap(&[SELF_EMPLOYED]),
            typed("I run a small hardware shop"),
            tap(&[PARTNER]),
            typed("Amina"),
            tap(&["Vegan"]),
        ]);
        assert!(interview.has_free_text());
        let message = build_user_message("Jerry", &interview.answers).unwrap();
        assert!(message.contains(
            "- (you) What do you do most days? They chose: Self-employed. Asked \"What do you \
             do, in a few words?\" they wrote: \"I run a small hardware shop\""
        ));
        assert!(message.contains("they wrote: \"Amina\""));
        assert!(
            !message.contains("(food)"),
            "a tap-only section is not sent"
        );
        assert!(message.contains("Already written, do not repeat:\n- Jerry is vegan.\n"));
        assert!(message.ends_with("JSON:"));

        // With their own words to come, no thin sentence is written for the same thing.
        let facts = templated_facts("Jerry", &interview.answers, false);
        assert_eq!(facts.len(), 1, "{facts:?}");
        // With no reply coming, every tap is written.
        let all: Vec<String> = templated_facts("Jerry", &interview.answers, true)
            .into_iter()
            .map(|f| f.sentence)
            .collect();
        assert_eq!(
            all,
            vec![
                "Jerry is self-employed.",
                "Jerry has a partner.",
                "Jerry is vegan."
            ]
        );
    }

    #[test]
    fn the_prompt_offers_only_words_the_reply_may_use() {
        let prompt = render_system("Jerry");
        for word in [
            "work",
            "partner",
            "child",
            "pet",
            "family",
            "friend",
            "diet",
            "allergy",
            "health",
            "routine",
            "likes",
            "dislikes",
            "place",
            "goal",
            "instruction",
        ] {
            assert!(About::parse(word).is_some());
            assert!(prompt
                .split_whitespace()
                .any(|w| w.trim_end_matches(['.', ',']) == word));
        }
        assert!(!prompt.contains("{name}") && !prompt.contains("{categories}"));
    }

    #[test]
    fn the_review_holds_facts_the_policy_keeps_and_interests_by_name() {
        let interview = after(vec![
            tap(&[SELF_EMPLOYED]),
            typed("I run a small hardware shop"),
            tap(&[JUST_ME]),
        ]);
        let interview = finish(interview).unwrap();
        let written = parse_reply(
            "{\"facts\":[\
             {\"about\":\"work\",\"content\":\"Jerry runs a small hardware shop.\"},\
             {\"about\":\"work\",\"content\":\"Jerry is self-employed.\"},\
             {\"about\":\"work\",\"content\":\"I run a shop.\"},\
             {\"about\":\"sport\",\"content\":\"Jerry likes sport.\"},\
             {\"about\":\"family\",\"content\":\"jerry lives alone\"}],\
             \"interests\":[{\"topic\":\"Arsenal\",\"category\":\"sport\",\"facets\":[\"Saka\"]}]}",
        );
        let review = into_review(interview, "Jerry", &jerry(), &[], written, now());
        assert_eq!(review.status, InterviewStatus::Review);
        let facts: Vec<(&str, About, &str)> = review
            .facts
            .iter()
            .map(|f| (f.id.as_str(), f.about, f.content.as_str()))
            .collect();
        assert_eq!(
            facts,
            vec![
                ("f1", About::Family, "Jerry lives alone."),
                ("f2", About::Work, "Jerry runs a small hardware shop."),
            ],
            "the second work sentence, the first-person one, the unknown word and the repeat \
             are dropped"
        );
        assert_eq!(review.interests.len(), 1);
        assert_eq!(review.interests[0].id, "d1");
        assert_eq!(review.interests[0].topic, "Arsenal");

        let (kept_facts, kept_interests) =
            approved(&review, &["f2".to_string()], &["d1".to_string()]).unwrap();
        assert_eq!(kept_facts.len(), 1);
        assert_eq!(kept_facts[0].content, "Jerry runs a small hardware shop.");
        assert_eq!(kept_interests.len(), 1);
        assert_eq!(
            approved(&MemberInterview::default(), &[], &[]),
            Err(Refusal::NotInReview)
        );
    }

    #[test]
    fn a_draft_never_repeats_or_thins_what_the_pond_already_holds() {
        let interview = finish(after(vec![
            tap(&[EMPLOYED]),
            skip(),
            tap(&[PARTNER, PETS]),
            skip(),
            tap(&["Vegan"]),
        ]))
        .unwrap();
        let known = vec![
            (About::Partner, "Jerry's partner is Amina.".to_string()),
            (About::Diet, "jerry is vegan".to_string()),
        ];
        let review = into_review(interview, "Jerry", &jerry(), &known, None, now());
        let kept: Vec<&str> = review.facts.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(
            kept,
            vec!["Jerry is employed.", "Jerry has pets."],
            "the thin partner sentence and the diet the Pond already holds are left out"
        );
    }

    #[test]
    fn no_reply_still_leaves_what_the_taps_established() {
        let interview = finish(after(vec![tap(&[RETIRED])])).unwrap();
        let unusable = parse_reply("sorry, I cannot");
        assert_eq!(unusable, None);
        let review = into_review(interview, "Jerry", &jerry(), &[], unusable, now());
        assert_eq!(review.facts.len(), 1);
        assert_eq!(review.facts[0].content, "Jerry is retired.");

        // Typed words the model never wrote up do not take the taps with them.
        let interview = finish(after(vec![
            tap(&[SELF_EMPLOYED]),
            typed("I run a small hardware shop"),
        ]))
        .unwrap();
        let review = into_review(interview, "Jerry", &jerry(), &[], None, now());
        assert_eq!(review.facts.len(), 1);
        assert_eq!(review.facts[0].content, "Jerry is self-employed.");
    }

    #[test]
    fn a_write_up_nobody_finished_is_stale_after_the_timeout() {
        let mut interview = finish(after(vec![tap(&[RETIRED])])).unwrap();
        let at = now();
        interview.updated_at = Some(at - chrono::Duration::seconds(THINKING_TIMEOUT_SECS + 5));
        assert!(interview.thinking_is_stale(at));
        interview.updated_at = Some(at);
        assert!(!interview.thinking_is_stale(at));
    }
}
