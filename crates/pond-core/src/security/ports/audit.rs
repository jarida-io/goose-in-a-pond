//! Audit umbrella: which store a record goes to.
//!
//! - [`EventLog`] (`events`): **the default**, for anything a user could ask the assistant
//!   about (what it did, touched, sent, who paired). Backs `GET /api/v1/activity`.
//! - [`OperationalLogRepository`] (`event_log` table): drained tracing output for
//!   `GET /api/v1/logs`, written only by `pond-server`'s drain. Kept out of [`EventLog`] so
//!   log noise does not bury the activity feed.
//! - [`TelemetryPort`] (`turn_metrics`): completed-turn metrics for the telemetry dashboard,
//!   kept separate because re-aggregating over the event store risks wrong numbers.
//! - `memory_repository::log_event`: memory-mutation audit; lives in
//!   [`crate::user_data::ports::memory_repository`], deliberately not re-exported here.
//!
//! Sensors and cameras already emit into [`EventLog`] (`BusEvent::to_event`); their typed tables
//! stay since an append-only bag can't serve range queries or hold camera `acknowledged` state.
pub use crate::security::ports::event_log::*;
pub use crate::security::ports::telemetry::*;
