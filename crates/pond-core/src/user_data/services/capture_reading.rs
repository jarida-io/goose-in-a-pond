//! Reading one picture a member's phone sent: what the maker is asked, how its reply and the
//! critic's are read, and whether the reading is kept, forgotten or tried again.

use serde_json::{Map, Value};

use super::extraction_gate::{plan_in, section, PlanCandidate, REPLY_CUE};
use super::screenshot_notes::keeps_private;
use crate::user_data::domain::capture::{CaptureDetails, Sensitivity};
use crate::user_data::services::model_json::{object_in, text_of, texts_of};

const MAX_DESCRIPTION_CHARS: usize = 300;
const MAX_INTENT_CHARS: usize = 160;
/// Items kept in each list of details.
pub const MAX_DETAIL_ITEMS: usize = 8;
const MAX_DETAIL_CHARS: usize = 80;
const MAX_LINK_CHARS: usize = 300;
const MAX_REASON_CHARS: usize = 200;
/// A run of digits this long is a phone, card or account number, never a price or a date.
const LONG_NUMBER_DIGITS: usize = 9;

/// Fixed so it stays in the KV prefix. It shows no worked reading: a small model copies one.
pub const READ_SYSTEM: &str = "\
You are shown one picture from the phone of a member of this household: a \
screenshot or a photo. Say what it shows and what the member most likely \
wants from it, so Goose can help with it later.

First decide how sensitive it is:
- private: a login or password screen, a code sent to them, a private \
conversation, an identity or medical document.
- money: a receipt, a payment or transfer message, a bank or mobile-money screen.
- none: anything else.
When unsure between two, choose the more sensitive.

Reply with JSON and nothing else:

{\"sensitive\": \"none\" or \"money\" or \"private\", \"description\": \"<what it shows>\", \"intent\": \"<what they want from it>\", \"details\": {\"names\": [], \"dates\": [], \"places\": [], \"prices\": [], \"links\": []}, \"plan\": null}

- For \"private\", leave every other field empty: describe nothing.
- \"description\": what the picture shows, by its name: the event, place, \
product, article or topic. Under 200 characters. Only what is on the \
picture, never a guess.
- \"intent\": what the member most likely wants from it, in a few words. \
Under 100 characters.
- \"details\": the names, dates, places, prices and links the picture \
writes, as written, at most 8 of each. Never a private person's name, a \
phone number, an email or an account number.
- \"plan\": only when the picture shows one named thing they could attend, \
go on, buy or build: {\"kind\": \"event\" or \"trip\" or \"purchase\" or \
\"project\", \"title\": \"<its name>\", \"when\": \"<the date as written>\", \
\"where\": \"<the place>\", \"details\": \"<one line>\"}. Otherwise null.
- No explanation, no preamble.";

/// What goes beside the picture in the maker's call.
pub const READ_MESSAGE: &str = "The picture is attached.\n\nJSON:";

/// What a maker made of one picture.
#[derive(Debug, Clone, PartialEq, Eq)]
// Made once per picture and moved straight into its capture; boxing would buy nothing.
#[allow(clippy::large_enum_variant)]
pub enum Reading {
    /// Nothing about the picture may be kept.
    Private,
    Kept {
        sensitive: Sensitivity,
        description: String,
        /// What the member seems to want with it; may be empty.
        intent: String,
        details: CaptureDetails,
        plan: Option<PlanCandidate>,
    },
}

fn letters(raw: &str) -> String {
    raw.chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn clipped(text: String, max: usize) -> String {
    text.chars().take(max).collect()
}

fn has_long_number(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_digit())
        .any(|run| run.len() >= LONG_NUMBER_DIGITS)
}

fn items(map: &Map<String, Value>, key: &str, max_chars: usize, link: bool) -> Vec<String> {
    texts_of(map, key)
        .into_iter()
        .filter(|item| !item.starts_with('<'))
        // A link may carry a long id or an @ in its path; anything else that does is a
        // phone number, an account or an address for mail.
        .filter(|item| link || !(item.contains('@') || has_long_number(item)))
        .map(|item| clipped(item, max_chars))
        .take(MAX_DETAIL_ITEMS)
        .collect()
}

fn details_in(map: &Map<String, Value>) -> CaptureDetails {
    let Some(details) = map.get("details").and_then(Value::as_object) else {
        return CaptureDetails::default();
    };
    CaptureDetails {
        names: items(details, "names", MAX_DETAIL_CHARS, false),
        dates: items(details, "dates", MAX_DETAIL_CHARS, false),
        places: items(details, "places", MAX_DETAIL_CHARS, false),
        prices: items(details, "prices", MAX_DETAIL_CHARS, false),
        links: items(details, "links", MAX_LINK_CHARS, true),
    }
}

/// Whether prose about a picture judged harmless carries what a private screen has.
fn reads_as_private(prose: &str) -> bool {
    // Prices are wanted here, so the guard's rule on numbers is left to `has_long_number`.
    let without_digits: String = prose.chars().filter(|c| !c.is_ascii_digit()).collect();
    keeps_private(&without_digits) || has_long_number(prose)
}

/// Reads a maker's reply. `None` when it names no sensitivity, or keeps a picture it does not
/// describe; a money picture needs no description, since none is kept.
pub fn parse_reading(raw: &str) -> Option<Reading> {
    let map = object_in(raw)?;
    let sensitive = match letters(map.get("sensitive")?.as_str()?).as_str() {
        "private" => return Some(Reading::Private),
        "money" => Sensitivity::Money,
        "none" => Sensitivity::None,
        _ => return None,
    };
    let description = text_of(&map, "description")
        .map(|text| clipped(text, MAX_DESCRIPTION_CHARS))
        .unwrap_or_default();
    let intent = text_of(&map, "intent")
        .map(|text| clipped(text, MAX_INTENT_CHARS))
        .unwrap_or_default();
    if sensitive == Sensitivity::None {
        if description.is_empty() {
            return None;
        }
        if reads_as_private(&format!("{description} {intent}")) {
            return Some(Reading::Private);
        }
    }
    Some(Reading::Kept {
        sensitive,
        description,
        intent,
        details: details_in(&map),
        plan: map.get("plan").and_then(plan_in),
    })
}

/// Fixed so it stays in the KV prefix.
pub const READ_CRITIC_SYSTEM: &str = "\
You are shown one picture from the phone of a member of this household, and \
what another reader wrote about it. Check that reading against the picture \
before anything is kept. You are the last check, so look for what is wrong \
with it.

Answer \"private\" when the picture is a login or password screen, a code \
sent to them, a private conversation, or an identity or medical document, \
whatever the reading says. Nothing about it is then kept.

Answer \"disagree\" when any of these is true:
- the reading names something the picture does not show;
- a name, date, place, price or link in it is not what the picture writes;
- it calls the picture \"none\" when it shows a receipt, a payment or \
transfer message, or a bank or mobile-money screen, or \"money\" when it \
shows none of those;
- its plan is not something the picture shows.

Answer \"agree\" otherwise. Wording you would have chosen differently is not \
a reason to disagree.

Reply with JSON and nothing else:

{\"verdict\": \"agree\" or \"disagree\" or \"private\", \"why\": \"<a few words>\"}

- \"why\": one line, under 150 characters; \"\" when you agree.
- No explanation, no preamble.";

const READING_HEADING: &str = "The reading to check:";

/// The critic's user message, sent with the same picture the maker saw.
pub fn build_read_critic_message(reading: &Reading) -> String {
    let body = match reading {
        Reading::Private => "Sensitive: private".to_string(),
        Reading::Kept {
            sensitive,
            description,
            intent,
            details,
            plan,
        } => {
            let mut lines = vec![format!("Sensitive: {}", sensitive.as_str())];
            if !description.is_empty() {
                lines.push(format!("Shows: {description}"));
            }
            if !intent.is_empty() {
                lines.push(format!("They want: {intent}"));
            }
            if !details.is_empty() {
                lines.push(details.as_line());
            }
            if let Some(plan) = plan {
                lines.push(format!("Plan: {}", plan.as_line()));
            }
            lines.join("\n")
        }
    };
    let mut out = String::new();
    section(&mut out, READING_HEADING, &body);
    out.push_str("The picture is attached.\n\n");
    out.push_str(REPLY_CUE);
    out
}

/// A critic's ruling on a reading of the picture it was shown too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadVerdict {
    Agree,
    /// With what it found wrong.
    Disagree(String),
    /// The picture is private, whatever the reading kept.
    Private,
    Unreadable,
}

pub fn parse_read_verdict(raw: &str) -> ReadVerdict {
    let Some(map) = object_in(raw) else {
        return ReadVerdict::Unreadable;
    };
    let said = map
        .get("verdict")
        .and_then(Value::as_str)
        .map(letters)
        .unwrap_or_default();
    match said.as_str() {
        "agree" | "agrees" | "agreed" => ReadVerdict::Agree,
        "disagree" | "disagrees" | "disagreed" => ReadVerdict::Disagree(
            text_of(&map, "why")
                .map(|why| clipped(why, MAX_REASON_CHARS))
                .unwrap_or_default(),
        ),
        "private" => ReadVerdict::Private,
        _ => ReadVerdict::Unreadable,
    }
}

/// What becomes of a picture once its maker and its critic have answered.
#[derive(Debug, Clone, PartialEq, Eq)]
// Made once per picture and matched on at once; boxing would buy nothing.
#[allow(clippy::large_enum_variant)]
pub enum ReadOutcome {
    Keep(Reading),
    /// The picture and its row are deleted.
    Forget,
    /// Read it again, while it has tries left.
    Retry,
}

/// Forgets when either says private, keeps a reading the critic agrees with, and tries again
/// on a disagreement, an unreadable ruling or no reading.
pub fn settle_reading(maker: Option<Reading>, critic: ReadVerdict) -> ReadOutcome {
    match (maker, critic) {
        (Some(Reading::Private), _) | (_, ReadVerdict::Private) => ReadOutcome::Forget,
        (Some(reading), ReadVerdict::Agree) => ReadOutcome::Keep(reading),
        _ => ReadOutcome::Retry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::AnchorKind;

    const FLIGHT: &str = r#"{"sensitive": "none",
        "description": "A Jambojet flight from Nairobi\n to Kisumu on 12 December",
        "intent": "to book the flight",
        "details": {"names": ["Jambojet"], "dates": ["12 December"], "places": ["Nairobi", "Kisumu"],
                    "prices": ["KSh 8,400"], "links": ["https://www.jambojet.com/book/123456789012"]},
        "plan": {"kind": "trip", "title": "Kisumu in December", "when": "12 December",
                 "where": "Kisumu", "details": "Jambojet, KSh 8,400"}}"#;

    fn flight() -> Reading {
        parse_reading(FLIGHT).expect("the flight reads")
    }

    #[test]
    fn a_harmless_picture_reads_with_its_description_intent_details_and_plan() {
        let Reading::Kept {
            sensitive,
            description,
            intent,
            details,
            plan,
        } = flight()
        else {
            panic!("the flight is kept");
        };
        assert_eq!(sensitive, Sensitivity::None);
        assert_eq!(
            description,
            "A Jambojet flight from Nairobi to Kisumu on 12 December"
        );
        assert_eq!(intent, "to book the flight");
        assert_eq!(details.names, ["Jambojet"]);
        assert_eq!(details.places, ["Nairobi", "Kisumu"]);
        assert_eq!(details.prices, ["KSh 8,400"]);
        assert_eq!(
            details.links,
            ["https://www.jambojet.com/book/123456789012"]
        );
        let plan = plan.expect("the trip is a plan");
        assert_eq!(plan.kind, AnchorKind::Trip);
        assert_eq!(plan.title, "Kisumu in December");
        assert_eq!(plan.place, "Kisumu");
    }

    #[test]
    fn a_private_picture_keeps_nothing_whatever_else_the_reply_says() {
        for raw in [
            r#"{"sensitive": "private"}"#,
            r#"{"sensitive": "Private.", "description": "A WhatsApp chat with Amina", "details": {"names": ["Amina"]}}"#,
            "```json\n{\"sensitive\": \"private\", \"description\": \"\", \"plan\": null}\n```",
        ] {
            assert_eq!(parse_reading(raw), Some(Reading::Private), "{raw}");
        }
    }

    #[test]
    fn a_money_picture_is_kept_as_money_with_or_without_a_description() {
        let described = parse_reading(
            r#"{"sensitive": "money", "description": "An M-Pesa message, balance shown", "intent": ""}"#,
        );
        assert!(matches!(
            described,
            Some(Reading::Kept {
                sensitive: Sensitivity::Money,
                ..
            })
        ));
        let bare = parse_reading(r#"{"sensitive": "money"}"#);
        assert_eq!(
            bare,
            Some(Reading::Kept {
                sensitive: Sensitivity::Money,
                description: String::new(),
                intent: String::new(),
                details: CaptureDetails::default(),
                plan: None,
            })
        );
    }

    #[test]
    fn a_reply_with_no_description_or_no_sensitivity_is_no_reading() {
        for raw in [
            "",
            "I cannot see a picture.",
            r#"{"sensitive": "none"}"#,
            r#"{"sensitive": "none", "description": " "}"#,
            r#"{"sensitive": "none", "description": "<what it shows>"}"#,
            r#"{"description": "A poster for the AI Summit"}"#,
            r#"{"sensitive": "low", "description": "A poster for the AI Summit"}"#,
            r#"{"sensitive": null, "description": "A poster for the AI Summit"}"#,
            READ_SYSTEM,
        ] {
            assert_eq!(parse_reading(raw), None, "{raw}");
        }
    }

    #[test]
    fn a_harmless_reading_that_describes_a_private_screen_is_private() {
        for description in [
            "The login page of a bank",
            "A message from Amina about dinner",
            "A prescription for amoxicillin",
            "A contact card for amina@example.com",
            "A delivery note with the number 0712345678",
        ] {
            let raw = format!(r#"{{"sensitive": "none", "description": "{description}"}}"#);
            assert_eq!(parse_reading(&raw), Some(Reading::Private), "{description}");
        }
        let priced = parse_reading(
            r#"{"sensitive": "none", "description": "A Toyota Fielder 2015 for KSh 1,250,000 on Kilimall"}"#,
        );
        assert!(
            matches!(priced, Some(Reading::Kept { .. })),
            "a price is not a private number"
        );
    }

    #[test]
    fn details_are_capped_and_never_carry_a_number_or_an_address_for_mail() {
        let names: Vec<String> = (1..=12).map(|n| format!("\"Speaker {n}\"")).collect();
        let raw = format!(
            r#"{{"sensitive": "none", "description": "{}", "intent": "{}",
                "details": {{"names": [{}, "amina@example.com", "0712 345678901"],
                             "dates": ["...", " ", "<the date>", "3 December"],
                             "places": "KICC",
                             "prices": ["{}"],
                             "links": ["https://medium.com/@jerry/a-post", "{}"]}},
                "plan": {{"kind": "holiday", "title": "Diani"}}}}"#,
            "d".repeat(900),
            "i".repeat(900),
            names.join(", "),
            "p".repeat(200),
            "l".repeat(900),
        );
        let Some(Reading::Kept {
            description,
            intent,
            details,
            plan,
            ..
        }) = parse_reading(&raw)
        else {
            panic!("the reading is kept");
        };
        assert_eq!(description.chars().count(), MAX_DESCRIPTION_CHARS);
        assert_eq!(intent.chars().count(), MAX_INTENT_CHARS);
        assert_eq!(details.names.len(), MAX_DETAIL_ITEMS);
        assert_eq!(details.names[7], "Speaker 8");
        assert_eq!(details.dates, ["3 December"]);
        assert!(details.places.is_empty(), "a list was asked for");
        assert_eq!(details.prices[0].chars().count(), MAX_DETAIL_CHARS);
        assert_eq!(details.links[0], "https://medium.com/@jerry/a-post");
        assert_eq!(details.links[1].chars().count(), MAX_LINK_CHARS);
        assert_eq!(plan, None, "not a kind a plan may be");

        let mailed = parse_reading(
            r#"{"sensitive": "none", "description": "A conference badge",
                "details": {"names": ["Jerry", "jerry@example.com", "+254 712345678"]}}"#,
        );
        let Some(Reading::Kept { details, .. }) = mailed else {
            panic!("the badge is kept");
        };
        assert_eq!(details.names, ["Jerry"]);
    }

    #[test]
    fn the_critic_is_shown_the_reading_beside_the_picture() {
        assert_eq!(
            build_read_critic_message(&flight()),
            "The reading to check:\n\
             Sensitive: none\n\
             Shows: A Jambojet flight from Nairobi to Kisumu on 12 December\n\
             They want: to book the flight\n\
             Names: Jambojet. Dates: 12 December. Places: Nairobi, Kisumu. Prices: KSh 8,400. \
             Links: https://www.jambojet.com/book/123456789012\n\
             Plan: trip: Kisumu in December; when: 12 December; where: Kisumu; \
             details: Jambojet, KSh 8,400\n\n\
             The picture is attached.\n\nJSON:"
        );
        let money = parse_reading(r#"{"sensitive": "money"}"#).unwrap();
        assert_eq!(
            build_read_critic_message(&money),
            "The reading to check:\nSensitive: money\n\nThe picture is attached.\n\nJSON:"
        );
        assert!(build_read_critic_message(&Reading::Private).contains("Sensitive: private"));
    }

    #[test]
    fn the_critics_ruling_reads_however_a_small_model_writes_it() {
        assert_eq!(
            parse_read_verdict(r#"{"verdict": "agree", "why": ""}"#),
            ReadVerdict::Agree
        );
        assert_eq!(
            parse_read_verdict("```json\n{\"verdict\": \"Agree.\"}\n```"),
            ReadVerdict::Agree
        );
        assert_eq!(
            parse_read_verdict(
                "<think>the price differs</think>{\"verdict\": \"disagree\", \"why\": \"the price is  KSh 9,400\"}"
            ),
            ReadVerdict::Disagree("the price is KSh 9,400".into())
        );
        assert_eq!(
            parse_read_verdict(r#"{"verdict": "DISAGREE"}"#),
            ReadVerdict::Disagree(String::new())
        );
        assert_eq!(
            parse_read_verdict(r#"Here: {"verdict": "private", "why": "a chat"}"#),
            ReadVerdict::Private
        );
        for raw in [
            "",
            "It looks right to me.",
            r#"{"verdict": "maybe"}"#,
            r#"{"why": "fine"}"#,
            r#"{"verdict": "agree""#,
            READ_CRITIC_SYSTEM,
        ] {
            assert_eq!(parse_read_verdict(raw), ReadVerdict::Unreadable, "{raw}");
        }
        let long = format!(r#"{{"verdict": "disagree", "why": "{}"}}"#, "w".repeat(900));
        let ReadVerdict::Disagree(why) = parse_read_verdict(&long) else {
            panic!("a disagreement");
        };
        assert_eq!(why.chars().count(), MAX_REASON_CHARS);
    }

    #[test]
    fn a_reading_the_critic_agrees_with_is_kept() {
        assert_eq!(
            settle_reading(Some(flight()), ReadVerdict::Agree),
            ReadOutcome::Keep(flight())
        );
    }

    #[test]
    fn a_picture_either_calls_private_is_forgotten() {
        for critic in [
            ReadVerdict::Agree,
            ReadVerdict::Disagree("x".into()),
            ReadVerdict::Private,
            ReadVerdict::Unreadable,
        ] {
            assert_eq!(
                settle_reading(Some(Reading::Private), critic),
                ReadOutcome::Forget
            );
        }
        assert_eq!(
            settle_reading(Some(flight()), ReadVerdict::Private),
            ReadOutcome::Forget
        );
        assert_eq!(
            settle_reading(None, ReadVerdict::Private),
            ReadOutcome::Forget
        );
    }

    #[test]
    fn a_disagreement_an_unreadable_ruling_or_no_reading_is_tried_again() {
        assert_eq!(
            settle_reading(Some(flight()), ReadVerdict::Disagree("wrong date".into())),
            ReadOutcome::Retry
        );
        assert_eq!(
            settle_reading(Some(flight()), ReadVerdict::Unreadable),
            ReadOutcome::Retry
        );
        for critic in [
            ReadVerdict::Agree,
            ReadVerdict::Disagree(String::new()),
            ReadVerdict::Unreadable,
        ] {
            assert_eq!(settle_reading(None, critic), ReadOutcome::Retry);
        }
    }

    #[test]
    fn the_prompts_say_what_is_private_what_is_money_and_the_caps() {
        for private in [
            "a login or password screen",
            "a private conversation",
            "an identity or medical document",
        ] {
            assert!(READ_SYSTEM.contains(private), "{private}");
        }
        for money in [
            "a receipt",
            "a payment or transfer message",
            "a bank or mobile-money screen",
        ] {
            assert!(READ_SYSTEM.contains(money), "{money}");
            assert!(READ_CRITIC_SYSTEM.contains(money), "{money}");
        }
        assert!(READ_SYSTEM.contains("leave every other field empty"));
        assert!(READ_SYSTEM.contains("what the member most likely wants from it"));
        assert!(READ_SYSTEM.contains(&format!("at most {MAX_DETAIL_ITEMS} of each")));
        for field in ["names", "dates", "places", "prices", "links"] {
            assert!(READ_SYSTEM.contains(&format!("\"{field}\": []")), "{field}");
        }
    }
}
