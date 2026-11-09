//! What each engine session read from the web, in order: the address, its title and site, the
//! lines it was given and when, under a number the session can cite. A report's sources come from
//! here rather than from links a model writes, and a chat turn's reads are counted here. Held in
//! memory only.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

/// Pages a conversation turn reads at most; past it the turn answers, or offers a long task.
pub const MAX_CHAT_TURN_READS: usize = 3;

/// As long as an address stays readable, so a task's sources outlive its reads.
const SESSION_LIFETIME: Duration = Duration::from_secs(2 * 60 * 60);

const MAX_SESSIONS: usize = 64;
const MAX_READS_PER_SESSION: usize = 64;

/// The lines kept of one page: a chip shows the line, not the page.
const MAX_USED_BYTES: usize = 1_800;

/// One page a session read.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRead {
    /// Its number in the session, from 1; a page read again keeps it.
    pub number: usize,
    pub address: String,
    pub title: String,
    pub site: String,
    pub lines_used: Vec<String>,
    pub read_at: DateTime<Utc>,
}

#[derive(Debug)]
struct Ledger {
    session: String,
    reads: Vec<SourceRead>,
    /// Pages read since the turn began, for a session a conversation turn is running in.
    chat_turn_reads: Option<usize>,
    touched: Instant,
}

#[derive(Debug, Default)]
pub struct SourceLedger {
    sessions: Mutex<Vec<Ledger>>,
}

impl SourceLedger {
    pub const fn new() -> Self {
        Self {
            sessions: Mutex::new(Vec::new()),
        }
    }

    /// A conversation turn began in `session`: its reads count from none again.
    pub fn chat_turn_started(&self, session: &str) {
        self.with(session, |ledger| ledger.chat_turn_reads = Some(0));
    }

    /// Pages read in the turn running in `session`; `None` when no conversation turn runs there,
    /// as in a long task's step.
    pub fn turn_reads(&self, session: &str) -> Option<usize> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions
            .iter()
            .find(|l| l.session == session)
            .and_then(|l| l.chat_turn_reads)
    }

    /// Records a read and returns its number.
    pub fn record(
        &self,
        session: &str,
        address: &str,
        title: &str,
        site: &str,
        lines_used: Vec<String>,
    ) -> usize {
        let lines_used = within(lines_used, MAX_USED_BYTES);
        self.with(session, |ledger| {
            if let Some(count) = ledger.chat_turn_reads.as_mut() {
                *count += 1;
            }
            let now = Utc::now();
            if let Some(read) = ledger.reads.iter_mut().find(|r| r.address == address) {
                let mut lines = std::mem::take(&mut read.lines_used);
                for line in lines_used {
                    if !lines.contains(&line) {
                        lines.push(line);
                    }
                }
                read.lines_used = within(lines, MAX_USED_BYTES);
                read.read_at = now;
                return read.number;
            }
            let number = ledger.reads.last().map_or(1, |r| r.number + 1);
            ledger.reads.push(SourceRead {
                number,
                address: address.to_string(),
                title: title.to_string(),
                site: site.to_string(),
                lines_used,
                read_at: now,
            });
            let excess = ledger.reads.len().saturating_sub(MAX_READS_PER_SESSION);
            ledger.reads.drain(..excess);
            number
        })
    }

    /// The pages `session` read, in the order it first read them.
    pub fn reads(&self, session: &str) -> Vec<SourceRead> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions
            .iter()
            .find(|l| l.session == session)
            .map(|l| l.reads.clone())
            .unwrap_or_default()
    }

    fn with<T>(&self, session: &str, f: impl FnOnce(&mut Ledger) -> T) -> T {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|l| now.saturating_duration_since(l.touched) < SESSION_LIFETIME);
        let index = match sessions.iter().position(|l| l.session == session) {
            Some(i) => i,
            None => {
                sessions.push(Ledger {
                    session: session.to_string(),
                    reads: Vec::new(),
                    chat_turn_reads: None,
                    touched: now,
                });
                let excess = sessions.len().saturating_sub(MAX_SESSIONS);
                sessions.drain(..excess);
                sessions.len() - 1
            }
        };
        let ledger = &mut sessions[index];
        ledger.touched = now;
        f(ledger)
    }
}

/// The ledger the page tool records into and the turn path resets.
pub fn global() -> &'static SourceLedger {
    static LEDGER: SourceLedger = SourceLedger::new();
    &LEDGER
}

/// The leading lines that fit in `max_bytes`.
fn within(lines: Vec<String>, max_bytes: usize) -> Vec<String> {
    let mut used = 0;
    lines
        .into_iter()
        .take_while(|l| {
            used += l.len() + 1;
            used <= max_bytes
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_turn_counts_its_reads_and_a_step_does_not() {
        let ledger = SourceLedger::new();
        assert_eq!(ledger.turn_reads("chat"), None);
        ledger.chat_turn_started("chat");
        assert_eq!(ledger.turn_reads("chat"), Some(0));
        ledger.record(
            "chat",
            "https://a.example/",
            "A",
            "a.example",
            vec!["one".into()],
        );
        ledger.record(
            "chat",
            "https://a.example/",
            "A",
            "a.example",
            vec!["two".into()],
        );
        assert_eq!(
            ledger.turn_reads("chat"),
            Some(2),
            "each read counts, a part too"
        );
        ledger.chat_turn_started("chat");
        assert_eq!(ledger.turn_reads("chat"), Some(0));
        ledger.record("step", "https://b.example/", "B", "b.example", Vec::new());
        assert_eq!(ledger.turn_reads("step"), None);
    }

    #[test]
    fn a_page_keeps_its_number_and_gathers_its_lines() {
        let ledger = SourceLedger::new();
        assert_eq!(
            ledger.record(
                "s",
                "https://a.example/",
                "A",
                "a.example",
                vec!["x".into()]
            ),
            1
        );
        assert_eq!(
            ledger.record("s", "https://b.example/", "B", "b.example", vec![]),
            2
        );
        assert_eq!(
            ledger.record(
                "s",
                "https://a.example/",
                "A",
                "a.example",
                vec!["x".into(), "y".into()]
            ),
            1
        );
        let reads = ledger.reads("s");
        assert_eq!(reads.len(), 2);
        assert_eq!(reads[0].lines_used, vec!["x".to_string(), "y".to_string()]);
        assert_eq!(reads[1].site, "b.example");
        assert!(ledger.reads("other").is_empty());
    }

    #[test]
    fn the_lines_kept_stay_within_their_bytes() {
        let ledger = SourceLedger::new();
        let long = "w".repeat(1_000);
        ledger.record(
            "s",
            "https://a.example/",
            "A",
            "a.example",
            vec![long.clone(), long.clone(), long],
        );
        let used: usize = ledger.reads("s")[0]
            .lines_used
            .iter()
            .map(|l| l.len() + 1)
            .sum();
        assert!(used <= MAX_USED_BYTES, "{used}");
    }
}
