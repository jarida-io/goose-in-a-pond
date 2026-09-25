//! Joins [`producer`](crate::context::producer) to [`IngestPipeline`].
//! Not a bus subscriber: callers pass fresh `Settings` per event, or new PUTs would be ignored.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::context::ingest::{IngestOutcome, IngestPipeline};
use crate::context::ports::ContextRepository;
use crate::context::producer::BusProducer;
use crate::shared::ports::event_bus::BusEvent;
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::settings::Settings;

/// Scope for enumerating sources; each source supplies its own `profile_id` to its items.
/// A constant, not an argument: a session's `Guest` scope would stop the pond recording.
pub const INGEST_SCOPE: ProfileScope = ProfileScope::Household;

/// What one bus event did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AbsorbReport {
    /// One per stored row. Plural because two members may follow one device.
    pub ingested: Vec<IngestOutcome>,
    /// Sources that were offered the event and said no.
    pub refused: usize,
    /// Accepted but unwritable rows; counted, not an error, so other sources still get theirs.
    pub storage_failures: usize,
}

impl AbsorbReport {
    pub fn stored(&self) -> usize {
        self.ingested.len()
    }
}

#[derive(Debug, Error)]
pub enum AbsorbError {
    /// The source list was unreadable; an error so a broken store doesn't look unconfigured.
    #[error(
        "could not read this household's context sources, so the event was offered to none of \
         them: {0}"
    )]
    SourcesUnreadable(#[source] anyhow::Error),
}

/// Turns bus events into stored context items.
pub struct BusIngest {
    repo: Arc<dyn ContextRepository>,
    pipeline: Arc<IngestPipeline>,
}

impl BusIngest {
    pub fn new(repo: Arc<dyn ContextRepository>, pipeline: Arc<IngestPipeline>) -> Self {
        Self { repo, pipeline }
    }

    /// Offer one bus event to every source that might follow it.
    pub async fn absorb(
        &self,
        settings: &Settings,
        event: &BusEvent,
        now: DateTime<Utc>,
    ) -> Result<AbsorbReport, AbsorbError> {
        let producer = BusProducer::from_settings(settings);
        // Before the store read, so a default pond pays one bool per event and no query.
        if !producer.is_enabled() {
            return Ok(AbsorbReport::default());
        }

        let sources = self
            .repo
            .list_sources(&INGEST_SCOPE)
            .await
            .map_err(AbsorbError::SourcesUnreadable)?;

        let mut report = AbsorbReport::default();
        for source in &sources {
            let raw = match producer.raw_item_for(source, event) {
                Ok(raw) => raw,
                Err(reason) => {
                    report.refused += 1;
                    tracing::trace!(
                        target: "giap::trace",
                        source_id = %source.id(),
                        source_kind = %source.kind().as_str(),
                        reason = %reason,
                        "[context] a bus event was not ingested"
                    );
                    continue;
                }
            };
            match self.pipeline.ingest(source, raw, now).await {
                Ok(outcome) => report.ingested.push(outcome),
                Err(e) => {
                    report.storage_failures += 1;
                    tracing::warn!(
                        source_id = %source.id(),
                        error = %e,
                        "[context] a bus event matched a source and could not be stored"
                    );
                }
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::domain::{ContextSource, SourceKind, SourceParts, SourceStatus};
    use crate::context::mocks::mock_context_repository::MockContextRepository;
    use crate::security::domain::redaction::RedactionKind;
    use crate::security::mocks::mock_redactor::MockRedactor;
    use crate::user_data::domain::sensor::SensorReading;

    const HALL_PIR: &str = "hall-pir";
    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("representable instant")
    }

    fn on() -> Settings {
        Settings {
            context_ingest_enabled: true,
            ..Settings::default()
        }
    }

    fn source(id: &str, owner: &str, provider: &str) -> ContextSource {
        ContextSource::from_parts(SourceParts {
            id: id.into(),
            kind: SourceKind::Sensor,
            provider: provider.into(),
            profile_id: owner.into(),
            scopes: vec![],
            cursor: None,
            last_sync: None,
            status: SourceStatus::Connected,
            secret_ref: None,
            created_at: at(0),
        })
        .expect("valid source")
    }

    fn motion(device: &str, secs: i64) -> BusEvent {
        BusEvent::Sensor(SensorReading {
            device_id: device.into(),
            sensor_type: "motion".into(),
            value: 1.0,
            unit: "bool".into(),
            recorded_at: at(secs),
        })
    }

    fn wire(repo: Arc<MockContextRepository>) -> BusIngest {
        let redactor = Arc::new(MockRedactor::replacing(KEY, RedactionKind::ApiKey));
        BusIngest::new(repo.clone(), Arc::new(IngestPipeline::new(repo, redactor)))
    }

    #[tokio::test]
    async fn an_event_from_a_followed_device_is_stored_under_the_sources_owner() {
        let repo = Arc::new(MockContextRepository::new());
        repo.upsert_source(&source("src-jerry", "jerry", HALL_PIR))
            .await
            .unwrap();
        let ingest = wire(repo.clone());

        let report = ingest
            .absorb(&on(), &motion(HALL_PIR, 10), at(11))
            .await
            .expect("the source list is readable");
        assert_eq!(report.stored(), 1, "nothing was stored: {report:?}");
        assert_eq!(report.refused, 0);

        let stored = repo.all_items();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].profile_id(), "jerry");
        assert_eq!(stored[0].source_id(), "src-jerry");
    }

    #[tokio::test]
    async fn a_default_pond_stores_nothing_and_does_not_even_read_the_store() {
        // The store fails every read, so success here means it was never read.
        let repo = Arc::new(
            MockContextRepository::new().with_unreadable_sources("the store must not be read"),
        );
        let ingest = wire(repo);
        let report = ingest
            .absorb(&Settings::default(), &motion(HALL_PIR, 10), at(11))
            .await
            .expect("a disabled producer must not touch the store");
        assert_eq!(report, AbsorbReport::default());

        // Vacuity control: with the toggle on, the same store does get read.
        let armed = Arc::new(
            MockContextRepository::new().with_unreadable_sources("the store must not be read"),
        );
        let ingest = wire(armed);
        assert!(ingest
            .absorb(&on(), &motion(HALL_PIR, 10), at(11))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn an_unreadable_source_list_refuses_rather_than_reporting_no_sources() {
        let repo = Arc::new(MockContextRepository::new().with_unreadable_sources("disk is gone"));
        let ingest = wire(repo);
        let err = ingest
            .absorb(&on(), &motion(HALL_PIR, 10), at(11))
            .await
            .expect_err("an unreadable store must not be reported as an empty one");
        assert!(
            matches!(err, AbsorbError::SourcesUnreadable(_)),
            "wrong refusal: {err}"
        );
        assert!(
            err.to_string().contains("disk is gone"),
            "the refusal drops the cause, so an operator cannot act on it: {err}"
        );
    }

    #[tokio::test]
    async fn one_event_reaches_every_source_that_follows_it_and_no_others() {
        let repo = Arc::new(MockContextRepository::new());
        repo.upsert_source(&source("src-jerry", "jerry", HALL_PIR))
            .await
            .unwrap();
        repo.upsert_source(&source("src-liz", "liz", HALL_PIR))
            .await
            .unwrap();
        repo.upsert_source(&source("src-porch", "jerry", "porch-pir"))
            .await
            .unwrap();
        let ingest = wire(repo.clone());

        let report = ingest
            .absorb(&on(), &motion(HALL_PIR, 10), at(11))
            .await
            .unwrap();
        assert_eq!(report.stored(), 2, "both followers must get a row");
        assert_eq!(report.refused, 1, "the porch source must be refused");

        let owners: Vec<String> = repo
            .all_items()
            .iter()
            .map(|i| i.profile_id().to_string())
            .collect();
        assert_eq!(owners, vec!["jerry".to_string(), "liz".to_string()]);
    }

    #[tokio::test]
    async fn a_failed_write_is_counted_rather_than_abandoning_the_event() {
        let repo =
            Arc::new(MockContextRepository::new().with_unwritable_items("no space left on device"));
        repo.upsert_source(&source("src-jerry", "jerry", HALL_PIR))
            .await
            .unwrap();
        let ingest = wire(repo.clone());

        let report = ingest
            .absorb(&on(), &motion(HALL_PIR, 10), at(11))
            .await
            .expect("a write failure is not a read failure");
        assert_eq!(report.storage_failures, 1);
        assert_eq!(report.stored(), 0);
        assert!(repo.all_items().is_empty());
    }

    #[tokio::test]
    async fn re_delivering_one_event_through_the_service_writes_one_row() {
        let repo = Arc::new(MockContextRepository::new());
        repo.upsert_source(&source("src-jerry", "jerry", HALL_PIR))
            .await
            .unwrap();
        let ingest = wire(repo.clone());

        for tick in 0..3 {
            ingest
                .absorb(&on(), &motion(HALL_PIR, 10), at(100 + tick))
                .await
                .unwrap();
        }
        assert_eq!(
            repo.all_items().len(),
            1,
            "three deliveries of one reading wrote {} rows",
            repo.all_items().len()
        );

        // Vacuity control: a different reading does write a second row.
        ingest
            .absorb(&on(), &motion(HALL_PIR, 20), at(200))
            .await
            .unwrap();
        assert_eq!(repo.all_items().len(), 2);
    }

    /// A narrower scope silently empties the corpus; a wrong `Owner` misattributes.
    #[test]
    fn the_enumeration_scope_is_the_households() {
        assert_eq!(INGEST_SCOPE, ProfileScope::Household);
        assert_ne!(INGEST_SCOPE, ProfileScope::Guest);
    }
}
