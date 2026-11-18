//! A member's specialists and the Budget specialist's ledger: which ones are on, and the
//! payments Budget read from their money pictures, which the member reads, corrects and
//! deletes. Reading a picture into the ledger is the engine's.

use std::sync::Arc;

use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Path, Query, State,
    },
    http::StatusCode,
    routing::{get, patch, put},
    Extension, Json, Router,
};
use chrono::{Duration, NaiveDate, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::ledger::{category_of, month_bounds, month_totals, LedgerEntry};
use pond_core::user_data::domain::specialist::{specialist, AgentSwitch, SpecialistId};
use pond_core::user_data::services::ledger_reading::{minor_digits, CATEGORIES};

use crate::anchors::bad_body;
use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

pub(crate) const SERVED: bool = true;

const MAX_PAYEE_CHARS: usize = 80;
const MAX_NOTE_CHARS: usize = 300;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/agents", get(list_agents))
        .route("/me/agents/{id}", put(switch_agent))
        .route("/me/ledger", get(read_ledger))
        .route("/me/ledger/{id}", patch(change_entry).delete(remove_entry))
}

fn in_words(count: usize) -> String {
    match count {
        1 => "one",
        2 => "two",
        3 => "three",
        4 => "four",
        5 => "five",
        _ => return count.to_string(),
    }
    .to_string()
}

/// Why Goose offered a specialist, in a line its member reads.
fn why_offered(agent: SpecialistId, held_money_captures: usize) -> String {
    match (agent, held_money_captures) {
        (SpecialistId::Budget, 1) => "You have sent a picture of a receipt or a payment".into(),
        (SpecialistId::Budget, held) if held > 1 => format!(
            "You have sent {} pictures of receipts or payments",
            in_words(held)
        ),
        _ => "Goose thinks this would help with what you have been doing".into(),
    }
}

/// Where a switch goes when its member turns it on or off.
fn next_switch(held: AgentSwitch, on: bool) -> AgentSwitch {
    match (on, held) {
        (true, _) => AgentSwitch::On,
        // A no to an offer stays a no, so it is not offered again.
        (false, AgentSwitch::Offered | AgentSwitch::Declined) => AgentSwitch::Declined,
        (false, _) => AgentSwitch::Off,
    }
}

async fn agents(state: &AppState, member: &str) -> Result<Json<Value>, Failure> {
    let switches = state
        .engine
        .agents
        .switches(member)
        .await
        .map_err(server_error)?;
    let offered = |agent: SpecialistId| {
        switches
            .iter()
            .any(|switch| switch.agent == agent && switch.state == AgentSwitch::Offered)
    };
    let held = if offered(SpecialistId::Budget) {
        let hot = state.engine.captures.hot(member).await;
        hot.map_err(server_error)?.len()
    } else {
        0
    };
    let shown: Vec<Value> = switches
        .iter()
        .map(|switch| {
            let shipped = specialist(switch.agent);
            let why = match switch.state {
                AgentSwitch::Offered => why_offered(switch.agent, held),
                _ => String::new(),
            };
            json!({
                "id": switch.agent,
                "title": shipped.title,
                "about": shipped.about,
                "state": switch.state,
                "why": why,
            })
        })
        .collect();
    Ok(Json(json!({ "agents": shown })))
}

async fn list_agents(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    agents(&state, &member).await
}

#[derive(Debug, Deserialize)]
struct Switch {
    on: bool,
}

async fn switch_agent(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<Switch>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    let agent =
        SpecialistId::parse(&id).ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such agent"))?;
    let switches = &state.engine.agents;
    let held = switches
        .switch(&member, agent)
        .await
        .map_err(server_error)?;
    let next = next_switch(held, body.on);
    // An off that changes nothing stores nothing, so the member stays on the shipped default.
    if next != held {
        switches
            .set(&member, agent, next, Utc::now())
            .await
            .map_err(server_error)?;
    }
    if agent == SpecialistId::Budget && !body.on {
        let hot = state.engine.captures.hot(&member).await;
        crate::captures::forget(&state, &member, &hot.map_err(server_error)?).await?;
    }
    agents(&state, &member).await
}

/// The ledger is the Budget agent's: with it off there is none to read or change.
async fn budget_is_on(state: &AppState, member: &str) -> Result<(), Failure> {
    let switch = state
        .engine
        .agents
        .switch(member, SpecialistId::Budget)
        .await
        .map_err(server_error)?;
    if switch.is_on() {
        return Ok(());
    }
    Err(failure(
        StatusCode::CONFLICT,
        "the Budget agent is off: turn it on in My Goose to keep a ledger",
    ))
}

fn no_such_entry() -> Failure {
    failure(StatusCode::NOT_FOUND, "no such ledger entry of yours")
}

/// An amount in minor units as a decimal in the currency's major unit: `420050` is "4200.50".
fn in_major(minor: i64, currency: &str) -> String {
    let places = minor_digits(currency) as usize;
    let sign = if minor < 0 { "-" } else { "" };
    let unit = 10_u64.pow(places as u32);
    let (whole, part) = (minor.unsigned_abs() / unit, minor.unsigned_abs() % unit);
    if places == 0 {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{part:0places$}")
    }
}

/// A plain decimal in the currency's major unit, as minor units; `None` for anything but
/// digits with one optional point, or more places than the currency has.
fn in_minor(written: &str, currency: &str) -> Option<i64> {
    let places = minor_digits(currency) as usize;
    let written = written.trim();
    let (negative, body) = match written.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, written),
    };
    let (whole, part) = body.split_once('.').unwrap_or((body, ""));
    let digits = |text: &str| text.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(part) || part.len() > places {
        return None;
    }
    let unit = 10_i64.checked_pow(places as u32)?;
    let mut minor = whole.parse::<i64>().ok()?.checked_mul(unit)?;
    if !part.is_empty() {
        minor = minor.checked_add(format!("{part:0<places$}").parse().ok()?)?;
    }
    Some(if negative { -minor } else { minor })
}

fn entry_view(entry: &LedgerEntry) -> Value {
    json!({
        "id": entry.id,
        "occurred_on": entry.occurred_on,
        "payee": entry.payee,
        "amount": in_major(entry.amount_minor, &entry.currency),
        "currency": entry.currency,
        "category": entry.category,
        "anchor_id": entry.anchor_id,
        "note": entry.note,
    })
}

fn totals_view(entries: &[LedgerEntry]) -> Vec<Value> {
    month_totals(entries)
        .iter()
        .map(|total| {
            let by_category: Vec<Value> = total
                .categories
                .iter()
                .map(|category| {
                    json!({
                        "category": category.category,
                        "total": in_major(category.amount_minor, &total.currency),
                    })
                })
                .collect();
            json!({
                "currency": total.currency,
                "total": in_major(total.amount_minor, &total.currency),
                "by_category": by_category,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct MonthQuery {
    #[serde(default)]
    month: Option<String>,
}

async fn read_ledger(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    query: Result<Query<MonthQuery>, QueryRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Query(query) = query
        .map_err(|rejection| failure(StatusCode::UNPROCESSABLE_ENTITY, rejection.body_text()))?;
    budget_is_on(&state, &member).await?;
    let month = match query.month {
        Some(month) => month.trim().to_string(),
        None => {
            let zone = crate::todos::timezone(&state).await;
            Utc::now().with_timezone(&zone).format("%Y-%m").to_string()
        }
    };
    let (from, until) = month_bounds(&month).ok_or_else(|| {
        failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "month must be a year and a month, such as 2026-11",
        )
    })?;
    let entries = state
        .engine
        .ledger
        .list(&member, from, until)
        .await
        .map_err(server_error)?;
    Ok(Json(json!({
        "month": from.format("%Y-%m").to_string(),
        "entries": entries.iter().map(entry_view).collect::<Vec<_>>(),
        "totals": totals_view(&entries),
    })))
}

/// A field left out of the body stays as it is; one sent as `null` is cleared.
fn sent<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
struct EntryPatch {
    #[serde(default)]
    payee: Option<String>,
    /// A decimal in the entry's currency, as text or as a number: `"4200.50"` or `4200.5`.
    #[serde(default)]
    amount: Option<Value>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    occurred_on: Option<NaiveDate>,
    #[serde(default, deserialize_with = "sent")]
    note: Option<Option<String>>,
    #[serde(default, deserialize_with = "sent")]
    anchor_id: Option<Option<String>>,
}

fn one_line(text: &str, max_chars: usize) -> Option<String> {
    let line: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect();
    (!line.is_empty()).then_some(line)
}

fn unusable(why: impl Into<String>) -> Failure {
    failure(StatusCode::UNPROCESSABLE_ENTITY, why)
}

async fn change_entry(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<EntryPatch>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(patch) = body.map_err(bad_body)?;
    budget_is_on(&state, &member).await?;
    let ledger = &state.engine.ledger;
    let mut entry = ledger
        .get(&member, &id)
        .await
        .map_err(server_error)?
        .ok_or_else(no_such_entry)?;

    if let Some(payee) = patch.payee.as_deref() {
        entry.payee = one_line(payee, MAX_PAYEE_CHARS)
            .ok_or_else(|| unusable("an entry needs a payee: say who was paid"))?;
    }
    if let Some(amount) = patch.amount {
        let written = match amount {
            Value::String(text) => text,
            Value::Number(number) => number.to_string(),
            _ => String::new(),
        };
        entry.amount_minor = in_minor(&written, &entry.currency)
            .filter(|minor| *minor != 0)
            .ok_or_else(|| {
                unusable(format!(
                    "write the amount as a plain number in {}, such as {}; to drop the entry, \
                     delete it",
                    entry.currency,
                    in_major(420_050, &entry.currency)
                ))
            })?;
    }
    if let Some(category) = patch.category.as_deref() {
        let category = category_of(category);
        if !CATEGORIES.contains(&category.as_str()) {
            return Err(unusable(format!(
                "choose a category from: {}",
                CATEGORIES.join(", ")
            )));
        }
        entry.category = category;
    }
    if let Some(day) = patch.occurred_on {
        let zone = crate::todos::timezone(&state).await;
        // A day of grace for a member who is a time zone ahead of the pond.
        let latest = Utc::now().with_timezone(&zone).date_naive() + Duration::days(1);
        if day > latest {
            return Err(unusable(
                "a payment cannot be dated after today: check the date",
            ));
        }
        entry.occurred_on = day;
    }
    if let Some(note) = patch.note {
        entry.note = note.as_deref().and_then(|n| one_line(n, MAX_NOTE_CHARS));
    }
    if let Some(anchor_id) = patch.anchor_id {
        if let Some(anchor_id) = anchor_id.as_deref() {
            let anchors = &state.engine.anchors;
            if anchors
                .get(&member, anchor_id)
                .await
                .map_err(server_error)?
                .is_none()
            {
                return Err(unusable(
                    "that is not one of your anchors: choose one you can open",
                ));
            }
        }
        entry.anchor_id = anchor_id;
    }

    if !ledger.save(&entry).await.map_err(server_error)? {
        return Err(no_such_entry());
    }
    Ok(Json(entry_view(&entry)))
}

async fn remove_entry(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    budget_is_on(&state, &member).await?;
    if !state
        .engine
        .ledger
        .delete(&member, &id)
        .await
        .map_err(server_error)?
    {
        return Err(no_such_entry());
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_amount_is_written_and_read_in_the_currencys_major_unit() {
        for (minor, currency, written) in [
            (420_050, "KES", "4200.50"),
            (-50_000, "KES", "-500.00"),
            (5, "KES", "0.05"),
            (50_000, "UGX", "50000"),
            (1_250, "KWD", "1.250"),
            (0, "EUR", "0.00"),
        ] {
            assert_eq!(in_major(minor, currency), written, "{minor} {currency}");
            assert_eq!(in_minor(written, currency), Some(minor), "{written}");
        }
        assert_eq!(in_minor(" 4200 ", "KES"), Some(420_000));
        assert_eq!(in_minor("4200.5", "KES"), Some(420_050));
        assert_eq!(in_minor("4200.", "KES"), Some(420_000));
        for unreadable in [
            "",
            "-",
            ".50",
            "4,200.50",
            "4200.505",
            "KSh 4200",
            "1e3",
            "50000.5 ",
            "99999999999999999999",
        ] {
            let currency = if unreadable.starts_with("50000") {
                "UGX"
            } else {
                "KES"
            };
            assert_eq!(in_minor(unreadable, currency), None, "{unreadable}");
        }
    }

    #[test]
    fn a_no_to_an_offer_is_kept_and_any_yes_turns_the_agent_on() {
        use AgentSwitch::{Declined, Off, Offered, On};
        for held in [On, Off, Offered, Declined] {
            assert_eq!(next_switch(held, true), On);
        }
        assert_eq!(next_switch(On, false), Off);
        assert_eq!(next_switch(Off, false), Off);
        assert_eq!(next_switch(Offered, false), Declined);
        assert_eq!(next_switch(Declined, false), Declined);
    }

    #[test]
    fn an_offer_says_why_in_a_plain_line() {
        assert_eq!(
            why_offered(SpecialistId::Budget, 3),
            "You have sent three pictures of receipts or payments"
        );
        assert_eq!(
            why_offered(SpecialistId::Budget, 1),
            "You have sent a picture of a receipt or a payment"
        );
        assert!(why_offered(SpecialistId::Budget, 12).contains("12 pictures"));
        assert!(!why_offered(SpecialistId::Trip, 3).contains("receipt"));
        assert!(!why_offered(SpecialistId::Budget, 0).contains("receipt"));
    }
}
