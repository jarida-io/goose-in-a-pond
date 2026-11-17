//! Structured questions Goose asks a member when details are missing (see
//! docs/architecture/structured-questions.md): up to four to a batch, each with a header chip,
//! two to four options with a description, and a free-text "Other" the surfaces always offer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use super::interest::topic_key;

pub const QUESTIONS_PER_BATCH: usize = 4;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;
pub const MAX_HEADER_CHARS: usize = 12;
pub const MAX_TEXT_CHARS: usize = 200;
pub const MAX_LABEL_CHARS: usize = 60;
const MAX_DESCRIPTION_CHARS: usize = 160;
const MAX_OTHER_CHARS: usize = 500;
pub const MAX_DRAFT_CHARS: usize = 600;
pub const MAX_TABLE_COLUMNS: usize = 4;
pub const MAX_TABLE_ROWS: usize = 6;
const MAX_CELL_CHARS: usize = 60;

/// Rounds a chain runs before its next round is the single "keep going?" question.
pub const SOFT_LIMIT_ROUNDS: u32 = 5;

/// The interest confirmation question's header and options.
pub const INTEREST_HEADER: &str = "Interest";
pub const INTEREST_FOLLOW: &str = "Follow it";
pub const INTEREST_DROP: &str = "Not interested";
pub const INTEREST_FOLLOW_MEANS: &str = "Goose looks out for news, events and ideas about it";
pub const INTEREST_DROP_MEANS: &str = "Goose stops suggesting it and keeps it off your cards";

/// The soft-limit question's gap, header and options.
pub const KEEP_ASKING_GAP: &str = "keep_asking";
pub const KEEP_ASKING_HEADER: &str = "Questions";
pub const START_NOW: &str = "Start now";
pub const ASK_MORE: &str = "Ask more";
const START_NOW_MEANS: &str = "Goose starts with what it has and says what it assumed";
const ASK_MORE_MEANS: &str = "Goose asks a few more questions before it starts";

/// What a surface offers beside every question's options, so no option may mean it.
const OTHER_KEYS: [&str; 5] = ["other", "others", "something else", "else", "none of these"];

/// The key two wordings of one question share.
pub fn text_key(text: &str) -> String {
    topic_key(text)
}

fn one_line(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clipped(raw: &str, max: usize) -> String {
    one_line(raw).chars().take(max).collect()
}

/// Whether an option offers what "Other" already does.
pub fn means_other(label: &str) -> bool {
    let named = label.split('(').next().unwrap_or(label);
    OTHER_KEYS.contains(&topic_key(named).as_str())
}

/// What an option leads to, shown when options are hard to tell apart from their labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Preview {
    Draft {
        text: String,
    },
    Place {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        price: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_url: Option<String>,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

fn web_address(raw: Option<String>) -> Option<String> {
    raw.map(|url| url.trim().to_string())
        .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
}

impl Preview {
    /// Held to its limits; `None` when nothing worth showing is left.
    pub fn cleaned(self) -> Option<Self> {
        match self {
            Self::Draft { text } => {
                let text: String = text.trim().chars().take(MAX_DRAFT_CHARS).collect();
                (!text.is_empty()).then_some(Self::Draft { text })
            }
            Self::Place {
                name,
                price,
                url,
                image_url,
            } => {
                let name = clipped(&name, MAX_LABEL_CHARS);
                (!name.is_empty()).then(|| Self::Place {
                    name,
                    price: price
                        .map(|p| clipped(&p, MAX_CELL_CHARS))
                        .filter(|p| !p.is_empty()),
                    url: web_address(url),
                    image_url: web_address(image_url),
                })
            }
            Self::Table { columns, rows } => {
                let cell = |c: &String| clipped(c, MAX_CELL_CHARS);
                let columns: Vec<String> =
                    columns.iter().take(MAX_TABLE_COLUMNS).map(cell).collect();
                let rows: Vec<Vec<String>> = rows
                    .iter()
                    .take(MAX_TABLE_ROWS)
                    .map(|row| row.iter().take(columns.len()).map(cell).collect())
                    .collect();
                (!columns.is_empty() && !rows.is_empty()).then_some(Self::Table { columns, rows })
            }
        }
    }
}

/// A preview a model got wrong is left off, never the reason its option is lost.
fn lenient_preview<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Preview>, D::Error> {
    let raw = Option::<serde_json::Value>::deserialize(d)?;
    Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
}

/// One answer a member can tap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuestionOption {
    pub label: String,
    /// One line on what choosing it means.
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<Preview>,
}

impl QuestionOption {
    pub fn new(label: &str, description: &str) -> Self {
        Self {
            label: label.to_string(),
            description: description.to_string(),
            preview: None,
        }
    }
}

impl From<String> for QuestionOption {
    fn from(label: String) -> Self {
        Self::new(&label, "")
    }
}

impl From<&str> for QuestionOption {
    fn from(label: &str) -> Self {
        Self::new(label, "")
    }
}

/// A plain string reads as `{label}`: options stored before descriptions existed are strings.
impl<'de> Deserialize<'de> for QuestionOption {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Full {
            #[serde(alias = "text", alias = "option")]
            label: String,
            #[serde(default)]
            description: Option<String>,
            #[serde(default, deserialize_with = "lenient_preview")]
            preview: Option<Preview>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Plain(String),
            Full(Full),
        }
        Ok(match Either::deserialize(d)? {
            Either::Plain(label) => label.into(),
            Either::Full(full) => Self {
                label: full.label,
                description: full.description.unwrap_or_default(),
                preview: full.preview,
            },
        })
    }
}

/// Options as a lenient path keeps them: one line each, deduplicated by key, bounded, and never
/// an "Other".
pub fn clean_options(raw: impl IntoIterator<Item = QuestionOption>) -> Vec<QuestionOption> {
    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .map(|option| QuestionOption {
            label: clipped(&option.label, MAX_LABEL_CHARS),
            description: clipped(&option.description, MAX_DESCRIPTION_CHARS),
            preview: option.preview.and_then(Preview::cleaned),
        })
        .filter(|option| {
            let key = topic_key(&option.label);
            !key.is_empty() && !means_other(&option.label) && seen.insert(key)
        })
        .take(MAX_OPTIONS)
        .collect()
}

/// What an answer feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionPurpose {
    /// Details an anchor's deliverables need.
    Anchor,
    /// Whether a guessed interest is one.
    Interest,
    /// A gap in the fact sheet.
    Profile,
    /// Something the Budget specialist could not read from a capture.
    Ledger,
    /// The chat model asked it mid-turn.
    Chat,
}

impl QuestionPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anchor => "anchor",
            Self::Interest => "interest",
            Self::Profile => "profile",
            Self::Ledger => "ledger",
            Self::Chat => "chat",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "anchor" => Some(Self::Anchor),
            "interest" => Some(Self::Interest),
            "profile" => Some(Self::Profile),
            "ledger" => Some(Self::Ledger),
            "chat" => Some(Self::Chat),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionState {
    Open,
    /// A chat question whose turn stopped waiting; an answer is still taken.
    Waiting,
    Answered,
    Skipped,
    Expired,
}

impl QuestionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Waiting => "waiting",
            Self::Answered => "answered",
            Self::Skipped => "skipped",
            Self::Expired => "expired",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "waiting" => Some(Self::Waiting),
            "answered" => Some(Self::Answered),
            "skipped" => Some(Self::Skipped),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }

    /// Whether the member may still answer it.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open | Self::Waiting)
    }
}

/// What a member answered: options they chose, and their own words under "Other".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
}

impl Answer {
    /// The answer as a brief reads it: the choices, then the member's own words.
    pub fn as_words(&self) -> String {
        let mut parts = self.choices.clone();
        parts.extend(self.text.clone());
        parts.join("; ")
    }

    /// The answer as a chat tool result shows it: `Other: "..."` for the member's own words.
    pub fn as_reply(&self) -> String {
        let mut parts = self.choices.clone();
        if let Some(text) = &self.text {
            parts.push(format!("Other: \"{text}\""));
        }
        parts.join(", ")
    }

    /// Whether `option` is among the choices, however it is spelled.
    pub fn chose(&self, option: &str) -> bool {
        let key = topic_key(option);
        self.choices.iter().any(|choice| topic_key(choice) == key)
    }
}

/// Why an answer was not taken, worded for the member's screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerRefused {
    NotOpen,
    Empty,
    NotAnOption(String),
    OneChoiceOnly,
}

impl std::fmt::Display for AnswerRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOpen => f.write_str("this question is no longer open"),
            Self::Empty => f.write_str("choose an option or write an answer"),
            Self::NotAnOption(choice) => write!(f, "`{choice}` is not one of the options"),
            Self::OneChoiceOnly => f.write_str("this question takes one choice"),
        }
    }
}

impl std::error::Error for AnswerRefused {}

/// A question as the chat model or the engine's maker writes it, before it is checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct QuestionDraft {
    #[serde(default)]
    pub header: String,
    #[serde(default, alias = "question")]
    pub text: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub recommended: Option<usize>,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub gap: Option<String>,
}

/// Why code refused a question (section 2 of the spec), worded for the model that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionRefused {
    NoHeader,
    HeaderTooLong,
    NoText,
    TextTooLong,
    TooFewOptions,
    TooManyOptions,
    NoLabel,
    LabelTooLong(String),
    MeansOther(String),
    SameOption(String),
    /// The same question, or one alike in meaning, was asked already.
    Repeats {
        asked: String,
    },
    /// A stored answer already settles it.
    Settled {
        asked: String,
        answer: String,
    },
}

impl QuestionRefused {
    pub fn message(&self) -> String {
        match self {
            Self::NoHeader => "it has no header: give a chip of up to 12 characters".into(),
            Self::HeaderTooLong => format!("its header is over {MAX_HEADER_CHARS} characters"),
            Self::NoText => "it has no question text".into(),
            Self::TextTooLong => format!("its text is over {MAX_TEXT_CHARS} characters"),
            Self::TooFewOptions => format!("it has fewer than {MIN_OPTIONS} options"),
            Self::TooManyOptions => format!("it has more than {MAX_OPTIONS} options"),
            Self::NoLabel => "an option has no label".into(),
            Self::LabelTooLong(label) => {
                format!("the option \"{label}\" is over {MAX_LABEL_CHARS} characters")
            }
            Self::MeansOther(label) => {
                format!("the option \"{label}\" means Other, which every card already offers")
            }
            Self::SameOption(label) => format!("the option \"{label}\" is listed twice"),
            Self::Repeats { asked } => format!("it repeats a question already asked: \"{asked}\""),
            Self::Settled { asked, answer } => {
                format!("the member already answered \"{asked}\": {answer}")
            }
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Repeats { .. } => "repeats",
            Self::Settled { .. } => "settled",
            _ => "shape",
        }
    }
}

impl QuestionDraft {
    /// A question as a model wrote it in JSON: options as strings or objects, and its pick as a
    /// place or as the label it names. `None` when it is not an object.
    pub fn read(value: &serde_json::Value) -> Option<Self> {
        let item = value.as_object()?;
        let text_at = |key: &str| {
            item.get(key)
                .and_then(serde_json::Value::as_str)
                .map(one_line)
                .unwrap_or_default()
        };
        let options: Vec<QuestionOption> = item
            .get("options")
            .and_then(|o| serde_json::from_value(o.clone()).ok())
            .unwrap_or_default();
        let recommended = match item.get("recommended") {
            Some(serde_json::Value::Number(at)) => {
                at.as_u64().and_then(|at| usize::try_from(at).ok())
            }
            Some(serde_json::Value::String(label)) => {
                let key = topic_key(label);
                options.iter().position(|o| topic_key(&o.label) == key)
            }
            _ => None,
        };
        let text = match text_at("text") {
            text if text.is_empty() => text_at("question"),
            text => text,
        };
        Some(Self {
            header: text_at("header"),
            text,
            options,
            recommended,
            multi: item
                .get("multi")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            gap: Some(text_at("gap")).filter(|g| !g.is_empty()),
        })
    }

    /// The draft held to section 2's shape: refused when it breaks a rule, else trimmed to one
    /// line per field, its previews cut to their limits and a stray recommendation dropped.
    pub fn checked(&self) -> Result<Self, QuestionRefused> {
        let header = one_line(&self.header);
        if topic_key(&header).is_empty() {
            return Err(QuestionRefused::NoHeader);
        }
        if header.chars().count() > MAX_HEADER_CHARS {
            return Err(QuestionRefused::HeaderTooLong);
        }
        let text = one_line(&self.text);
        if topic_key(&text).is_empty() {
            return Err(QuestionRefused::NoText);
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(QuestionRefused::TextTooLong);
        }
        if self.options.len() < MIN_OPTIONS {
            return Err(QuestionRefused::TooFewOptions);
        }
        if self.options.len() > MAX_OPTIONS {
            return Err(QuestionRefused::TooManyOptions);
        }
        let mut keys = std::collections::HashSet::new();
        let mut options = Vec::with_capacity(self.options.len());
        for option in &self.options {
            let label = one_line(&option.label);
            if topic_key(&label).is_empty() {
                return Err(QuestionRefused::NoLabel);
            }
            if label.chars().count() > MAX_LABEL_CHARS {
                return Err(QuestionRefused::LabelTooLong(label));
            }
            if means_other(&label) {
                return Err(QuestionRefused::MeansOther(label));
            }
            if !keys.insert(topic_key(&label)) {
                return Err(QuestionRefused::SameOption(label));
            }
            options.push(QuestionOption {
                label,
                description: clipped(&option.description, MAX_DESCRIPTION_CHARS),
                preview: option.preview.clone().and_then(Preview::cleaned),
            });
        }
        Ok(Self {
            header,
            recommended: self.recommended.filter(|at| *at < options.len()),
            text,
            options,
            multi: self.multi,
            gap: self
                .gap
                .as_deref()
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .map(str::to_string),
        })
    }

    pub fn key(&self) -> String {
        text_key(&self.text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    pub batch_id: String,
    pub purpose: QuestionPurpose,
    #[serde(default)]
    pub anchor_id: Option<String>,
    /// The interest, fact gap or capture the question is about, when it is not an anchor.
    #[serde(default)]
    pub subject_ref: Option<String>,
    /// A chip of at most [`MAX_HEADER_CHARS`] naming what is asked; empty on older rows.
    #[serde(default)]
    pub header: String,
    pub text: String,
    pub options: Vec<QuestionOption>,
    /// The option Goose would pick, by place.
    #[serde(default)]
    pub recommended: Option<usize>,
    /// Whether several options may be chosen.
    pub multi: bool,
    /// The plan detail it settles; background questions only.
    #[serde(default)]
    pub gap: Option<String>,
    /// The chat that asked it.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Its batch's place in the chain of batches for one plan or one chat, from 1.
    #[serde(default = "first_round")]
    pub round: u32,
    /// The question whose free-text answer this one follows up.
    #[serde(default)]
    pub parent_id: Option<String>,
    pub state: QuestionState,
    #[serde(default)]
    pub answer: Option<Answer>,
    /// When the batch was pushed to the member.
    #[serde(default)]
    pub asked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub answered_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

fn first_round() -> u32 {
    1
}

impl Question {
    /// An open question of `batch_id` with no header; `None` when the text has no words or
    /// fewer than [`MIN_OPTIONS`] options are left once cleaned.
    pub fn new<O: Into<QuestionOption> + Clone>(
        profile_id: &str,
        batch_id: &str,
        purpose: QuestionPurpose,
        text: &str,
        options: &[O],
        multi: bool,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        let text = clipped(text, MAX_TEXT_CHARS);
        let options = clean_options(options.iter().cloned().map(Into::into));
        if text_key(&text).is_empty() || options.len() < MIN_OPTIONS {
            return None;
        }
        Some(Self {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            batch_id: batch_id.to_string(),
            purpose,
            anchor_id: None,
            subject_ref: None,
            header: String::new(),
            text,
            options,
            recommended: None,
            multi,
            gap: None,
            session_id: None,
            round: 1,
            parent_id: None,
            state: QuestionState::Open,
            answer: None,
            asked_at: None,
            answered_at: None,
            expires_at: None,
            created_at: now,
        })
    }

    /// An open question from a draft that passed [`QuestionDraft::checked`].
    pub fn from_draft(
        profile_id: &str,
        batch_id: &str,
        purpose: QuestionPurpose,
        draft: &QuestionDraft,
        now: DateTime<Utc>,
    ) -> Result<Self, QuestionRefused> {
        let draft = draft.checked()?;
        let mut question = Self::new(
            profile_id,
            batch_id,
            purpose,
            &draft.text,
            &draft.options,
            draft.multi,
            now,
        )
        .ok_or(QuestionRefused::TooFewOptions)?;
        question.header = draft.header;
        question.recommended = draft.recommended;
        question.gap = draft.gap;
        Ok(question)
    }

    pub fn key(&self) -> String {
        text_key(&self.text)
    }

    pub fn labels(&self) -> Vec<&str> {
        self.options.iter().map(|o| o.label.as_str()).collect()
    }

    /// Takes the member's answer: listed options only, one unless `multi`, or their own words.
    pub fn answer(&mut self, answer: Answer, now: DateTime<Utc>) -> Result<(), AnswerRefused> {
        if !self.state.is_open() {
            return Err(AnswerRefused::NotOpen);
        }
        let mut choices: Vec<String> = Vec::new();
        for choice in &answer.choices {
            let key = topic_key(choice);
            let Some(option) = self.options.iter().find(|o| topic_key(&o.label) == key) else {
                return Err(AnswerRefused::NotAnOption(choice.clone()));
            };
            if !choices.contains(&option.label) {
                choices.push(option.label.clone());
            }
        }
        if choices.len() > 1 && !self.multi {
            return Err(AnswerRefused::OneChoiceOnly);
        }
        let text = answer
            .text
            .map(|t| clipped(&t, MAX_OTHER_CHARS))
            .filter(|t| !t.is_empty());
        if choices.is_empty() && text.is_none() {
            return Err(AnswerRefused::Empty);
        }
        self.answer = Some(Answer { choices, text });
        self.state = QuestionState::Answered;
        self.answered_at = Some(now);
        Ok(())
    }

    /// The member passed on it; `false` when it was no longer open.
    pub fn skip(&mut self, now: DateTime<Utc>) -> bool {
        self.close(QuestionState::Skipped, now)
    }

    /// Nobody answered in time; `false` when it was no longer open.
    pub fn expire(&mut self, now: DateTime<Utc>) -> bool {
        self.close(QuestionState::Expired, now)
    }

    /// The chat turn that asked it stopped waiting; `false` unless it was open.
    pub fn wait(&mut self) -> bool {
        if self.state != QuestionState::Open {
            return false;
        }
        self.state = QuestionState::Waiting;
        true
    }

    fn close(&mut self, state: QuestionState, now: DateTime<Utc>) -> bool {
        if !self.state.is_open() {
            return false;
        }
        self.state = state;
        self.answered_at = Some(now);
        true
    }

    /// Open, with a deadline that has passed.
    pub fn is_overdue(&self, now: DateTime<Utc>) -> bool {
        self.state.is_open() && self.expires_at.is_some_and(|at| at <= now)
    }

    /// An answer in the member's own words, which one follow-up batch may dig into.
    pub fn has_free_text(&self) -> bool {
        self.answer.as_ref().is_some_and(|a| a.text.is_some())
    }

    /// The answer as a tool result line: `Dates: 12-15 Dec`.
    pub fn answer_line(&self) -> Option<String> {
        let answer = self.answer.as_ref()?;
        let named = if self.header.is_empty() {
            &self.text
        } else {
            &self.header
        };
        Some(format!("{named}: {}", answer.as_reply()))
    }
}

/// The interest confirmation question: one to a batch, with what each answer does.
pub fn interest_question(
    profile_id: &str,
    batch_id: &str,
    text: &str,
    now: DateTime<Utc>,
) -> Option<Question> {
    let options = [
        QuestionOption::new(INTEREST_FOLLOW, INTEREST_FOLLOW_MEANS),
        QuestionOption::new(INTEREST_DROP, INTEREST_DROP_MEANS),
    ];
    let mut question = Question::new(
        profile_id,
        batch_id,
        QuestionPurpose::Interest,
        text,
        &options,
        false,
        now,
    )?;
    question.header = INTEREST_HEADER.to_string();
    question.recommended = Some(0);
    Some(question)
}

/// The single question a chain asks once it has run [`SOFT_LIMIT_ROUNDS`]: start now, or ask
/// more. Named for the plan so the store's one-key rule does not take it for another plan's.
pub fn keep_asking_question(
    profile_id: &str,
    batch_id: &str,
    plan_title: &str,
    now: DateTime<Utc>,
) -> Option<Question> {
    let title: String = one_line(plan_title).chars().take(100).collect();
    let text = format!("Goose has enough to start on {title}. Keep answering questions?");
    let options = [
        QuestionOption::new(START_NOW, START_NOW_MEANS),
        QuestionOption::new(ASK_MORE, ASK_MORE_MEANS),
    ];
    let mut question = Question::new(
        profile_id,
        batch_id,
        QuestionPurpose::Anchor,
        &text,
        &options,
        false,
        now,
    )?;
    question.header = KEEP_ASKING_HEADER.to_string();
    question.recommended = Some(0);
    question.gap = Some(KEEP_ASKING_GAP.to_string());
    Some(question)
}

/// Whether round `round` of a chain is the soft-limit question rather than a batch of gaps.
pub fn is_soft_limit_round(round: u32) -> bool {
    round > 0 && round.is_multiple_of(SOFT_LIMIT_ROUNDS + 1)
}

/// One question as a surface asks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuestionView {
    pub id: String,
    pub header: String,
    pub text: String,
    pub options: Vec<QuestionOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<usize>,
    pub multi: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<String>,
}

impl From<&Question> for QuestionView {
    fn from(question: &Question) -> Self {
        Self {
            id: question.id.clone(),
            header: question.header.clone(),
            text: question.text.clone(),
            options: question.options.clone(),
            recommended: question.recommended,
            multi: question.multi,
            gap: question.gap.clone(),
        }
    }
}

/// The open questions of one batch, under the title of what they are about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuestionBatch {
    pub id: String,
    pub purpose: QuestionPurpose,
    pub anchor_id: Option<String>,
    pub session_id: Option<String>,
    pub round: u32,
    pub title: String,
    /// `waiting` once any of its questions is, else `open`.
    pub state: QuestionState,
    pub questions: Vec<QuestionView>,
    #[serde(skip)]
    pub created_at: DateTime<Utc>,
}

/// The open and waiting questions grouped into their batches, newest batch first, at most
/// [`QUESTIONS_PER_BATCH`] each; `title` names what a batch is about from its first question.
pub fn batches(questions: &[Question], title: impl Fn(&Question) -> String) -> Vec<QuestionBatch> {
    let mut out: Vec<QuestionBatch> = Vec::new();
    for question in questions.iter().filter(|q| q.state.is_open()) {
        let at = match out.iter().position(|b| b.id == question.batch_id) {
            Some(at) => at,
            None => {
                out.push(QuestionBatch {
                    id: question.batch_id.clone(),
                    purpose: question.purpose,
                    anchor_id: question.anchor_id.clone(),
                    session_id: question.session_id.clone(),
                    round: question.round,
                    title: title(question),
                    state: QuestionState::Open,
                    questions: Vec::new(),
                    created_at: question.created_at,
                });
                out.len() - 1
            }
        };
        let batch = &mut out[at];
        if question.state == QuestionState::Waiting {
            batch.state = QuestionState::Waiting;
        }
        batch.created_at = batch.created_at.max(question.created_at);
        if batch.questions.len() < QUESTIONS_PER_BATCH {
            batch.questions.push(question.into());
        }
    }
    // Reversed first: the sort is stable, so of batches stored in the same second the one
    // stored last still comes first.
    out.reverse();
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out
}

/// The member's answers to a batch as one chat message, for a turn that stopped waiting.
pub fn answers_message(answered: &[Question]) -> Option<String> {
    let lines: Vec<String> = answered.iter().filter_map(Question::answer_line).collect();
    (!lines.is_empty()).then(|| format!("My answers:\n{}", lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(batch: &str, text: &str, multi: bool) -> Question {
        Question::new(
            "jerry",
            batch,
            QuestionPurpose::Anchor,
            text,
            &["Under KSh 50,000", "KSh 50,000 to 100,000", "More"],
            multi,
            Utc::now(),
        )
        .unwrap()
    }

    fn chose(choices: &[&str], text: Option<&str>) -> Answer {
        Answer {
            choices: choices.iter().map(|c| c.to_string()).collect(),
            text: text.map(str::to_string),
        }
    }

    fn draft(header: &str, text: &str, labels: &[&str]) -> QuestionDraft {
        QuestionDraft {
            header: header.into(),
            text: text.into(),
            options: labels
                .iter()
                .map(|l| QuestionOption::new(l, "means this"))
                .collect(),
            ..QuestionDraft::default()
        }
    }

    #[test]
    fn a_lenient_question_needs_words_and_two_options_and_never_lists_other() {
        let make = |text: &str, raw: &[&str]| {
            Question::new(
                "jerry",
                "b1",
                QuestionPurpose::Profile,
                text,
                raw,
                false,
                Utc::now(),
            )
        };
        assert!(make("  ?? ", &["Yes", "No"]).is_none());
        assert!(make("Do you drive?", &["Yes", "yes", "Other"]).is_none());
        let asked = make("Do you drive?", &["Yes", "No", "Other", "a", "b", "c"]).unwrap();
        assert_eq!(asked.labels(), ["Yes", "No", "a", "b"]);
        assert_eq!(asked.round, 1);
    }

    #[test]
    fn options_read_as_objects_or_plain_strings() {
        let read: Vec<QuestionOption> = serde_json::from_str(
            r#"["Train", {"label": "Fly", "description": "An hour", "preview": {"kind": "draft", "text": "Leave at 7"}},
               {"label": "Drive", "preview": {"kind": "spaceship"}}]"#,
        )
        .unwrap();
        assert_eq!(read[0], QuestionOption::new("Train", ""));
        assert_eq!(read[1].description, "An hour");
        assert_eq!(
            read[1].preview,
            Some(Preview::Draft {
                text: "Leave at 7".into()
            })
        );
        assert_eq!(read[2].preview, None, "a bad preview is left off");
        let back = serde_json::to_value(&read[0]).unwrap();
        assert_eq!(
            back,
            serde_json::json!({"label": "Train", "description": ""})
        );
    }

    #[test]
    fn code_refuses_a_question_that_breaks_the_shape() {
        let ok = draft("Dates", "When do you leave?", &["Friday", "Saturday"]);
        assert!(ok.checked().is_ok());
        let cases: Vec<(QuestionDraft, QuestionRefused)> = vec![
            (draft(" ", "When?", &["A", "B"]), QuestionRefused::NoHeader),
            (
                draft("Travel dates!", "When?", &["A", "B"]),
                QuestionRefused::HeaderTooLong,
            ),
            (
                draft("Dates", &"x".repeat(201), &["A", "B"]),
                QuestionRefused::TextTooLong,
            ),
            (
                draft("Dates", "When?", &["A"]),
                QuestionRefused::TooFewOptions,
            ),
            (
                draft("Dates", "When?", &["A", "B", "C", "D", "E"]),
                QuestionRefused::TooManyOptions,
            ),
            (
                draft("Dates", "When?", &["Friday", "Other (say when)"]),
                QuestionRefused::MeansOther("Other (say when)".into()),
            ),
            (
                draft("Dates", "When?", &["Friday", "friday!"]),
                QuestionRefused::SameOption("friday!".into()),
            ),
            (
                draft("Dates", "When?", &["Friday", &"y".repeat(61)]),
                QuestionRefused::LabelTooLong("y".repeat(61)),
            ),
        ];
        for (asked, refused) in cases {
            assert_eq!(asked.checked(), Err(refused), "{asked:?}");
        }
    }

    #[test]
    fn a_checked_draft_drops_a_stray_pick_and_cuts_its_previews() {
        let mut asked = draft("Stay", "Where do you want to stay?", &["Beach", "Town"]);
        asked.recommended = Some(5);
        asked.options[0].preview = Some(Preview::Table {
            columns: (0..6).map(|n| format!("c{n}")).collect(),
            rows: vec![vec!["1".into(); 6]; 9],
        });
        asked.options[1].preview = Some(Preview::Draft {
            text: "z".repeat(700),
        });
        let checked = asked.checked().unwrap();
        assert_eq!(checked.recommended, None);
        match checked.options[0].preview.as_ref().unwrap() {
            Preview::Table { columns, rows } => {
                assert_eq!(columns.len(), MAX_TABLE_COLUMNS);
                assert_eq!(rows.len(), MAX_TABLE_ROWS);
                assert!(rows.iter().all(|row| row.len() == MAX_TABLE_COLUMNS));
            }
            other => panic!("{other:?}"),
        }
        match checked.options[1].preview.as_ref().unwrap() {
            Preview::Draft { text } => assert_eq!(text.chars().count(), MAX_DRAFT_CHARS),
            other => panic!("{other:?}"),
        }
        asked.recommended = Some(1);
        assert_eq!(asked.checked().unwrap().recommended, Some(1));
    }

    #[test]
    fn an_answer_is_a_listed_option_or_the_members_own_words() {
        let now = Utc::now();
        let mut budget = question("b1", "What is your budget?", false);
        assert_eq!(
            budget.answer(chose(&["A million"], None), now),
            Err(AnswerRefused::NotAnOption("A million".into()))
        );
        assert_eq!(
            budget.answer(chose(&[], Some("  ")), now),
            Err(AnswerRefused::Empty)
        );
        assert_eq!(
            budget.answer(chose(&["More", "under ksh 50,000"], None), now),
            Err(AnswerRefused::OneChoiceOnly)
        );
        budget
            .answer(chose(&["more"], Some("about 150k")), now)
            .unwrap();
        assert_eq!(budget.state, QuestionState::Answered);
        let answer = budget.answer.clone().unwrap();
        assert_eq!(answer.choices, ["More"], "stored as the option is spelled");
        assert_eq!(answer.as_words(), "More; about 150k");
        assert!(budget.has_free_text());
        assert_eq!(
            budget.answer(chose(&["More"], None), now),
            Err(AnswerRefused::NotOpen)
        );
        assert!(!budget.skip(now), "answered already");
    }

    #[test]
    fn a_waiting_question_still_takes_an_answer() {
        let now = Utc::now();
        let mut dates = question("b1", "When do you leave?", false);
        dates.header = "Dates".into();
        assert!(dates.wait());
        assert!(!dates.wait(), "waiting already");
        assert_eq!(dates.state, QuestionState::Waiting);
        dates.answer(chose(&[], Some("about 30k")), now).unwrap();
        assert_eq!(
            dates.answer_line().as_deref(),
            Some("Dates: Other: \"about 30k\"")
        );
    }

    #[test]
    fn several_options_are_taken_only_by_a_multi_question() {
        let mut wants = question("b1", "What matters most?", true);
        wants
            .answer(
                chose(&["More", "Under KSh 50,000", "More"], None),
                Utc::now(),
            )
            .unwrap();
        assert_eq!(wants.answer.unwrap().choices, ["More", "Under KSh 50,000"]);
    }

    #[test]
    fn an_interest_question_is_one_with_two_described_options() {
        let mut asked = interest_question(
            "jerry",
            "b1",
            "Do you want to keep up with Formula 1?",
            Utc::now(),
        )
        .unwrap();
        assert_eq!(asked.header, INTEREST_HEADER);
        assert!(asked.options.iter().all(|o| !o.description.is_empty()));
        asked
            .answer(chose(&["follow it"], None), Utc::now())
            .unwrap();
        let answer = asked.answer.unwrap();
        assert!(answer.chose(INTEREST_FOLLOW));
        assert!(!answer.chose(INTEREST_DROP));
    }

    #[test]
    fn the_soft_limit_falls_on_every_sixth_round() {
        let rounds: Vec<u32> = (1..=13).filter(|r| is_soft_limit_round(*r)).collect();
        assert_eq!(rounds, [6, 12]);
        let keep = keep_asking_question("jerry", "b6", "Diani in April", Utc::now()).unwrap();
        assert_eq!(keep.gap.as_deref(), Some(KEEP_ASKING_GAP));
        assert_eq!(keep.labels(), [START_NOW, ASK_MORE]);
        assert!(keep.text.contains("Diani in April"));
    }

    #[test]
    fn open_and_waiting_questions_are_shown_by_batch_newest_first() {
        let mut questions: Vec<Question> = (0..6)
            .map(|i| question("b1", &format!("Question {i}?"), false))
            .collect();
        let mut later = question("b2", "Another batch?", false);
        later.created_at = questions[0].created_at + chrono::Duration::minutes(1);
        later.wait();
        questions.push(later);
        questions[1].skip(Utc::now());
        let shown = batches(&questions, |q| format!("About {}", q.batch_id));
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].title, "About b2");
        assert_eq!(shown[0].state, QuestionState::Waiting);
        assert_eq!(shown[1].state, QuestionState::Open);
        assert_eq!(shown[1].questions.len(), QUESTIONS_PER_BATCH);
        assert!(shown[1].questions.iter().all(|q| q.text != "Question 1?"));
    }

    #[test]
    fn a_question_is_overdue_only_while_open() {
        let now = Utc::now();
        let mut asked = question("b1", "When do you leave?", false);
        assert!(!asked.is_overdue(now));
        asked.expires_at = Some(now - chrono::Duration::hours(1));
        assert!(asked.is_overdue(now));
        assert!(asked.expire(now));
        assert!(!asked.is_overdue(now));
        assert_eq!(asked.state, QuestionState::Expired);
    }

    #[test]
    fn answers_become_one_message_in_the_members_words() {
        let now = Utc::now();
        let mut dates = question("b1", "When do you leave?", false);
        dates.header = "Dates".into();
        dates.answer(chose(&["More"], None), now).unwrap();
        let mut budget = question("b1", "What is your budget?", false);
        budget.answer(chose(&[], Some("about 30k")), now).unwrap();
        assert_eq!(
            answers_message(&[dates, budget]).unwrap(),
            "My answers:\nDates: More\nWhat is your budget?: Other: \"about 30k\""
        );
        assert_eq!(answers_message(&[]), None);
    }
}
