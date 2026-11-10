//! The interests interview's turns: the prompt that writes the next question, reading its reply,
//! and the fallback question asked when the model's reply cannot be used. One model call per turn,
//! made by the route while the member waits; nothing here calls a model.

use chrono::{DateTime, Utc};

use crate::user_data::domain::interest::{Depth, Interest, MemberPortrait, NewsMode, CATEGORIES};
use crate::user_data::domain::interest_interview::{
    InterestInterview, InterviewNote, InterviewStatus, Question, MAX_QUESTIONS,
};
use crate::user_data::services::model_json::{object_in, text_of, texts_of};

/// Questions the model may end the round on, once it has enough.
pub const MIN_QUESTIONS: u32 = 8;
const MAX_QUESTION_CHARS: usize = 160;
const MAX_CHOICE_CHARS: usize = 40;
const MAX_CHOICES: usize = 6;
/// Memories shown to seed the interview: enough to confirm from, few enough to stay small.
pub const SEED_MEMORIES: usize = 12;

/// System prompt; `{name}` and `{planned}` are filled per member. No worked example: a model
/// this size asks the example's question.
const SYSTEM: &str = "\
You are interviewing {name} about what they are into, so the Pond can bring \
them news and ideas worth their time. One short question at a time, with tap \
answers.

Use these sources, given below: \"Already known\" (the Pond's portrait of \
{name}, their interests on file and their memories), the interview so far, and \
the notes so far.

Plan about {planned} questions:
1. Broad first: what they do for fun, what they follow, what they read, watch \
and play.
2. Then drill into each answer: which teams or players, which writers or \
genres, which games and on what, how closely they follow it, whether they want \
news of it, anything to keep away from.
3. Before question 8, ask how much news they want of their main interests and \
what to keep away from. Last, confirm what you have.
Prioritise what \"Already known\" names: confirm it by name and ask whether it \
is still true, rather than asking cold. When it names interests, the first \
question offers them as choices and asks which {name} still follows. Never ask \
what the notes already settle.

Reply with JSON only, exactly this shape:
{\"notes\":[{\"topic\":\"...\",\"category\":\"...\",\"facets\":[\"...\"],\
\"depth\":\"keen\",\"news\":\"both\",\"avoid\":false}],\"text\":\"...\",\
\"choices\":[\"...\",\"...\"],\"multi\":false,\"allow_free\":true}

- notes: one entry for EVERY topic {name}'s LAST answer named, chose or \
confirmed; [] when it named none. Never repeat the notes so far unless the \
last answer changed them.
  - topic: the specific thing by its own name: a team, a title, a game, a \
writer, a technique, a place. Granular: never a category word, and never a \
tap answer's label when the answer names the real thing.
  - category: one of {categories}.
  - facets: the specific names and angles the answer gave: players, \
platforms, sub-genres, models. May be [].
  - depth: \"curious\", \"keen\" or \"devoted\": how closely they follow it.
  - news: \"off\", \"tiles\", \"digest\" or \"both\": whether they want news \
of it; \"both\" unless they said otherwise.
  - avoid: true only when they want it kept away from them.
- text: the next question, one sentence under 140 characters, specific to \
{name}. One thing at a time. \"\" once at least 8 questions are asked and you \
have enough.
- choices: 2 to 6 tap answers, each one to four words, specific to the \
question: real names from what is known, not generic labels.
- multi: true when more than one choice can apply.
- allow_free: true unless the choices cover every possible answer.";

pub fn render_system(name: &str, planned: u32) -> String {
    SYSTEM
        .replace("{categories}", &CATEGORIES.join(", "))
        .replace("{planned}", &planned.to_string())
        .replace("{name}", name)
}

/// What the Pond already knows about the member, to confirm instead of asking cold.
#[derive(Debug, Clone, Default)]
pub struct KnownAboutMember {
    pub portrait: Option<MemberPortrait>,
    /// Confirmed rows and strong guesses; avoided rows too, so they are not asked about again.
    pub interests: Vec<Interest>,
    /// Memory texts, most important first.
    pub memories: Vec<String>,
}

fn interest_line(interest: &Interest) -> String {
    let mut line = format!("- {} ({}", interest.topic, interest.category);
    if !interest.facets.is_empty() {
        line.push_str(&format!("; {}", interest.facets.join(", ")));
    }
    if interest.avoid {
        line.push_str("; keep away");
    } else if interest.confirmed {
        line.push_str(&format!("; {}", interest.depth.as_str()));
    } else {
        line.push_str(&format!("; a guess from {} conversations", interest.evidence));
    }
    line.push(')');
    line
}

pub fn build_user_message(name: &str, known: &KnownAboutMember, interview: &InterestInterview) -> String {
    let mut out = String::from("Already known:\n");
    let mut said_anything = false;
    if let Some(portrait) = &known.portrait {
        out.push_str(&portrait.as_prompt_lines());
        out.push('\n');
        said_anything = true;
    }
    if !known.interests.is_empty() {
        out.push_str("Interests on file:\n");
        for interest in &known.interests {
            out.push_str(&interest_line(interest));
            out.push('\n');
        }
        said_anything = true;
    }
    if !known.memories.is_empty() {
        out.push_str("Memories:\n");
        for memory in known.memories.iter().take(SEED_MEMORIES) {
            out.push_str(&format!("- {}\n", memory.trim()));
        }
        said_anything = true;
    }
    if !said_anything {
        out.push_str("Nothing yet.\n");
    }

    out.push_str(&format!(
        "\nThe interview so far ({} of about {} questions asked):\n",
        interview.asked, interview.planned
    ));
    if interview.turns.is_empty() {
        out.push_str("Nothing yet: write the first question.\n");
    }
    for turn in &interview.turns {
        let speaker = match turn.role {
            crate::user_data::domain::interest_interview::Speaker::Goose => "Pond",
            crate::user_data::domain::interest_interview::Speaker::You => name,
        };
        out.push_str(&format!("{speaker}: {}\n", turn.text.trim()));
    }

    out.push_str("\nNotes so far:\n");
    if interview.draft.is_empty() {
        out.push_str("None.\n");
    }
    for draft in &interview.draft {
        out.push_str(&interest_line(draft));
        out.push('\n');
    }
    out.push_str("\nJSON:");
    out
}

/// What the model's reply came to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InterviewReply {
    pub notes: Vec<InterviewNote>,
    /// `None` when the model said it has enough, or wrote no usable question.
    pub question: Option<Question>,
}

/// `None` when the reply holds no JSON object at all.
pub fn parse_reply(raw: &str) -> Option<InterviewReply> {
    let map = object_in(raw)?;
    let notes = map
        .get("notes")
        .and_then(|n| n.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_object())
                .filter_map(|item| {
                    let topic = text_of(item, "topic")?;
                    Some(InterviewNote {
                        topic,
                        category: text_of(item, "category"),
                        facets: texts_of(item, "facets"),
                        depth: text_of(item, "depth").and_then(|d| Depth::parse(&d)),
                        news: text_of(item, "news").and_then(|n| NewsMode::parse(&n)),
                        avoid: item.get("avoid").and_then(|a| a.as_bool()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let question = text_of(&map, "text")
        .filter(|text| text.chars().count() <= MAX_QUESTION_CHARS)
        .map(|text| {
            let choices: Vec<String> = texts_of(&map, "choices")
                .into_iter()
                .filter(|c| c.chars().count() <= MAX_CHOICE_CHARS)
                .take(MAX_CHOICES)
                .collect();
            let allow_free = map
                .get("allow_free")
                .and_then(|a| a.as_bool())
                .unwrap_or(true);
            Question {
                text,
                // Fewer than two choices is not a tap answer: the member types instead.
                allow_free: allow_free || choices.len() < 2,
                multi: map.get("multi").and_then(|m| m.as_bool()).unwrap_or(false)
                    && choices.len() >= 2,
                choices: if choices.len() >= 2 { choices } else { Vec::new() },
            }
        });
    Some(InterviewReply { notes, question })
}

/// The broad questions a round can always fall back on, in order.
fn broad_questions() -> Vec<Question> {
    let q = |text: &str, choices: &[&str], multi: bool| Question {
        text: text.to_string(),
        choices: choices.iter().map(|c| c.to_string()).collect(),
        multi,
        allow_free: true,
    };
    vec![
        q(
            "What do you enjoy doing when you have a free evening?",
            &["Watching shows", "Gaming", "Reading", "Sport", "Music", "Going out"],
            true,
        ),
        q(
            "Which sports, teams or players do you follow, if any?",
            &["Football", "Formula 1", "Athletics", "Basketball", "None"],
            true,
        ),
        q(
            "What do you like to watch at the moment?",
            &["Series", "Films", "Documentaries", "Anime", "Not much"],
            true,
        ),
        q(
            "Which games do you play, and on what?",
            &["PlayStation", "PC", "Phone", "Xbox", "I don't play"],
            true,
        ),
        q(
            "What do you read, or listen to, to learn things?",
            &["Tech", "Science", "Business", "Politics", "History"],
            true,
        ),
        q(
            "Is there anything you are working on or planning right now?",
            &["A project", "A purchase", "A trip", "An event"],
            true,
        ),
        q(
            "Anything you would rather I never bring up?",
            &["Celebrity gossip", "Politics", "Sport", "Nothing"],
            true,
        ),
        q(
            "Anything else you are into that I have not asked about?",
            &[],
            false,
        ),
    ]
}

/// A question that moves the round on when the model's reply cannot be used: a news question
/// about the newest draft topic, else the next broad question not yet asked; `None` once every
/// one has been asked.
pub fn fallback_question(interview: &InterestInterview) -> Option<Question> {
    let asked: Vec<&str> = interview
        .turns
        .iter()
        .filter(|t| t.role == crate::user_data::domain::interest_interview::Speaker::Goose)
        .map(|t| t.text.as_str())
        .collect();
    if let Some(latest) = interview.draft.last() {
        let text = format!("How much do you want to hear about {}?", latest.topic);
        if !asked.contains(&text.as_str()) {
            return Some(Question {
                text,
                choices: vec![
                    "Every day".to_string(),
                    "Now and then".to_string(),
                    "No news, thanks".to_string(),
                ],
                multi: false,
                allow_free: true,
            });
        }
    }
    broad_questions()
        .into_iter()
        .find(|q| !asked.contains(&q.text.as_str()))
}

/// Why an interview request cannot go ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterviewRefusal {
    /// No question is waiting for an answer.
    NothingToAnswer,
    /// The answer was empty.
    EmptyAnswer,
    /// The draft is not ready for review.
    NotInReview,
}

impl InterviewRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            InterviewRefusal::NothingToAnswer => "no question is waiting for an answer",
            InterviewRefusal::EmptyAnswer => "an answer needs text or a choice",
            InterviewRefusal::NotInReview => "the interview has no draft to approve yet",
        }
    }
}

/// `POST /interview`: the interview to show and whether a question must be written now.
pub fn begin(existing: Option<InterestInterview>) -> (InterestInterview, bool) {
    let interview = existing.unwrap_or_default();
    match interview.status {
        InterviewStatus::NotStarted => {
            let mut fresh = InterestInterview {
                round: interview.round.max(1),
                ..InterestInterview::default()
            };
            fresh.status = InterviewStatus::Thinking;
            (fresh, true)
        }
        InterviewStatus::Done => {
            let mut next = interview.next_round();
            next.status = InterviewStatus::Thinking;
            (next, true)
        }
        // A question is waiting, or the draft is in review: resume where it is.
        InterviewStatus::Asking | InterviewStatus::Review => (interview, false),
        InterviewStatus::Thinking => (interview, false),
    }
}

/// The member's answer as one line: their taps, then anything they typed.
pub fn answer_text(text: Option<&str>, choices: &[String]) -> Option<String> {
    let taps = choices
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let typed = text.map(str::trim).unwrap_or_default();
    let line = match (taps.is_empty(), typed.is_empty()) {
        (true, true) => return None,
        (false, true) => taps,
        (true, false) => typed.to_string(),
        (false, false) => format!("{taps}. {typed}"),
    };
    Some(line.chars().take(600).collect())
}

/// `POST /interview/answer`: records the answer and sets the interview thinking.
pub fn record_answer(
    mut interview: InterestInterview,
    answer: Option<String>,
    now: DateTime<Utc>,
) -> Result<InterestInterview, InterviewRefusal> {
    if interview.status != InterviewStatus::Asking || interview.open_question().is_none() {
        return Err(InterviewRefusal::NothingToAnswer);
    }
    let answer = answer.ok_or(InterviewRefusal::EmptyAnswer)?;
    interview.push_answer(answer, now);
    interview.status = InterviewStatus::Thinking;
    Ok(interview)
}

/// Folds the model's reply (or its absence) into the interview: the notes into the draft, then
/// the next question, or the review once the round has asked enough.
pub fn apply_reply(
    mut interview: InterestInterview,
    reply: Option<InterviewReply>,
    now: DateTime<Utc>,
) -> InterestInterview {
    let reply = reply.unwrap_or_default();
    interview.apply_notes(&reply.notes, now);
    let model_is_done = reply.question.is_none() && interview.asked >= MIN_QUESTIONS;
    if interview.has_asked_enough() || model_is_done || interview.asked >= MAX_QUESTIONS {
        interview.status = InterviewStatus::Review;
        return interview;
    }
    let question = reply
        .question
        .filter(|q| {
            !interview
                .turns
                .iter()
                .any(|t| t.text.eq_ignore_ascii_case(&q.text))
        })
        .or_else(|| fallback_question(&interview));
    let Some(question) = question else {
        interview.status = InterviewStatus::Review;
        return interview;
    };
    interview.push_question(question, now);
    interview.status = InterviewStatus::Asking;
    interview
}

/// `POST /interview/finish`: straight to review with whatever the draft holds.
pub fn finish(mut interview: InterestInterview) -> InterestInterview {
    if matches!(
        interview.status,
        InterviewStatus::Asking | InterviewStatus::Thinking
    ) {
        interview.status = InterviewStatus::Review;
    }
    interview
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::interest_interview::Speaker;

    #[test]
    fn a_clean_reply_gives_notes_and_the_next_question() {
        let raw = r#"```json
{"notes":[{"topic":"The Boys","category":"film_tv","facets":["Homelander","Gen V"],"depth":"devoted","news":"both","avoid":false}],
 "text":"Which games do you play right now?","choices":["GTA V","inFAMOUS","None"],"multi":true,"allow_free":true}
```"#;
        let reply = parse_reply(raw).unwrap();
        assert_eq!(reply.notes.len(), 1);
        assert_eq!(reply.notes[0].depth, Some(Depth::Devoted));
        let q = reply.question.unwrap();
        assert_eq!(q.choices.len(), 3);
        assert!(q.multi);
    }

    #[test]
    fn one_choice_is_not_a_tap_answer() {
        let reply =
            parse_reply(r#"{"notes":[],"text":"What else?","choices":["Yes"],"allow_free":false}"#)
                .unwrap();
        let q = reply.question.unwrap();
        assert!(q.choices.is_empty());
        assert!(q.allow_free, "the member must be able to answer somehow");
    }

    #[test]
    fn rubbish_moves_the_round_on_with_a_fallback_question() {
        assert!(parse_reply("Sorry, I can't do that.").is_none());
        let mut interview = InterestInterview::default();
        interview.status = InterviewStatus::Thinking;
        let after = apply_reply(interview, None, Utc::now());
        assert_eq!(after.status, InterviewStatus::Asking);
        assert_eq!(after.asked, 1);
        assert!(after.open_question().is_some(), "never stuck in thinking");
    }

    #[test]
    fn a_fallback_never_repeats_a_question() {
        let mut interview = InterestInterview::default();
        let now = Utc::now();
        while let Some(q) = fallback_question(&interview) {
            assert!(
                !interview.turns.iter().any(|t| t.text == q.text),
                "{} was asked twice",
                q.text
            );
            interview.push_question(q, now);
            interview.push_answer("no".into(), now);
        }
    }

    #[test]
    fn the_round_goes_to_review_once_it_has_asked_enough() {
        let now = Utc::now();
        let mut interview = InterestInterview::default();
        for i in 0..interview.planned {
            interview.push_question(
                Question {
                    text: format!("question {i}?"),
                    choices: vec![],
                    multi: false,
                    allow_free: true,
                },
                now,
            );
            interview.push_answer("answer".into(), now);
        }
        let after = apply_reply(
            interview,
            Some(InterviewReply {
                notes: vec![InterviewNote {
                    topic: "GTA V".into(),
                    ..Default::default()
                }],
                question: Some(Question {
                    text: "One more?".into(),
                    choices: vec![],
                    multi: false,
                    allow_free: true,
                }),
            }),
            now,
        );
        assert_eq!(after.status, InterviewStatus::Review);
        assert_eq!(after.draft.len(), 1, "the last answer's notes still count");
    }

    #[test]
    fn an_answer_needs_a_question_waiting_and_some_words() {
        let now = Utc::now();
        let fresh = InterestInterview::default();
        assert_eq!(
            record_answer(fresh, Some("x".into()), now).unwrap_err(),
            InterviewRefusal::NothingToAnswer
        );
        let mut asking = InterestInterview::default();
        asking.push_question(fallback_question(&asking).unwrap(), now);
        asking.status = InterviewStatus::Asking;
        assert_eq!(
            record_answer(asking.clone(), None, now).unwrap_err(),
            InterviewRefusal::EmptyAnswer
        );
        let thinking = record_answer(asking, Some("Gaming".into()), now).unwrap();
        assert_eq!(thinking.status, InterviewStatus::Thinking);
        assert_eq!(thinking.turns.last().unwrap().role, Speaker::You);
    }

    #[test]
    fn taps_and_typing_make_one_answer() {
        assert_eq!(
            answer_text(Some("mostly on PS5"), &["GTA V".into(), "inFAMOUS".into()]).as_deref(),
            Some("GTA V, inFAMOUS. mostly on PS5")
        );
        assert_eq!(answer_text(Some("  "), &[]), None);
    }

    #[test]
    fn a_finished_round_starts_a_new_one_and_a_waiting_question_resumes() {
        let done = InterestInterview {
            status: InterviewStatus::Done,
            round: 2,
            ..Default::default()
        };
        let (next, ask) = begin(Some(done));
        assert!(ask);
        assert_eq!(next.round, 3);
        assert_eq!(next.status, InterviewStatus::Thinking);

        let mut waiting = InterestInterview::default();
        waiting.push_question(fallback_question(&waiting).unwrap(), Utc::now());
        waiting.status = InterviewStatus::Asking;
        let (same, ask) = begin(Some(waiting.clone()));
        assert!(!ask);
        assert_eq!(same.turns, waiting.turns);
    }

    #[test]
    fn the_prompt_names_its_sources_and_defines_every_field() {
        let prompt = render_system("Jerry", 10);
        assert!(prompt.contains("Use these sources"));
        for field in [
            "notes:", "topic:", "category:", "facets:", "depth:", "news:", "avoid:", "text:",
            "choices:", "multi:", "allow_free:",
        ] {
            assert!(prompt.contains(field), "{field} is read and never defined");
        }
        assert!(!prompt.contains("{name}") && !prompt.contains("{planned}"));
        assert!(prompt.contains("about 10 questions"));
    }

    #[test]
    fn the_seed_names_what_is_known_so_the_model_confirms_it() {
        let known = KnownAboutMember {
            portrait: None,
            interests: vec![Interest {
                id: "i".into(),
                profile_id: "p".into(),
                topic: "The Boys".into(),
                category: "film_tv".into(),
                facets: vec!["Gen V".into()],
                depth: Depth::Keen,
                news: NewsMode::Both,
                avoid: false,
                origin: crate::user_data::domain::interest::Origin::Chat,
                evidence: 3,
                confirmed: false,
                picture: None,
                updated_at: Utc::now(),
            }],
            memories: vec!["Jerry is shopping for a Toyota Fielder.".into()],
        };
        let message = build_user_message("Jerry", &known, &InterestInterview::default());
        assert!(message.contains("- The Boys (film_tv; Gen V; a guess from 3 conversations)"));
        assert!(message.contains("- Jerry is shopping for a Toyota Fielder."));
        assert!(message.contains("write the first question"));
    }
}
