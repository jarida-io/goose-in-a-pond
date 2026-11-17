//! The Budget specialist's reading of one money picture: the payments it shows as ledger
//! entries, the critic that checks them against the picture, and when Budget is offered.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use chrono_tz::Tz;
use serde_json::{Map, Value};

use super::extraction_gate::{numbered_drops, section, years_in, REPLY_CUE};
use crate::shared::domain::when_said::{day_from_words, earlier_from_words};
use crate::user_data::domain::ledger::{category_of, LedgerEntry, UNCATEGORISED};
use crate::user_data::domain::specialist::{AgentSwitch, MONEY_CAPTURES_BEFORE_OFFER};
use crate::user_data::services::model_json::{list_in, object_in, text_of};

/// The categories an entry may carry, as stored.
pub const CATEGORIES: [&str; 11] = [
    "groceries",
    "eating_out",
    "transport",
    "bills",
    "rent",
    "health",
    "shopping",
    "travel",
    "entertainment",
    "transfers",
    UNCATEGORISED,
];

/// Entries kept from one picture.
pub const MAX_ENTRIES: usize = 20;
const MAX_PAYEE_CHARS: usize = 80;
/// A word of a payee with this many digits is a phone, till or account number.
const NUMBER_DIGITS: usize = 5;

/// Fixed so it stays in the KV prefix. It shows no worked entry: a small model copies one.
pub const LEDGER_SYSTEM: &str = "\
You are shown one picture from the phone of a member of this household that \
shows money: a receipt, a payment or transfer message, or a bank or \
mobile-money screen. List the payments it shows the member made, for their \
own ledger.

Reply with JSON and nothing else:

{\"entries\": [{\"date\": \"<YYYY-MM-DD>\", \"payee\": \"<who was paid>\", \"amount\": \"<the amount as written>\", \"currency\": \"<three-letter code>\", \"category\": \"<one word from the list>\"}]}

- One entry for each payment. A receipt with a total is one entry for the \
total, never one for each item.
- A refund is an entry with a minus before the amount. Leave out other money \
they received.
- \"date\": the day it was paid; \"\" when the picture shows none.
- \"payee\": the shop, company or person paid, by name only.
- \"amount\": the amount paid, as the picture writes it.
- \"currency\": its three-letter code; \"\" when the picture does not say.
- \"category\": one of groceries, eating_out, transport, bills, rent, \
health, shopping, travel, entertainment, transfers, other.
- Never an account number, a phone number, a card number, a balance or a \
PIN, in any field.
- At most 20 entries. An empty list when the picture shows no payment.
- No explanation, no preamble.";

/// What goes beside the picture in the maker's call.
pub const LEDGER_MESSAGE: &str = "The picture is attached.\n\nJSON:";

/// Decimal places of a currency's minor unit.
pub fn minor_digits(currency: &str) -> u32 {
    const NONE: [&str; 16] = [
        "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "VND", "VUV",
        "XAF", "XOF", "XPF",
    ];
    const THREE: [&str; 7] = ["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];
    let code = currency.trim().to_uppercase();
    if NONE.contains(&code.as_str()) {
        0
    } else if THREE.contains(&code.as_str()) {
        3
    } else {
        2
    }
}

/// The currency a model named: a three-letter code, or a spelling this region writes.
fn currency_of(raw: &str) -> Option<String> {
    for (sign, code) in [("\u{20ac}", "EUR"), ("\u{a3}", "GBP")] {
        if raw.contains(sign) {
            return Some(code.to_string());
        }
    }
    let letters: String = raw
        .chars()
        .filter(|c| c.is_alphabetic())
        .collect::<String>()
        .to_uppercase();
    match letters.as_str() {
        "KSH" | "KSHS" => Some("KES".to_string()),
        "USH" | "USHS" => Some("UGX".to_string()),
        "TSH" | "TSHS" => Some("TZS".to_string()),
        code if code.len() == 3 && code.is_ascii() => Some(code.to_string()),
        _ => None,
    }
}

/// An amount as a model writes it, in minor units: "KSh 4,200.50", "4200", "1,250/=".
/// `plain_number` is a JSON number, whose point is always the decimal point.
fn amount_minor(written: &str, digits: u32, plain_number: bool) -> Option<i64> {
    let start = written.find(|c: char| c.is_ascii_digit())?;
    let negative = written[..start].contains(['-', '(', '\u{2212}']);
    // The first number only: "4,200.50 (VAT 579.38)" is one amount, not two run together.
    let body: String = written[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | ','))
        .collect();
    let body = body.trim_end_matches(['.', ',']);
    // With two kinds of mark the last is the decimal point. One mark alone is one too, unless
    // three digits follow it: those are thousands, except in a currency with three places.
    let point = body.rfind(['.', ',']).filter(|at| {
        let mark = body.as_bytes()[*at];
        let other = if mark == b'.' { ',' } else { '.' };
        let after = body.len() - at - 1;
        let alone = body.bytes().filter(|byte| *byte == mark).count() == 1;
        plain_number
            || body.contains(other)
            || (alone && (after != 3 || (digits == 3 && mark == b'.')))
    });
    let (whole, fraction) = match point {
        Some(at) => (&body[..at], &body[at + 1..]),
        None => (body, ""),
    };
    let whole: String = whole.chars().filter(char::is_ascii_digit).collect();
    let mut minor = whole
        .parse::<i64>()
        .ok()?
        .checked_mul(10_i64.checked_pow(digits)?)?;
    let mut places = fraction.chars().filter_map(|c| c.to_digit(10));
    for place in (0..digits).rev() {
        let digit = i64::from(places.next().unwrap_or(0));
        minor = minor.checked_add(digit * 10_i64.pow(place))?;
    }
    if places.next().is_some_and(|next| next >= 5) {
        minor = minor.checked_add(1)?;
    }
    Some(if negative { -minor } else { minor })
}

fn iso_day(words: &str) -> Option<NaiveDate> {
    words
        .char_indices()
        .filter(|(_, c)| c.is_ascii_digit())
        .find_map(|(at, _)| {
            let slice = words.get(at..at + 10)?;
            NaiveDate::parse_from_str(slice, "%Y-%m-%d")
                .or_else(|_| NaiveDate::parse_from_str(slice, "%Y/%m/%d"))
                .ok()
        })
}

/// The day a payment was made. A day the words do not place, or one still to come, is `today`.
fn paid_on(words: &str, today: NaiveDate, now: DateTime<Utc>, tz: Tz) -> NaiveDate {
    let latest = now.with_timezone(&tz).date_naive().max(today);
    let said = iso_day(words).or_else(|| {
        let day = day_from_words(words, today)?;
        match years_in(words).next() {
            Some(year) => day.with_year(year),
            // Said with no year, a day is read as the next one to come; a payment's is the
            // one before.
            None if day.year() > today.year() => day.with_year(day.year() - 1),
            None => Some(day),
        }
    });
    said.filter(|day| *day <= latest)
        .or_else(|| {
            earlier_from_words(words, now, tz)
                .map(|at| at.with_timezone(&tz).date_naive())
                .filter(|day| *day <= latest)
        })
        .unwrap_or(today)
}

/// A payee by name only: a word that is mostly a number is a phone, till or account number.
fn payee_of(raw: &str) -> String {
    raw.split_whitespace()
        .filter(|word| word.chars().filter(char::is_ascii_digit).count() < NUMBER_DIGITS)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_PAYEE_CHARS)
        .collect()
}

fn category_in(item: &Map<String, Value>) -> String {
    let named = text_of(item, "category")
        .map(|raw| category_of(&raw.replace('-', " ")))
        .unwrap_or_default();
    if CATEGORIES.contains(&named.as_str()) {
        named
    } else {
        UNCATEGORISED.to_string()
    }
}

fn entry_of(
    item: &Map<String, Value>,
    profile_id: &str,
    default_currency: &str,
    today: NaiveDate,
    tz: Tz,
    now: DateTime<Utc>,
) -> Option<LedgerEntry> {
    let payee = payee_of(&text_of(item, "payee")?);
    let (written, plain_number) = match item.get("amount")? {
        Value::String(written) => (written.clone(), false),
        Value::Number(number) => (number.to_string(), true),
        _ => return None,
    };
    let currency = text_of(item, "currency")
        .and_then(|named| currency_of(&named))
        .or_else(|| currency_of(&written))
        .unwrap_or_else(|| default_currency.trim().to_uppercase());
    let amount = amount_minor(&written, minor_digits(&currency), plain_number)?;
    if amount == 0 {
        return None;
    }
    let day = paid_on(&text_of(item, "date").unwrap_or_default(), today, now, tz);
    LedgerEntry::new(
        profile_id,
        day,
        &payee,
        amount,
        &currency,
        &category_in(item),
        now,
    )
}

/// The entries a maker read from one picture, at most [`MAX_ENTRIES`]. `today` is the day the
/// picture was taken: an entry with no readable date falls on it.
pub fn parse_entries(
    raw: &str,
    profile_id: &str,
    default_currency: &str,
    today: NaiveDate,
    tz: Tz,
    now: DateTime<Utc>,
) -> Vec<LedgerEntry> {
    let items: Vec<Value> = match object_in(raw) {
        Some(map) => match map.get("entries") {
            Some(Value::Array(items)) => items.clone(),
            None if map.contains_key("payee") => vec![Value::Object(map)],
            _ => Vec::new(),
        },
        None => list_in(raw, "entries").unwrap_or_default(),
    };
    items
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|item| entry_of(item, profile_id, default_currency, today, tz, now))
        .take(MAX_ENTRIES)
        .collect()
}

/// Fixed so it stays in the KV prefix.
pub const LEDGER_CRITIC_SYSTEM: &str = "\
You are shown one picture from the phone of a member of this household, and \
the numbered ledger entries another reader took from it. Check each entry \
against the picture before it is saved. You are the last check, so look for \
what is wrong.

Drop an entry when any of these is true:
- the picture does not show that payment;
- its payee, amount, currency or date is not what the picture says;
- it repeats another entry, or it is the total of entries already listed;
- it is a balance, or money the member received that is not a refund;
- it carries an account number, a phone number, a card number or a PIN.

Reply with JSON and nothing else:

{\"drop\": [<the numbers of the entries to drop>]}

- Use only the numbers you were given. An empty list when every entry is right.
- No explanation, no preamble.";

const ENTRIES_HEADING: &str = "Entries to check:";

/// An amount as a critic reads it: "4200.50 KES".
fn amount_text(minor: i64, currency: &str) -> String {
    let digits = minor_digits(currency);
    let unit = 10_u64.pow(digits);
    let sign = if minor < 0 { "-" } else { "" };
    let (whole, part) = (minor.unsigned_abs() / unit, minor.unsigned_abs() % unit);
    if digits == 0 {
        format!("{sign}{whole} {currency}")
    } else {
        format!(
            "{sign}{whole}.{part:0width$} {currency}",
            width = digits as usize
        )
    }
}

/// The critic's user message, sent with the same picture: the entries numbered from 1.
pub fn build_ledger_critic_message(entries: &[LedgerEntry]) -> String {
    let numbered: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(at, entry)| {
            format!(
                "{}. {} | {} | {} | {}",
                at + 1,
                entry.occurred_on.format("%Y-%m-%d"),
                entry.payee,
                amount_text(entry.amount_minor, &entry.currency),
                entry.category
            )
        })
        .collect();
    let mut out = String::new();
    section(&mut out, ENTRIES_HEADING, &numbered.join("\n"));
    out.push_str("The picture is attached.\n\n");
    out.push_str(REPLY_CUE);
    out
}

/// Which of `count` entries the critic dropped, as places from 0; `None` when the reply cannot
/// be read, and then nothing is saved.
pub fn parse_ledger_drops(raw: &str, count: usize) -> Option<Vec<usize>> {
    numbered_drops(raw, count).map(|drops| drops.into_iter().map(|(place, _)| place).collect())
}

/// Whether to offer the Budget specialist: the member holds enough money captures and has
/// neither turned it on nor answered an offer.
pub fn should_offer_budget(held_money_captures: usize, switch: AgentSwitch) -> bool {
    switch == AgentSwitch::Off && held_money_captures >= MONEY_CAPTURES_BEFORE_OFFER as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    /// Monday 16 November 2026, 12:00 in Nairobi.
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn today() -> NaiveDate {
        day(2026, 11, 16)
    }

    fn read(raw: &str) -> Vec<LedgerEntry> {
        parse_entries(raw, "jerry", "kes", today(), NAIROBI, now())
    }

    fn one(fields: &str) -> Option<LedgerEntry> {
        let mut entries = read(&format!(r#"{{"entries": [{{{fields}}}]}}"#));
        assert!(entries.len() <= 1);
        entries.pop()
    }

    const RECEIPT: &str = r#"{"entries": [
        {"date": "2026-11-03", "payee": "Naivas  Westlands", "amount": "KSh 4,200.50",
         "currency": "KES", "category": "groceries"},
        {"date": "", "payee": "Java House", "amount": 1250, "currency": "", "category": "Eating out"}]}"#;

    #[test]
    fn the_payments_a_picture_shows_read_as_ledger_entries() {
        let entries = read(RECEIPT);
        assert_eq!(entries.len(), 2);
        let naivas = &entries[0];
        assert_eq!(naivas.profile_id, "jerry");
        assert_eq!(naivas.occurred_on, day(2026, 11, 3));
        assert_eq!(naivas.payee, "Naivas Westlands");
        assert_eq!(naivas.amount_minor, 420_050);
        assert_eq!(naivas.currency, "KES");
        assert_eq!(naivas.category, "groceries");
        assert_eq!(naivas.created_at, now());
        let java = &entries[1];
        assert_eq!(java.occurred_on, today(), "no date falls on the picture's");
        assert_eq!(java.amount_minor, 125_000);
        assert_eq!(java.currency, "KES", "the member's own currency");
        assert_eq!(java.category, "eating_out");
    }

    #[test]
    fn an_amount_reads_in_minor_units_however_it_is_written() {
        for (written, minor) in [
            ("KSh 4,200.50", 420_050),
            ("4200", 420_000),
            ("1,250/=", 125_000),
            ("Ksh. 1,250.00", 125_000),
            ("KES4200.5", 420_050),
            ("4.200,50", 420_050),
            ("1,250,000", 125_000_000),
            ("0.50", 50),
            ("12", 1_200),
            ("1,250/-", 125_000),
            ("-1,250", -125_000),
            ("KSh -500", -50_000),
            ("(1,250.00)", -125_000),
            ("4,200.505", 420_051),
            ("4,200.50 (VAT 579.38)", 420_050),
            ("4,200.50.", 420_050),
        ] {
            assert_eq!(amount_minor(written, 2, false), Some(minor), "{written}");
        }
        assert_eq!(amount_minor("USh 50,000", 0, false), Some(50_000));
        assert_eq!(amount_minor("50,000.00", 0, false), Some(50_000));
        assert_eq!(amount_minor("1.250", 3, false), Some(1_250));
        assert_eq!(amount_minor("1.250", 2, false), Some(125_000));
        assert_eq!(amount_minor("1250.125", 2, true), Some(125_013));
        assert_eq!(amount_minor("4200.5", 2, true), Some(420_050));
        for unreadable in ["", "free", "KSh", "99999999999999999999"] {
            assert_eq!(amount_minor(unreadable, 2, false), None, "{unreadable}");
        }
    }

    #[test]
    fn an_entry_takes_its_currency_from_the_field_then_the_amount_then_the_member() {
        let currency = |fields: &str| {
            one(&format!(r#""payee": "Shop", {fields}"#))
                .unwrap()
                .currency
        };
        assert_eq!(currency(r#""amount": "12.99", "currency": "usd""#), "USD");
        assert_eq!(currency(r#""amount": "USD 12.99", "currency": """#), "USD");
        assert_eq!(
            currency(r#""amount": "KSh 4,200", "currency": "KSh""#),
            "KES"
        );
        assert_eq!(currency(r#""amount": "KSh 4,200""#), "KES");
        let euros = format!(r#""amount": "{}12,50""#, '\u{20ac}');
        assert_eq!(currency(&euros), "EUR");
        assert_eq!(
            currency(r#""amount": "4200", "currency": "shillings""#),
            "KES"
        );
        assert_eq!(currency(r#""amount": "4200""#), "KES");

        let ugandan = one(r#""payee": "Cafe Javas", "amount": "USh 50,000""#).unwrap();
        assert_eq!(
            (ugandan.currency.as_str(), ugandan.amount_minor),
            ("UGX", 50_000)
        );
        assert_eq!(minor_digits("ugx"), 0);
        assert_eq!(minor_digits("KWD"), 3);
        assert_eq!(minor_digits("KES"), 2);
    }

    #[test]
    fn a_payment_falls_on_the_day_the_picture_says_and_never_on_one_still_to_come() {
        let on = |date: &str| {
            one(&format!(
                r#""payee": "Shop", "amount": "100", "date": "{date}""#
            ))
            .unwrap()
            .occurred_on
        };
        assert_eq!(on("2026-11-03"), day(2026, 11, 3));
        assert_eq!(on("2026-11-03 14:22"), day(2026, 11, 3));
        assert_eq!(on("Paid 2026/11/03"), day(2026, 11, 3));
        assert_eq!(on("2025-12-31"), day(2025, 12, 31));
        assert_eq!(on("3 November"), day(2026, 11, 3));
        assert_eq!(on("3 Nov 2025 14:22"), day(2025, 11, 3));
        assert_eq!(on("16 November"), today());
        assert_eq!(on("today"), today());
        assert_eq!(on("yesterday"), day(2026, 11, 15));
        assert_eq!(on("Friday"), day(2026, 11, 13));
        for unplaced in ["", "03/11/2026", "last quarter", "2026-12-25", "2026-13-45"] {
            assert_eq!(on(unplaced), today(), "{unplaced}");
        }
    }

    #[test]
    fn an_older_picture_dates_its_undated_entries_by_the_day_it_was_taken() {
        let taken = day(2026, 11, 10);
        let entries = parse_entries(
            r#"{"entries": [{"payee": "Shop", "amount": "100"},
                            {"payee": "Kiosk", "amount": "50", "date": "2026-11-14"}]}"#,
            "jerry",
            "KES",
            taken,
            NAIROBI,
            now(),
        );
        assert_eq!(entries[0].occurred_on, taken);
        assert_eq!(entries[1].occurred_on, day(2026, 11, 14));
    }

    #[test]
    fn a_category_outside_the_list_becomes_other() {
        let category = |named: &str| {
            one(&format!(
                r#""payee": "Shop", "amount": "100", "category": "{named}""#
            ))
            .unwrap()
            .category
        };
        for listed in CATEGORIES {
            assert_eq!(category(listed), listed);
        }
        assert_eq!(category("Eating-Out"), "eating_out");
        assert_eq!(category(" TRANSPORT "), "transport");
        for unknown in ["fuel", "food and drink", "", "<one word from the list>"] {
            assert_eq!(category(unknown), "other", "{unknown}");
        }
        assert_eq!(
            one(r#""payee": "Shop", "amount": "100""#).unwrap().category,
            "other"
        );
    }

    #[test]
    fn an_entry_needs_a_payee_by_name_and_an_amount() {
        for fields in [
            r#""amount": "100""#,
            r#""payee": " ", "amount": "100""#,
            r#""payee": "0712345678", "amount": "100""#,
            r#""payee": "Shop""#,
            r#""payee": "Shop", "amount": "free""#,
            r#""payee": "Shop", "amount": "0.00""#,
            r#""payee": "Shop", "amount": null"#,
            r#""payee": "<who was paid>", "amount": "100""#,
        ] {
            assert_eq!(one(fields), None, "{fields}");
        }
        let named = one(r#""payee": "JOHN DOE 0712345678 till 123456", "amount": "100""#).unwrap();
        assert_eq!(named.payee, "JOHN DOE till");
        let long = one(&format!(
            r#""payee": "{}", "amount": "100""#,
            "shop ".repeat(60)
        ))
        .unwrap();
        assert!(long.payee.chars().count() <= MAX_PAYEE_CHARS);
    }

    #[test]
    fn the_shapes_a_small_model_writes_all_read() {
        let entry = r#"{"date": "2026-11-03", "payee": "Naivas", "amount": "4,200.50"}"#;
        for raw in [
            format!(r#"{{"entries": [{entry}]}}"#),
            format!("```json\n{{\"entries\": [{entry}]}}\n```"),
            format!("Here are the payments:\n[{entry}]\nThat is all."),
            format!("<think>one payment</think>[{entry}]"),
            entry.to_string(),
        ] {
            let entries = read(&raw);
            assert_eq!(entries.len(), 1, "{raw}");
            assert_eq!(entries[0].amount_minor, 420_050, "{raw}");
        }
        for nothing in [
            "",
            "The picture shows no payment.",
            r#"{"entries": []}"#,
            r#"{"entries": "none"}"#,
            r#"{"payments": [{"payee": "Shop", "amount": "100"}]}"#,
            LEDGER_SYSTEM,
        ] {
            assert!(read(nothing).is_empty(), "{nothing}");
        }
    }

    #[test]
    fn a_picture_yields_twenty_entries_at_most() {
        let many: Vec<String> = (1..=30)
            .map(|n| format!(r#"{{"payee": "Shop {n}", "amount": "{n}"}}"#))
            .collect();
        let entries = read(&format!(r#"{{"entries": [{}]}}"#, many.join(",")));
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(entries[19].payee, "Shop 20");
    }

    #[test]
    fn the_critic_is_shown_the_entries_by_number_beside_the_picture() {
        let mut entries = read(RECEIPT);
        entries.extend(one(
            r#""payee": "Refund", "amount": "-50", "currency": "UGX", "date": "2026-11-01""#,
        ));
        assert_eq!(
            build_ledger_critic_message(&entries),
            "Entries to check:\n\
             1. 2026-11-03 | Naivas Westlands | 4200.50 KES | groceries\n\
             2. 2026-11-16 | Java House | 1250.00 KES | eating_out\n\
             3. 2026-11-01 | Refund | -50 UGX | other\n\n\
             The picture is attached.\n\nJSON:"
        );
        assert_eq!(amount_text(5, "KES"), "0.05 KES");
        assert_eq!(amount_text(-1_250, "KWD"), "-1.250 KWD");
    }

    #[test]
    fn the_critics_drops_read_as_places_and_an_unreadable_reply_saves_nothing() {
        assert_eq!(parse_ledger_drops(r#"{"drop": [2]}"#, 3), Some(vec![1]));
        assert_eq!(parse_ledger_drops(r#"{"drop": []}"#, 3), Some(vec![]));
        assert_eq!(
            parse_ledger_drops("```json\n{\"drop\": [3, 1, 3, \"2\"]}\n```", 3),
            Some(vec![0, 1, 2])
        );
        for raw in [
            "",
            "They all match the picture.",
            r#"{"keep": [1, 2, 3]}"#,
            r#"{"drop": [7]}"#,
            r#"{"drop": [1"#,
            LEDGER_CRITIC_SYSTEM,
        ] {
            assert_eq!(parse_ledger_drops(raw, 3), None, "{raw}");
        }
    }

    #[test]
    fn budget_is_offered_at_three_held_money_pictures_while_its_switch_is_off() {
        assert!(!should_offer_budget(0, AgentSwitch::Off));
        assert!(!should_offer_budget(2, AgentSwitch::Off));
        assert!(should_offer_budget(3, AgentSwitch::Off));
        assert!(should_offer_budget(4, AgentSwitch::Off));
        for answered in [AgentSwitch::On, AgentSwitch::Offered, AgentSwitch::Declined] {
            assert!(!should_offer_budget(3, answered), "{answered:?}");
        }
    }

    #[test]
    fn the_prompt_names_every_category_the_cap_and_what_is_never_listed() {
        for category in CATEGORIES {
            assert!(LEDGER_SYSTEM.contains(category), "{category}");
        }
        assert!(LEDGER_SYSTEM.contains(&format!("At most {MAX_ENTRIES} entries")));
        for never in ["an account number", "a balance", "PIN"] {
            assert!(LEDGER_SYSTEM.contains(never), "{never}");
        }
        assert!(LEDGER_SYSTEM.contains("An empty list when the picture shows no payment"));
        assert_eq!(CATEGORIES.len(), 11);
        assert_eq!(CATEGORIES[10], "other");
    }
}
