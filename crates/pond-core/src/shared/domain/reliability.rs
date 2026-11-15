//! How dependably the pond has reached people and answered them over a period, in the terms a
//! person would use: did a notice arrive, did a message get a reply, is a phone still around.

use serde::Serialize;

use crate::mcp::domain::notice_delivery::DeliveryTotals;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TurnTotals {
    /// Messages people sent.
    pub asked: u64,
    /// Those with a reply stored after them.
    pub answered: u64,
}

impl TurnTotals {
    pub fn unanswered(&self) -> u64 {
        self.asked.saturating_sub(self.answered)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PhoneTotals {
    /// Phones with a live pairing.
    pub paired: u64,
    /// Of those, a test harness's.
    pub test: u64,
    /// Of those, not seen for longer than the household's limit, so no longer notified.
    pub unseen: u64,
    /// Of those, registered for push, so a wake can be sent at all.
    pub with_push_token: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ReliabilityReport {
    pub days: u32,
    pub notices: DeliveryTotals,
    pub turns: TurnTotals,
    pub phones: PhoneTotals,
    /// Times the pond was told to stop.
    pub stops: u64,
    /// Whether this pond has a key to wake Android phones with.
    pub android_push_key: bool,
    /// Whether this pond has a key to wake iPhones with.
    pub apple_push_key: bool,
}

impl ReliabilityReport {
    /// The report as a person reads it in a terminal, one fact a line.
    pub fn lines(&self) -> Vec<String> {
        let n = &self.notices;
        let within = match n.percent_within_a_minute() {
            Some(percent) => format!("{} ({percent}%)", n.fetched_within_a_minute),
            None => "0".to_string(),
        };
        let key = |present: bool| if present { "present" } else { "MISSING" };
        vec![
            format!("Last {} days", self.days),
            String::new(),
            "Notices".to_string(),
            format!("  queued for phones        {}", n.queued),
            format!("  push wake sent           {}", n.woken),
            format!("  fetched by the phone     {}", n.fetched),
            format!("  fetched within a minute  {within}"),
            format!("  fetched after an hour    {}", n.fetched_after_an_hour),
            format!("  never fetched            {}", n.never_fetched()),
            format!("  shown, as the phone said {}", n.shown),
            format!("  opened                   {}", n.opened),
            format!("  said aloud instead       {}", n.spoken),
            String::new(),
            "Messages".to_string(),
            format!("  sent by people           {}", self.turns.asked),
            format!("  answered                 {}", self.turns.answered),
            format!("  left without a reply     {}", self.turns.unanswered()),
            String::new(),
            "Phones".to_string(),
            format!("  paired                   {}", self.phones.paired),
            format!("  registered for push      {}", self.phones.with_push_token),
            format!("  unseen past the limit    {}", self.phones.unseen),
            format!("  test devices             {}", self.phones.test),
            String::new(),
            "Pond".to_string(),
            format!("  stops                    {}", self.stops),
            format!("  Android push key         {}", key(self.android_push_key)),
            format!("  Apple push key           {}", key(self.apple_push_key)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_says_what_share_arrived_and_which_key_is_missing() {
        let report = ReliabilityReport {
            days: 7,
            notices: DeliveryTotals {
                queued: 40,
                fetched: 30,
                fetched_within_a_minute: 10,
                ..DeliveryTotals::default()
            },
            turns: TurnTotals {
                asked: 20,
                answered: 19,
            },
            android_push_key: true,
            ..ReliabilityReport::default()
        };
        let lines = report.lines().join("\n");
        assert!(
            lines.contains("fetched within a minute  10 (25%)"),
            "{lines}"
        );
        assert!(lines.contains("never fetched            10"), "{lines}");
        assert!(lines.contains("left without a reply     1"), "{lines}");
        assert!(
            lines.contains("Android push key         present"),
            "{lines}"
        );
        assert!(
            lines.contains("Apple push key           MISSING"),
            "{lines}"
        );
    }
}
