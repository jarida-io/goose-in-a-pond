//! The Budget specialist's records: what a member paid, read from their money captures.
//! Amounts are whole minor units, so no total drifts by a rounding.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// The category of an entry nobody categorised.
pub const UNCATEGORISED: &str = "other";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    pub occurred_on: NaiveDate,
    pub payee: String,
    /// In the currency's minor unit; a refund is negative.
    pub amount_minor: i64,
    /// An ISO 4217 code, upper case.
    pub currency: String,
    pub category: String,
    #[serde(default)]
    pub anchor_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl LedgerEntry {
    /// `None` when the payee or the currency has no words.
    pub fn new(
        profile_id: &str,
        occurred_on: NaiveDate,
        payee: &str,
        amount_minor: i64,
        currency: &str,
        category: &str,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        let payee = payee.split_whitespace().collect::<Vec<_>>().join(" ");
        let currency = currency.trim().to_uppercase();
        if payee.is_empty() || currency.is_empty() {
            return None;
        }
        Some(Self {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            occurred_on,
            payee,
            amount_minor,
            currency,
            category: category_of(category),
            anchor_id: None,
            note: None,
            created_at: now,
        })
    }
}

/// A category as stored: lower case, one word; nothing left is [`UNCATEGORISED`].
pub fn category_of(raw: &str) -> String {
    let cleaned = raw
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_");
    if cleaned.is_empty() {
        UNCATEGORISED.to_string()
    } else {
        cleaned
    }
}

/// The first day of the month `month` names (`2026-11`) and of the month after it.
pub fn month_bounds(month: &str) -> Option<(NaiveDate, NaiveDate)> {
    let (year, number) = month.trim().split_once('-')?;
    let first = NaiveDate::from_ymd_opt(year.parse().ok()?, number.parse().ok()?, 1)?;
    let next = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)?
    };
    Some((first, next))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CategoryTotal {
    pub category: String,
    pub amount_minor: i64,
}

/// One currency's total and its categories, largest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CurrencyTotal {
    pub currency: String,
    pub amount_minor: i64,
    pub categories: Vec<CategoryTotal>,
}

/// What `entries` add up to, one total per currency: amounts in different currencies are
/// never added together.
pub fn month_totals(entries: &[LedgerEntry]) -> Vec<CurrencyTotal> {
    let mut by_currency: BTreeMap<&str, BTreeMap<&str, i64>> = BTreeMap::new();
    for entry in entries {
        *by_currency
            .entry(entry.currency.as_str())
            .or_default()
            .entry(entry.category.as_str())
            .or_default() += entry.amount_minor;
    }
    by_currency
        .into_iter()
        .map(|(currency, categories)| {
            let mut categories: Vec<CategoryTotal> = categories
                .into_iter()
                .map(|(category, amount_minor)| CategoryTotal {
                    category: category.to_string(),
                    amount_minor,
                })
                .collect();
            categories.sort_by(|a, b| {
                b.amount_minor
                    .cmp(&a.amount_minor)
                    .then(a.category.cmp(&b.category))
            });
            CurrencyTotal {
                currency: currency.to_string(),
                amount_minor: categories.iter().map(|c| c.amount_minor).sum(),
                categories,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(payee: &str, amount_minor: i64, currency: &str, category: &str) -> LedgerEntry {
        LedgerEntry::new(
            "jerry",
            NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
            payee,
            amount_minor,
            currency,
            category,
            Utc::now(),
        )
        .unwrap()
    }

    #[test]
    fn a_month_is_totalled_by_category_and_never_across_currencies() {
        let totals = month_totals(&[
            entry("Naivas", 420_000, "kes", "Groceries"),
            entry("Carrefour", 180_050, "KES", "groceries"),
            entry("KPLC", 350_000, "KES", "power"),
            entry("Refund", -50_000, "KES", "power"),
            entry("Hetzner", 1_299, "EUR", ""),
        ]);
        assert_eq!(
            totals,
            vec![
                CurrencyTotal {
                    currency: "EUR".into(),
                    amount_minor: 1_299,
                    categories: vec![CategoryTotal {
                        category: "other".into(),
                        amount_minor: 1_299
                    }],
                },
                CurrencyTotal {
                    currency: "KES".into(),
                    amount_minor: 900_050,
                    categories: vec![
                        CategoryTotal {
                            category: "groceries".into(),
                            amount_minor: 600_050
                        },
                        CategoryTotal {
                            category: "power".into(),
                            amount_minor: 300_000
                        },
                    ],
                },
            ]
        );
        assert!(month_totals(&[]).is_empty());
    }

    #[test]
    fn a_month_runs_from_its_first_day_to_the_next_months() {
        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(
            month_bounds("2026-11"),
            Some((day(2026, 11, 1), day(2026, 12, 1)))
        );
        assert_eq!(
            month_bounds(" 2026-12 "),
            Some((day(2026, 12, 1), day(2027, 1, 1)))
        );
        assert_eq!(month_bounds("2026-13"), None);
        assert_eq!(month_bounds("November"), None);
    }

    #[test]
    fn an_entry_needs_a_payee_and_a_currency() {
        let on = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        assert!(LedgerEntry::new("jerry", on, "  ", 100, "KES", "food", Utc::now()).is_none());
        assert!(LedgerEntry::new("jerry", on, "Naivas", 100, " ", "food", Utc::now()).is_none());
        assert_eq!(
            entry("Naivas", 100, "kes", "Eating Out").category,
            "eating_out"
        );
    }
}
