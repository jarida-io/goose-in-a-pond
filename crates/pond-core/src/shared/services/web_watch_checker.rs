//! Checks the watches that are due, in code: read the page's items, compare them with the last
//! good check, push what matches to the owner, and keep the rest for the daily digest. A refusing
//! site is checked less often, doubling, and the owner is told once it has refused three times.

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::shared::domain::web_watch::{
    backoff_hours, compare, Comparison, WatchChange, CHANGE_HISTORY_DAYS, FAILURES_BEFORE_TELLING,
};
use crate::shared::ports::web_watch::{
    CheckOutcome, WatchNotice, WatchNotifier, WatchPageReader, WatchReadError, WebWatch,
    WebWatchRepository,
};

/// Watches checked in one pass at most, so a pass stays short.
const MAX_DUE_PER_PASS: usize = 8;
/// Lines a notice lists before saying how many more.
const NOTICE_LINES: usize = 3;
const DIGEST_LINES: usize = 5;

pub struct WebWatchChecker {
    repo: Arc<dyn WebWatchRepository>,
    reader: Arc<dyn WatchPageReader>,
    notifier: Arc<dyn WatchNotifier>,
}

impl WebWatchChecker {
    pub fn new(
        repo: Arc<dyn WebWatchRepository>,
        reader: Arc<dyn WatchPageReader>,
        notifier: Arc<dyn WatchNotifier>,
    ) -> Self {
        Self {
            repo,
            reader,
            notifier,
        }
    }

    /// Checks every watch due at `now`; returns how many were checked.
    pub async fn check_due(&self, now: DateTime<Utc>) -> Result<usize> {
        let due = self.repo.due(now, MAX_DUE_PER_PASS).await?;
        for watch in &due {
            if let Err(e) = self.check(watch, now).await {
                tracing::warn!(target: "giap::trace", kind = "web_watch_check_failed", watch = %watch.id, error = %e);
            }
        }
        Ok(due.len())
    }

    async fn check(&self, watch: &WebWatch, now: DateTime<Utc>) -> Result<()> {
        let read = self.reader.read(&watch.address).await;
        let (outcome, notice) = match read {
            Ok(items) => match compare(watch.items.as_deref(), &items, &watch.filters) {
                Comparison::Emptied => self.failed(watch, now, Failure::Emptied),
                comparison => {
                    let changes = match comparison {
                        Comparison::Changes(changes) => changes,
                        _ => Vec::new(),
                    };
                    let notice = match_notice(watch, &changes);
                    let outcome = CheckOutcome {
                        items: Some(items),
                        failures: 0,
                        checked_at: now,
                        next_check_at: now + Duration::hours(watch.interval_hours.into()),
                        changes,
                    };
                    (outcome, notice)
                }
            },
            Err(e) => self.failed(watch, now, Failure::Read(e)),
        };
        tracing::info!(
            target: "giap::trace",
            kind = "web_watch_check",
            watch = %watch.id,
            changes = outcome.changes.len(),
            matched = outcome.changes.iter().filter(|c| c.matched).count(),
            failures = outcome.failures,
        );
        self.repo.record_check(&watch.id, &outcome).await?;
        if let Some(notice) = notice {
            self.notifier.notify(notice).await?;
        }
        Ok(())
    }

    fn failed(
        &self,
        watch: &WebWatch,
        now: DateTime<Utc>,
        why: Failure,
    ) -> (CheckOutcome, Option<WatchNotice>) {
        let failures = watch.failures.saturating_add(1);
        let wait = backoff_hours(watch.interval_hours, failures);
        let notice = (failures == FAILURES_BEFORE_TELLING).then(|| WatchNotice {
            owner: watch.owner.clone(),
            title: format!("Can't check {}", watch.title),
            body: format!(
                "{} The pond will keep trying every {wait} hours.",
                why.words(failures)
            ),
            watch_id: Some(watch.id.clone()),
        });
        let outcome = CheckOutcome {
            items: watch.items.clone(),
            failures,
            checked_at: now,
            next_check_at: now + Duration::hours(wait.into()),
            changes: Vec::new(),
        };
        (outcome, notice)
    }

    /// Sends each owner one digest of the changes that did not match, and forgets changes past
    /// their history. Call once a day, at the digest hour.
    pub async fn send_digests(&self, now: DateTime<Utc>) -> Result<usize> {
        let mut sent = 0;
        for owner in self.repo.owners_with_undigested().await? {
            let changes = self.repo.undigested(&owner).await?;
            if changes.is_empty() {
                continue;
            }
            let lines: Vec<String> = changes.iter().map(|c| c.change.line()).collect();
            let watches: std::collections::HashSet<&str> =
                changes.iter().map(|c| c.watch_id.as_str()).collect();
            let notice = WatchNotice {
                owner: owner.clone(),
                title: format!(
                    "Today on your watches: {} change{}",
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" }
                ),
                body: listed(&lines, DIGEST_LINES),
                watch_id: (watches.len() == 1).then(|| changes[0].watch_id.clone()),
            };
            self.notifier.notify(notice).await?;
            let ids: Vec<i64> = changes.iter().map(|c| c.id).collect();
            self.repo.mark_digested(&ids).await?;
            sent += 1;
        }
        self.repo
            .prune_changes(now - Duration::days(CHANGE_HISTORY_DAYS))
            .await?;
        Ok(sent)
    }
}

enum Failure {
    Read(WatchReadError),
    Emptied,
}

impl Failure {
    fn words(&self, failures: u32) -> String {
        match self {
            Failure::Read(WatchReadError::Refused) => {
                format!("The site has refused the pond {failures} times in a row.")
            }
            Failure::Read(WatchReadError::RateLimited) => {
                format!("The site has asked the pond to wait {failures} times in a row.")
            }
            Failure::Read(WatchReadError::Gone) => {
                "The page is gone: the listing may have ended, or the site moved it.".to_string()
            }
            Failure::Read(WatchReadError::Other(_)) => {
                format!("The page couldn't be read {failures} times in a row.")
            }
            Failure::Emptied => "The page lists nothing now: it may have moved, or the site \
                                 changed how it is built."
                .to_string(),
        }
    }
}

/// The push for a check's matches, if any.
fn match_notice(watch: &WebWatch, changes: &[WatchChange]) -> Option<WatchNotice> {
    let lines: Vec<String> = changes
        .iter()
        .filter(|c| c.matched)
        .map(WatchChange::line)
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(WatchNotice {
        owner: watch.owner.clone(),
        title: match lines.len() {
            1 => format!("A match on {}", watch.title),
            n => format!("{n} matches on {}", watch.title),
        },
        body: listed(&lines, NOTICE_LINES),
        watch_id: Some(watch.id.clone()),
    })
}

fn listed(lines: &[String], shown: usize) -> String {
    let mut body = lines
        .iter()
        .take(shown)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let more = lines.len().saturating_sub(shown);
    if more > 0 {
        body.push_str(&format!("\nand {more} more"));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::web_watch::{WatchFilters, WatchItem};
    use crate::shared::ports::web_watch::{StoredChange, WatchEdit};
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Store {
        watches: Mutex<Vec<WebWatch>>,
        changes: Mutex<Vec<StoredChange>>,
    }

    #[async_trait]
    impl WebWatchRepository for Store {
        async fn create(&self, watch: &WebWatch) -> Result<()> {
            self.watches.lock().unwrap().push(watch.clone());
            Ok(())
        }
        async fn get(&self, id: &str) -> Result<Option<WebWatch>> {
            Ok(self
                .watches
                .lock()
                .unwrap()
                .iter()
                .find(|w| w.id == id)
                .cloned())
        }
        async fn list_for(&self, owner: &str) -> Result<Vec<WebWatch>> {
            Ok(self
                .watches
                .lock()
                .unwrap()
                .iter()
                .filter(|w| w.owner == owner)
                .cloned()
                .collect())
        }
        async fn due(&self, now: DateTime<Utc>, _limit: usize) -> Result<Vec<WebWatch>> {
            Ok(self
                .watches
                .lock()
                .unwrap()
                .iter()
                .filter(|w| !w.paused && w.next_check_at <= now)
                .cloned()
                .collect())
        }
        async fn record_check(&self, id: &str, outcome: &CheckOutcome) -> Result<()> {
            let mut watches = self.watches.lock().unwrap();
            let watch = watches.iter_mut().find(|w| w.id == id).unwrap();
            watch.items = outcome.items.clone();
            watch.failures = outcome.failures;
            watch.last_checked_at = Some(outcome.checked_at);
            watch.next_check_at = outcome.next_check_at;
            let mut changes = self.changes.lock().unwrap();
            for change in &outcome.changes {
                let id = changes.len() as i64 + 1;
                changes.push(StoredChange {
                    id,
                    watch_id: watch.id.clone(),
                    found_at: outcome.checked_at,
                    change: change.clone(),
                    digested: change.matched,
                });
            }
            Ok(())
        }
        async fn edit(&self, _owner: &str, _id: &str, _edit: &WatchEdit) -> Result<bool> {
            Ok(false)
        }
        async fn delete(&self, _owner: &str, _id: &str) -> Result<bool> {
            Ok(false)
        }
        async fn changes(&self, watch_id: &str, _limit: usize) -> Result<Vec<StoredChange>> {
            Ok(self
                .changes
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.watch_id == watch_id)
                .cloned()
                .collect())
        }
        async fn undigested(&self, owner: &str) -> Result<Vec<StoredChange>> {
            let mine: Vec<String> = self
                .list_for(owner)
                .await?
                .into_iter()
                .map(|w| w.id)
                .collect();
            Ok(self
                .changes
                .lock()
                .unwrap()
                .iter()
                .filter(|c| !c.digested && mine.contains(&c.watch_id))
                .cloned()
                .collect())
        }
        async fn owners_with_undigested(&self) -> Result<Vec<String>> {
            let changes = self.changes.lock().unwrap().clone();
            let watches = self.watches.lock().unwrap().clone();
            let mut owners: Vec<String> = watches
                .iter()
                .filter(|w| changes.iter().any(|c| !c.digested && c.watch_id == w.id))
                .map(|w| w.owner.clone())
                .collect();
            owners.dedup();
            Ok(owners)
        }
        async fn mark_digested(&self, ids: &[i64]) -> Result<()> {
            for c in self.changes.lock().unwrap().iter_mut() {
                if ids.contains(&c.id) {
                    c.digested = true;
                }
            }
            Ok(())
        }
        async fn prune_changes(&self, before: DateTime<Utc>) -> Result<u64> {
            let mut changes = self.changes.lock().unwrap();
            let n = changes.len();
            changes.retain(|c| c.found_at >= before);
            Ok((n - changes.len()) as u64)
        }
    }

    struct Pages(Mutex<Vec<std::result::Result<Vec<WatchItem>, WatchReadError>>>);

    #[async_trait]
    impl WatchPageReader for Pages {
        async fn read(
            &self,
            _address: &str,
        ) -> std::result::Result<Vec<WatchItem>, WatchReadError> {
            self.0.lock().unwrap().remove(0)
        }
    }

    #[derive(Default)]
    struct Told(Mutex<Vec<WatchNotice>>);

    #[async_trait]
    impl WatchNotifier for Told {
        async fn notify(&self, notice: WatchNotice) -> Result<()> {
            self.0.lock().unwrap().push(notice);
            Ok(())
        }
    }

    fn car(title: &str, price: &str, n: u32) -> WatchItem {
        WatchItem {
            title: title.into(),
            price: Some(price.into()),
            link: Some(format!("https://c.example/{n}")),
            ..Default::default()
        }
    }

    fn watch(now: DateTime<Utc>) -> WebWatch {
        WebWatch {
            id: "w1".into(),
            owner: "amina".into(),
            address: "https://c.example/used".into(),
            title: "Used cars".into(),
            looking_for: "a Forester under 1.5M".into(),
            filters: WatchFilters {
                words: vec!["forester".into()],
                max_price: Some(1_500_000.0),
                ..Default::default()
            },
            interval_hours: 6,
            paused: false,
            items: Some(vec![car("2015 Toyota RAV4", "1200000", 1)]),
            failures: 0,
            last_checked_at: None,
            next_check_at: now,
            created_at: now,
            session_id: None,
        }
    }

    fn checker(
        store: Arc<Store>,
        pages: Vec<std::result::Result<Vec<WatchItem>, WatchReadError>>,
        told: Arc<Told>,
    ) -> WebWatchChecker {
        WebWatchChecker::new(store, Arc::new(Pages(Mutex::new(pages))), told)
    }

    #[tokio::test]
    async fn a_match_is_pushed_at_once_and_the_rest_waits_for_the_digest() {
        let now = Utc::now();
        let store = Arc::new(Store::default());
        store.create(&watch(now)).await.unwrap();
        let told = Arc::new(Told::default());
        let page = vec![
            car("2016 Subaru Forester", "1400000", 2),
            car("2018 Mazda CX-5", "1900000", 3),
        ];
        let checker = checker(store.clone(), vec![Ok(page)], told.clone());
        assert_eq!(checker.check_due(now).await.unwrap(), 1);
        {
            let notices = told.0.lock().unwrap();
            assert_eq!(notices.len(), 1);
            assert_eq!(notices[0].owner, "amina");
            assert_eq!(notices[0].title, "A match on Used cars");
            assert_eq!(notices[0].body, "New: 2016 Subaru Forester, 1,400,000");
        }
        let watch = store.get("w1").await.unwrap().unwrap();
        assert_eq!(watch.next_check_at, now + Duration::hours(6));
        assert_eq!(watch.items.unwrap().len(), 2);
        assert_eq!(checker.send_digests(now).await.unwrap(), 1);
        let notices = told.0.lock().unwrap();
        assert_eq!(notices[1].title, "Today on your watches: 2 changes");
        assert!(
            notices[1].body.contains("New: 2018 Mazda CX-5"),
            "{}",
            notices[1].body
        );
        assert!(
            notices[1].body.contains("Gone: 2015 Toyota RAV4"),
            "{}",
            notices[1].body
        );
        assert!(
            !notices[1].body.contains("Forester"),
            "a pushed match is not digested again"
        );
    }

    #[tokio::test]
    async fn a_refusing_site_is_checked_less_often_and_the_owner_told_once() {
        let mut now = Utc::now();
        let store = Arc::new(Store::default());
        store.create(&watch(now)).await.unwrap();
        let told = Arc::new(Told::default());
        let refusals = (0..4).map(|_| Err(WatchReadError::Refused)).collect();
        let checker = checker(store.clone(), refusals, told.clone());
        let mut waits = Vec::new();
        for _ in 0..4 {
            checker.check_due(now).await.unwrap();
            let watch = store.get("w1").await.unwrap().unwrap();
            waits.push((watch.next_check_at - now).num_hours());
            now = watch.next_check_at;
        }
        assert_eq!(waits, vec![12, 24, 24, 24]);
        let notices = told.0.lock().unwrap();
        assert_eq!(notices.len(), 1, "told at the third refusal only");
        assert!(notices[0]
            .body
            .starts_with("The site has refused the pond 3 times in a row."));
        let watch = store.watches.lock().unwrap()[0].clone();
        assert_eq!(
            watch.items.unwrap().len(),
            1,
            "the last good items are kept"
        );
    }

    #[tokio::test]
    async fn an_emptied_page_counts_as_a_failure_not_as_everything_gone() {
        let now = Utc::now();
        let store = Arc::new(Store::default());
        store.create(&watch(now)).await.unwrap();
        let checker = checker(
            store.clone(),
            vec![Ok(Vec::new())],
            Arc::new(Told::default()),
        );
        checker.check_due(now).await.unwrap();
        let watch = store.get("w1").await.unwrap().unwrap();
        assert_eq!(watch.failures, 1);
        assert!(store.changes("w1", 10).await.unwrap().is_empty());
    }
}
