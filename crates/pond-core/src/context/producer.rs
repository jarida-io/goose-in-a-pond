//! The on-pond producer: pure, from a [`BusEvent`] and [`ContextSource`] to a [`RawItem`].
//! Refusals are named [`NotIngested`]s. The [`BusEvent`] match has no wildcard arm, by design.

use chrono::{DateTime, SecondsFormat, Utc};
use thiserror::Error;

use crate::context::domain::{
    ContextSource, ItemKind, SourceAvailability, SourceKind, SourceStatus,
};
use crate::context::ingest::RawItem;
use crate::shared::ports::event_bus::BusEvent;
use crate::user_data::domain::sensor::{CameraEvent, SensorReading};
use crate::user_data::domain::settings::Settings;

// ── The worth-keeping policy ────────────────────────────────────────────────

/// Signals whose readings are transitions, not samples (those live in `sensor_readings`).
/// Keyed on the signal, never the value: Matter's `contact` is `true = closed`.
pub const DISCRETE_SENSOR_TYPES: &[&str] = &[
    // Minted in this tree: `motion` (sensor route), `occupancy`/`contact` (Matter clusters).
    "motion",
    "occupancy",
    "contact",
    // The same shape from bridges that name them separately; all binary.
    "door",
    "window",
    "smoke",
    "leak",
    "button",
    // One bit like `leak`, off the same Matter cluster; only the endpoint's device type differs.
    "freeze",
    "rain",
    // Matter `SmokeCoAlarm`'s name for `smoke`: tri-state, but rare and every change matters.
    "smoke_alarm",
    // Carbon monoxide, and the battery that lets either alarm sound; same shape as `smoke_alarm`.
    "co_alarm",
    "alarm_battery",
    // Air-purifier filter alerts: 0/1/2 but rare and actionable. The tripwire's extraction misses
    // the Matter bridge's shape for these, so only this list keeps them.
    "hepa_filter_change",
    "carbon_filter_change",
];

/// Event types meaning "the pixels changed": vision emits `motion` when no classifier ran.
pub const UNCLASSIFIED_CAMERA_EVENT_TYPES: &[&str] = &["motion"];

/// A classified camera event below this confidence is dropped; `None` is kept.
/// Matches `pond-adapters-vision`'s `MIN_CLASSIFIER_CONFIDENCE`.
pub const MIN_CAMERA_CONFIDENCE: f64 = 0.5;

// ── Refusals ────────────────────────────────────────────────────────────────

/// Why the worth-keeping rule dropped an event otherwise addressed to this source.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum DropReason {
    #[error(
        "`{signal}` is a measurement, not a transition: the reading series already lives in \
         sensor_readings under retention_sensor_days, and copying it into the context corpus \
         costs 2880 rows a day per device and answers no question"
    )]
    ContinuousSample { signal: String },
    #[error(
        "a camera event typed `{event_type}` says the pixels changed and names nothing; without \
         a classifier label there is no household fact to record"
    )]
    NothingNamedIt { event_type: String },
    #[error(
        "a camera event at confidence {confidence} is below the {MIN_CAMERA_CONFIDENCE} floor \
         the on-device classifier itself applies"
    )]
    LowConfidence { confidence: f64 },
}

/// Why a bus event did not become a context item.
/// Missing-prerequisite variants quote [`SourceAvailability::refusal`] rather than restating it.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum NotIngested {
    #[error(
        "the on-pond context producer is off; set `context_ingest_enabled` to turn it on, which \
         nobody has done by upgrading"
    )]
    Disabled,

    #[error("cannot ingest from a {kind} source: {reason}")]
    SourceUnavailable {
        kind: &'static str,
        reason: &'static str,
    },

    #[error(
        "a voice source is not fed from the bus: memory extraction already curates facts out of \
         conversation turns, and a second uncurated copy of every turn would spend the retrieval \
         budget twice to recall worse"
    )]
    VoiceIsCuratedByMemoryExtraction,

    #[error("nothing on this pond's event bus produces data for a {kind} source")]
    NoBusEventFeedsThisKind { kind: &'static str },

    #[error(
        "this source is `{status}` rather than connected, and only a connected source ingests: \
         pausing a source is the one control a member has that stops the copying without \
         deleting what is already there, and a producer that ignored it would make that \
         control a no-op"
    )]
    SourceNotConnected { status: &'static str },

    #[error("a {event} event cannot feed a {source_kind} source")]
    WrongFamilyForSource {
        source_kind: &'static str,
        event: &'static str,
    },

    #[error("this source follows `{follows}`; the event came from `{came_from}`")]
    DifferentDevice { follows: String, came_from: String },

    #[error(
        "a {event} event is the pond noticing itself rather than a household fact, and a corpus \
         of the pond's own heartbeat teaches a model that the passage of time is something to \
         have opinions about"
    )]
    PondNoticingItself { event: &'static str },

    #[error(
        "a presence event names its own household member, and this source has one owner: \
         ingesting it here would file one member's movements in another member's corpus, which \
         is the misattribution `profile_id` is non-optional to prevent"
    )]
    WouldMisattribute,

    #[error(
        "no landed source kind describes a device state change: the only SourceKind whose \
         retention category is `device` is `mobile`, which has not landed, so there is no \
         sensitivity floor or retention window chosen for this data"
    )]
    NoLandedSourceKindDescribesIt,

    #[error(transparent)]
    NotWorthKeeping(#[from] DropReason),
}

// ── The producer ────────────────────────────────────────────────────────────

/// Turns bus events into [`RawItem`]s for the sources that follow them.
/// Built only from [`Settings`], so no caller can skip reading `context_ingest_enabled`.
#[derive(Debug, Clone)]
pub struct BusProducer {
    enabled: bool,
}

impl BusProducer {
    /// The only constructor.
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.context_ingest_enabled,
        }
    }

    /// Whether anything will be produced at all.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// One event, one source: an item or a named refusal. Pure: no clock, store or global.
    pub fn raw_item_for(
        &self,
        source: &ContextSource,
        event: &BusEvent,
    ) -> Result<RawItem, NotIngested> {
        if !self.enabled {
            return Err(NotIngested::Disabled);
        }

        // 1. Availability, quoting the table's own sentence; refused whatever the event.
        let availability = source.kind().availability();
        if availability != SourceAvailability::Landed {
            return Err(NotIngested::SourceUnavailable {
                kind: source.kind().as_str(),
                reason: availability.refusal(),
            });
        }

        // 2. Which landed kinds the bus feeds. No wildcard, so promoting a kind to `Landed` can't
        //    silently make it bus-ingestable.
        match source.kind() {
            SourceKind::Sensor | SourceKind::Camera => {}
            SourceKind::Voice => return Err(NotIngested::VoiceIsCuratedByMemoryExtraction),
            SourceKind::Mobile
            | SourceKind::Mail
            | SourceKind::Calendar
            | SourceKind::Files
            | SourceKind::Chat => {
                return Err(NotIngested::NoBusEventFeedsThisKind {
                    kind: source.kind().as_str(),
                })
            }
        }

        // 3. Source state: only `Connected` ingests; pausing stops copying without deleting.
        match source.status() {
            SourceStatus::Connected => {}
            status @ (SourceStatus::NeedsReauth | SourceStatus::Error | SourceStatus::Paused) => {
                return Err(NotIngested::SourceNotConnected {
                    status: status.as_str(),
                })
            }
        }

        // 4. The event. NO WILDCARD ARM.
        match event {
            BusEvent::Sensor(reading) => self.item_from_sensor(source, reading),
            BusEvent::Camera(camera) => self.item_from_camera(source, camera),
            // Only `Mobile` retains as `Device`, and it hasn't landed.
            BusEvent::Device(_) => Err(NotIngested::NoLandedSourceKindDescribesIt),
            // The pond noticing itself; `proactive_review::reviewable` refuses the same pair.
            BusEvent::Time(_) => Err(NotIngested::PondNoticingItself { event: "time" }),
            BusEvent::Session(_) => Err(NotIngested::PondNoticingItself { event: "session" }),
            // Presence carries its own `profile_id`: no correct owner to store it under.
            BusEvent::Presence(_) => Err(NotIngested::WouldMisattribute),
        }
    }

    /// Every (source, item) pair one event produces across a household's sources.
    /// Refusals are dropped; use [`raw_item_for`](Self::raw_item_for) to see them.
    pub fn items_for<'a>(
        &self,
        sources: &'a [ContextSource],
        event: &BusEvent,
    ) -> Vec<(&'a ContextSource, RawItem)> {
        sources
            .iter()
            .filter_map(|source| {
                self.raw_item_for(source, event)
                    .ok()
                    .map(|item| (source, item))
            })
            .collect()
    }

    fn item_from_sensor(
        &self,
        source: &ContextSource,
        reading: &SensorReading,
    ) -> Result<RawItem, NotIngested> {
        if source.kind() != SourceKind::Sensor {
            return Err(NotIngested::WrongFamilyForSource {
                source_kind: source.kind().as_str(),
                event: "sensor",
            });
        }
        if reading.device_id != source.provider() {
            return Err(NotIngested::DifferentDevice {
                follows: source.provider().to_string(),
                came_from: reading.device_id.clone(),
            });
        }
        sensor_is_worth_keeping(reading)?;

        let unit = reading.unit.trim();
        let body = if unit.is_empty() {
            format!(
                "{} reported {} = {}",
                reading.device_id, reading.sensor_type, reading.value
            )
        } else {
            format!(
                "{} reported {} = {} {}",
                reading.device_id, reading.sensor_type, reading.value, unit
            )
        };

        Ok(RawItem {
            external_id: sensor_external_id(reading),
            kind: ItemKind::Event,
            occurred_at: reading.recorded_at,
            title: format!("{} at {}", reading.sensor_type, reading.device_id),
            body,
            // Must stay empty: a reading names no person.
            participants: Vec::new(),
        })
    }

    fn item_from_camera(
        &self,
        source: &ContextSource,
        camera: &CameraEvent,
    ) -> Result<RawItem, NotIngested> {
        if source.kind() != SourceKind::Camera {
            return Err(NotIngested::WrongFamilyForSource {
                source_kind: source.kind().as_str(),
                event: "camera",
            });
        }
        if camera.camera_id != source.provider() {
            return Err(NotIngested::DifferentDevice {
                follows: source.provider().to_string(),
                came_from: camera.camera_id.clone(),
            });
        }
        camera_is_worth_keeping(camera)?;

        let body = match camera.confidence {
            Some(c) => format!(
                "{} saw {} (confidence {c})",
                camera.camera_id, camera.event_type
            ),
            None => format!("{} saw {}", camera.camera_id, camera.event_type),
        };

        Ok(RawItem {
            external_id: camera_external_id(camera),
            kind: ItemKind::Event,
            occurred_at: camera.created_at,
            title: format!("{} at {}", camera.event_type, camera.camera_id),
            // `snapshot_path` and `metadata` stay on the camera_events row: unbounded token cost.
            body,
            participants: Vec::new(),
        })
    }
}

// ── Idempotency ─────────────────────────────────────────────────────────────

/// The external id of a sensor reading, from the event alone so re-delivery updates the row.
fn sensor_external_id(reading: &SensorReading) -> String {
    format!(
        "sensor:{}:{}:{}",
        reading.device_id,
        reading.sensor_type,
        instant(reading.recorded_at)
    )
}

/// The external id of a camera event: `camera:{camera}:{type}:{instant}`.
/// Not [`CameraEvent::id`], which is `None` until persisted and would mint a second row.
fn camera_external_id(camera: &CameraEvent) -> String {
    format!(
        "camera:{}:{}:{}",
        camera.camera_id,
        camera.event_type,
        instant(camera.created_at)
    )
}

/// Milliseconds, UTC, `Z`-suffixed, so the key never moves with host locale or offset.
fn instant(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

// ── Worth keeping ───────────────────────────────────────────────────────────

/// See [`DISCRETE_SENSOR_TYPES`].
fn sensor_is_worth_keeping(reading: &SensorReading) -> Result<(), DropReason> {
    let signal = reading.sensor_type.trim().to_ascii_lowercase();
    if DISCRETE_SENSOR_TYPES.contains(&signal.as_str()) {
        Ok(())
    } else {
        Err(DropReason::ContinuousSample {
            signal: reading.sensor_type.clone(),
        })
    }
}

/// See [`UNCLASSIFIED_CAMERA_EVENT_TYPES`] and [`MIN_CAMERA_CONFIDENCE`].
fn camera_is_worth_keeping(camera: &CameraEvent) -> Result<(), DropReason> {
    let label = camera.event_type.trim().to_ascii_lowercase();
    // A blank label names nothing either; it's what an adapter that forgot the field sends.
    if label.is_empty() || UNCLASSIFIED_CAMERA_EVENT_TYPES.contains(&label.as_str()) {
        return Err(DropReason::NothingNamedIt {
            event_type: camera.event_type.clone(),
        });
    }
    // Written as "at or above is kept" so a client-sent `NaN` (every comparison false) is refused.
    match camera.confidence {
        None => Ok(()),
        Some(c) if c >= MIN_CAMERA_CONFIDENCE => Ok(()),
        Some(c) => Err(DropReason::LowConfidence { confidence: c }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::domain::{SourceParts, SourceStatus};
    use crate::security::domain::event::EventCategory;
    use crate::shared::domain::session_activity::{
        PresenceTransition, ProfilePresence, SessionLifecycle, SessionPhase,
    };
    use crate::shared::domain::time_tick::{TimeBoundary, TimeTick};
    use crate::user_data::domain::device::{DeviceStateChanged, DeviceStateValue};
    use crate::user_data::domain::session::IdentificationSource;

    const HALL_PIR: &str = "hall-pir";
    const DOOR_CAM: &str = "front-door-cam";

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("representable instant")
    }

    fn producer() -> BusProducer {
        BusProducer::from_settings(&Settings {
            context_ingest_enabled: true,
            ..Settings::default()
        })
    }

    fn source(kind: SourceKind, provider: &str) -> ContextSource {
        source_in(kind, provider, SourceStatus::Connected)
    }

    fn source_in(kind: SourceKind, provider: &str, status: SourceStatus) -> ContextSource {
        ContextSource::from_parts(SourceParts {
            id: format!("src-{}-{provider}", kind.as_str()),
            kind,
            provider: provider.to_string(),
            profile_id: "jerry".into(),
            scopes: vec![],
            cursor: None,
            last_sync: None,
            status,
            secret_ref: None,
            created_at: at(0),
        })
        .expect("valid source")
    }

    fn reading(device: &str, signal: &str, value: f64, secs: i64) -> BusEvent {
        BusEvent::Sensor(SensorReading {
            device_id: device.into(),
            sensor_type: signal.into(),
            value,
            unit: "bool".into(),
            recorded_at: at(secs),
        })
    }

    fn camera(camera_id: &str, event_type: &str, confidence: Option<f64>, secs: i64) -> BusEvent {
        BusEvent::Camera(CameraEvent {
            id: Some(7),
            camera_id: camera_id.into(),
            event_type: event_type.into(),
            confidence,
            snapshot_path: Some("/var/pond/snap.jpg".into()),
            metadata: Some(r#"{"changed_fraction":0.31}"#.into()),
            acknowledged: false,
            created_at: at(secs),
        })
    }

    /// A variant's name, by an exhaustive match so a new `BusEvent` variant won't compile here.
    fn variant_name(event: &BusEvent) -> &'static str {
        match event {
            BusEvent::Sensor(_) => "sensor",
            BusEvent::Camera(_) => "camera",
            BusEvent::Device(_) => "device",
            BusEvent::Time(_) => "time",
            BusEvent::Presence(_) => "presence",
            BusEvent::Session(_) => "session",
        }
    }

    /// One of every [`BusEvent`] variant, labelled by [`variant_name`].
    fn one_of_each_variant() -> Vec<(&'static str, BusEvent)> {
        let every = vec![
            ("sensor", reading(HALL_PIR, "motion", 1.0, 10)),
            ("camera", camera(DOOR_CAM, "person", Some(0.91), 10)),
            (
                "device",
                BusEvent::Device(DeviceStateChanged {
                    device_id: "kitchen-lamp".into(),
                    key: "power".into(),
                    value: DeviceStateValue::Bool(true),
                    changed_at: at(10),
                }),
            ),
            (
                "time",
                BusEvent::Time(TimeTick {
                    boundary: TimeBoundary::Hour,
                    at: at(10),
                    local_hour: 12,
                }),
            ),
            (
                "presence",
                BusEvent::Presence(ProfilePresence {
                    profile_id: "liz".into(),
                    transition: PresenceTransition::Arrived,
                    source: IdentificationSource::Face,
                    confidence: Some(0.71),
                    session_id: "sess-1".into(),
                    at: at(10),
                }),
            ),
            (
                "session",
                BusEvent::Session(SessionLifecycle {
                    phase: SessionPhase::Idle,
                    session_id: None,
                    at: at(10),
                    idle_secs: 900,
                }),
            ),
        ];
        for (label, event) in &every {
            assert_eq!(
                *label,
                variant_name(event),
                "the fixture labels a {} event as `{label}`, and every sweep below branches on \
                 that label -- so the assertions would be made about the wrong variant",
                variant_name(event)
            );
        }
        // And every variant appears exactly once.
        let mut labels: Vec<&str> = every.iter().map(|(l, _)| *l).collect();
        labels.sort_unstable();
        assert_eq!(
            labels,
            vec!["camera", "device", "presence", "sensor", "session", "time"],
            "BusEvent gained, lost or renamed a variant. Every consumer of this fixture asserts a \
             disposition per variant, so extend the fixture before deciding what the new one means"
        );
        every
    }

    // ── 1. Every variant is dispositioned ───────────────────────────────────

    /// A single `_ =>` silently switches off exhaustiveness: no compile error, no failing test.
    #[test]
    fn the_event_match_has_no_wildcard_arm() {
        const SRC: &str = include_str!("producer.rs");
        let block = SRC
            .split("// 4. The event. NO WILDCARD ARM.")
            .nth(1)
            .and_then(|s| s.split("\n    }\n").next())
            .expect(
                "the marker comment above the event match is gone, so this guard reads nothing",
            );

        // Vacuity controls: the extraction found the match, not the whole file.
        assert!(
            block.contains("match event {"),
            "the extracted fragment is not the event match: {block}"
        );
        assert!(
            block.len() < SRC.len() / 2,
            "the extraction swallowed most of the file, so `_ =>` anywhere else would fail this \
             test and a wildcard in the match would not be what it is reporting"
        );

        assert!(
            !block.contains("_ =>"),
            "the event match grew a wildcard arm. A seventh BusEvent variant now compiles \
             without anybody deciding what it means for the context corpus, which is the one \
             mechanism that survives whoever wrote this module leaving:\n{block}"
        );
        for variant in [
            "BusEvent::Sensor",
            "BusEvent::Camera",
            "BusEvent::Device",
            "BusEvent::Time",
            "BusEvent::Presence",
            "BusEvent::Session",
        ] {
            assert!(
                block.contains(variant),
                "`{variant}` has no arm of its own in the event match"
            );
        }
    }

    /// The four refusals must name four different reasons.
    #[test]
    fn every_bus_event_variant_is_dispositioned() {
        let p = producer();
        let sensor_src = source(SourceKind::Sensor, HALL_PIR);
        let camera_src = source(SourceKind::Camera, DOOR_CAM);

        let mut produced = 0usize;
        let mut refusals: Vec<NotIngested> = Vec::new();

        for (name, event) in one_of_each_variant() {
            // Offer each event to both bus-fed kinds; neither may ingest it by accident.
            let answers = [
                p.raw_item_for(&sensor_src, &event),
                p.raw_item_for(&camera_src, &event),
            ];
            let ok = answers.iter().filter(|a| a.is_ok()).count();
            match name {
                "sensor" | "camera" => {
                    assert_eq!(
                        ok, 1,
                        "a {name} event must be ingested by exactly its own source kind, got {ok} \
                         acceptances: {answers:?}"
                    );
                    produced += 1;
                }
                _ => {
                    assert_eq!(ok, 0, "a {name} event became a context item: {answers:?}");
                    for answer in answers {
                        refusals.push(answer.expect_err("just asserted no acceptance"));
                    }
                }
            }
        }

        assert_eq!(produced, 2, "neither device-shaped event produced an item");

        // Assert each refusal's variant: a count-only check passes if everything says `Disabled`.
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, NotIngested::NoLandedSourceKindDescribesIt)),
            "a device state change was refused without saying no source kind describes it: \
             {refusals:?}"
        );
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, NotIngested::PondNoticingItself { event: "time" })),
            "a time tick was not refused as the pond noticing itself: {refusals:?}"
        );
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, NotIngested::PondNoticingItself { event: "session" })),
            "a session transition was not refused as the pond noticing itself: {refusals:?}"
        );
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, NotIngested::WouldMisattribute)),
            "a presence event was refused without naming the misattribution: {refusals:?}"
        );
    }

    /// The premise of the `Device` refusal; if a `Device`-retention kind lands, revisit that arm.
    #[test]
    fn no_landed_source_kind_describes_a_device_state_change() {
        let landed_device_kinds: Vec<&str> = SourceKind::ALL
            .into_iter()
            .filter(|k| {
                k.retention_category() == EventCategory::Device
                    && k.availability() == SourceAvailability::Landed
            })
            .map(|k| k.as_str())
            .collect();
        assert!(
            landed_device_kinds.is_empty(),
            "{landed_device_kinds:?} now describes device state and HAS landed, so \
             `NoLandedSourceKindDescribesIt` is no longer true. Decide what a device state change \
             becomes before this ships."
        );
        // Vacuity control: the filter does match some kind.
        assert!(
            SourceKind::ALL
                .into_iter()
                .any(|k| k.retention_category() == EventCategory::Device),
            "no SourceKind maps to the Device retention category at all, so the assertion above \
             is vacuous"
        );
    }

    #[test]
    fn a_voice_source_is_refused_even_though_its_kind_has_landed() {
        assert_eq!(
            SourceKind::Voice.availability(),
            SourceAvailability::Landed,
            "if voice ever stops being Landed this test is asserting the wrong thing -- it exists \
             precisely because the availability table does NOT refuse voice"
        );
        let p = producer();
        for (_, event) in one_of_each_variant() {
            assert_eq!(
                p.raw_item_for(&source(SourceKind::Voice, HALL_PIR), &event),
                Err(NotIngested::VoiceIsCuratedByMemoryExtraction),
                "a voice source ingested {event:?}"
            );
        }
    }

    // ── 2. A kind that has not landed is refused, with the sentence ─────────

    /// The refusal quotes `SourceAvailability::refusal()` verbatim, so the two can't drift.
    #[test]
    fn a_source_whose_kind_is_not_landed_is_refused_with_the_sentence_naming_what_is_missing() {
        let p = producer();
        let event = reading(HALL_PIR, "motion", 1.0, 10);

        let mut refused = 0usize;
        for kind in SourceKind::ALL {
            let availability = kind.availability();
            if availability == SourceAvailability::Landed {
                continue;
            }
            refused += 1;
            let err = p
                .raw_item_for(&source(kind, HALL_PIR), &event)
                .expect_err("an unlanded kind must be refused");
            assert_eq!(
                err,
                NotIngested::SourceUnavailable {
                    kind: kind.as_str(),
                    reason: availability.refusal(),
                },
                "{} was refused for the wrong reason",
                kind.as_str()
            );
            let rendered = err.to_string();
            assert!(
                rendered.contains(kind.as_str()),
                "the refusal does not say which kind: {rendered}"
            );
            assert!(
                !availability.refusal().is_empty() && rendered.contains(availability.refusal()),
                "the refusal does not carry the availability table's own sentence: {rendered}"
            );
            // Pin the missing mechanism, not a phase id, which expires.
            assert!(
                rendered.contains("ingest route") || rendered.contains("connector"),
                "the refusal does not name what has to land first: {rendered}"
            );
        }

        // Vacuity controls, both directions.
        assert_eq!(
            refused,
            SourceKind::ALL.len() - 5,
            "sensor, camera, voice, mail and calendar have a path; files and chat do not. If \
             that changed, this sweep is no longer testing what it claims"
        );
        assert!(
            p.raw_item_for(&source(SourceKind::Sensor, HALL_PIR), &event)
                .is_ok(),
            "no source kind produces an item, so the refusals above are not a boundary"
        );
    }

    #[test]
    fn only_a_connected_source_ingests() {
        let p = producer();
        let event = reading(HALL_PIR, "motion", 1.0, 10);

        let mut refused = 0usize;
        for status in SourceStatus::ALL {
            let src = source_in(SourceKind::Sensor, HALL_PIR, status);
            let answer = p.raw_item_for(&src, &event);
            if status == SourceStatus::Connected {
                assert!(
                    answer.is_ok(),
                    "a connected source refused its own device's event: {answer:?}"
                );
                continue;
            }
            refused += 1;
            assert_eq!(
                answer,
                Err(NotIngested::SourceNotConnected {
                    status: status.as_str()
                }),
                "a {} source was not refused, or was refused for the wrong reason",
                status.as_str()
            );
        }
        assert_eq!(
            refused,
            SourceStatus::ALL.len() - 1,
            "exactly one status may ingest; if a second one is now acceptable this sweep is no \
             longer testing what it claims"
        );

        // A corrupt stored status parses as `Error`, so it refuses too.
        assert_eq!(SourceStatus::parse("who-knows"), SourceStatus::Error);
        assert_eq!(
            p.raw_item_for(
                &source_in(
                    SourceKind::Sensor,
                    HALL_PIR,
                    SourceStatus::parse("who-knows")
                ),
                &event
            ),
            Err(NotIngested::SourceNotConnected { status: "error" })
        );
    }

    // ── 3. Idempotency ──────────────────────────────────────────────────────

    /// A different event must get a different row, or a constant id would pass.
    #[test]
    fn re_delivering_the_same_event_addresses_the_same_row() {
        let p = producer();
        let sensor_src = source(SourceKind::Sensor, HALL_PIR);
        let camera_src = source(SourceKind::Camera, DOOR_CAM);

        for (src, first, second) in [
            (
                &sensor_src,
                reading(HALL_PIR, "motion", 1.0, 10),
                reading(HALL_PIR, "motion", 1.0, 10),
            ),
            (
                &camera_src,
                camera(DOOR_CAM, "person", Some(0.91), 10),
                camera(DOOR_CAM, "person", Some(0.91), 10),
            ),
        ] {
            let a = p.raw_item_for(src, &first).expect("produced");
            let b = p.raw_item_for(src, &second).expect("produced");
            assert_eq!(
                a.external_id, b.external_id,
                "re-delivering the same event minted a second external id, so a bus replay would \
                 fill the corpus with duplicates of one reading"
            );
        }

        // Each of the key's three parts changes the id on its own.
        let base = p
            .raw_item_for(&sensor_src, &reading(HALL_PIR, "motion", 1.0, 10))
            .expect("produced")
            .external_id;
        let later = p
            .raw_item_for(&sensor_src, &reading(HALL_PIR, "motion", 1.0, 11))
            .expect("produced")
            .external_id;
        assert_ne!(
            base, later,
            "two readings a second apart share a row, so the second destroys the first"
        );
        let other_signal = p
            .raw_item_for(&sensor_src, &reading(HALL_PIR, "contact", 1.0, 10))
            .expect("produced")
            .external_id;
        assert_ne!(
            base, other_signal,
            "two signals from one device share a row"
        );

        let other_device_src = source(SourceKind::Sensor, "porch-pir");
        let other_device = p
            .raw_item_for(&other_device_src, &reading("porch-pir", "motion", 1.0, 10))
            .expect("produced")
            .external_id;
        assert_ne!(base, other_device, "two devices share a row");
    }

    /// Repetition passes even under `Utc::now()`; the key must hold the event's own instant.
    #[test]
    fn the_external_id_is_a_function_of_the_event_and_nothing_else() {
        let p = producer();
        let src = source(SourceKind::Sensor, HALL_PIR);
        let event = reading(HALL_PIR, "occupancy", 1.0, 42);

        let item = p.raw_item_for(&src, &event).expect("produced");
        assert!(
            item.external_id.contains(&instant(at(42))),
            "the key `{}` does not contain the reading's own instant `{}`, so it was derived from \
             something other than the event -- a clock, a counter or a random. A bus replay would \
             then mint a new row for a reading already stored",
            item.external_id,
            instant(at(42))
        );
        // Vacuity control: different moments render differently.
        assert_ne!(instant(at(42)), instant(at(43)));

        let ids: std::collections::BTreeSet<String> = (0..25)
            .map(|_| p.raw_item_for(&src, &event).expect("produced").external_id)
            .collect();
        assert_eq!(
            ids.len(),
            1,
            "the same event produced {} different external ids, so the key depends on something \
             outside the event: {ids:?}",
            ids.len()
        );
        // The whole item too: a clock-built body would rewrite the row on every replay.
        let items: std::collections::BTreeSet<String> = (0..5)
            .map(|_| {
                let item = p.raw_item_for(&src, &event).expect("produced");
                format!("{}|{}|{}", item.title, item.body, item.occurred_at)
            })
            .collect();
        assert_eq!(
            items.len(),
            1,
            "the item's content is not stable: {items:?}"
        );

        // The camera key is a separate derivation and needs the same claim.
        let cam_src = source(SourceKind::Camera, DOOR_CAM);
        let cam = p
            .raw_item_for(&cam_src, &camera(DOOR_CAM, "person", Some(0.9), 42))
            .expect("produced");
        assert!(
            cam.external_id.contains(&instant(at(42))),
            "the camera key `{}` does not contain the event's own instant",
            cam.external_id
        );
    }

    // ── 4. Worth keeping, in both directions ────────────────────────────────

    #[test]
    fn the_sensor_rule_keeps_transitions_and_drops_samples() {
        let p = producer();
        let src = source(SourceKind::Sensor, HALL_PIR);

        // Kept at both polarities: Matter's `contact` is `true = closed`.
        for signal in ["motion", "occupancy", "contact", "door"] {
            for value in [0.0, 1.0] {
                assert!(
                    p.raw_item_for(&src, &reading(HALL_PIR, signal, value, 10))
                        .is_ok(),
                    "a {signal} reading of {value} was dropped; polarity is device-specific and \
                     this rule must not consult the value"
                );
            }
        }

        // Dropped: measurements, already held in `sensor_readings`.
        for signal in ["temperature", "humidity", "co2", "pressure", "battery"] {
            let err = p
                .raw_item_for(&src, &reading(HALL_PIR, signal, 21.4, 10))
                .expect_err("a measurement must be dropped");
            assert_eq!(
                err,
                NotIngested::NotWorthKeeping(DropReason::ContinuousSample {
                    signal: signal.to_string()
                }),
                "a {signal} reading was dropped for the wrong reason"
            );
        }

        // An unrecognised signal drops: the narrowing default.
        assert!(
            p.raw_item_for(&src, &reading(HALL_PIR, "flux-capacitance", 1.0, 10))
                .is_err(),
            "an unrecognised signal was kept, so a new sensor type defaults to being ingested"
        );

        // Case and padding must not decide it.
        assert!(p
            .raw_item_for(&src, &reading(HALL_PIR, " Motion ", 1.0, 10))
            .is_ok());
    }

    #[test]
    fn the_camera_rule_keeps_classifications_and_drops_bare_motion() {
        let p = producer();
        let src = source(SourceKind::Camera, DOOR_CAM);

        for label in ["person", "vehicle", "package", "pet"] {
            assert!(
                p.raw_item_for(&src, &camera(DOOR_CAM, label, Some(0.91), 10))
                    .is_ok(),
                "a classified `{label}` event was dropped, so a pond with a classifier ingests \
                 nothing either"
            );
        }
        // A missing confidence is kept: the manual POST route may omit it.
        assert!(p
            .raw_item_for(&src, &camera(DOOR_CAM, "person", None, 10))
            .is_ok());

        for (label, confidence) in [("motion", Some(0.99)), ("MOTION", None), ("  ", None)] {
            let err = p
                .raw_item_for(&src, &camera(DOOR_CAM, label, confidence, 10))
                .expect_err("an unclassified camera event must be dropped");
            assert!(
                matches!(
                    err,
                    NotIngested::NotWorthKeeping(DropReason::NothingNamedIt { .. })
                ),
                "`{label}` was dropped for the wrong reason: {err}"
            );
        }

        // Low confidence is its own refusal, distinct from "nothing named it".
        let err = p
            .raw_item_for(&src, &camera(DOOR_CAM, "person", Some(0.2), 10))
            .expect_err("a low-confidence classification must be dropped");
        assert_eq!(
            err,
            NotIngested::NotWorthKeeping(DropReason::LowConfidence { confidence: 0.2 })
        );
        // The floor is a floor, not a ceiling: exactly at it, the event is kept.
        assert!(p
            .raw_item_for(
                &src,
                &camera(DOOR_CAM, "person", Some(MIN_CAMERA_CONFIDENCE), 10)
            )
            .is_ok());
    }

    #[test]
    fn a_confidence_that_is_not_a_number_is_refused_rather_than_kept() {
        let p = producer();
        let src = source(SourceKind::Camera, DOOR_CAM);
        let err = p
            .raw_item_for(&src, &camera(DOOR_CAM, "person", Some(f64::NAN), 10))
            .expect_err("a NaN confidence must not be treated as clearing the floor");
        assert!(
            matches!(
                err,
                NotIngested::NotWorthKeeping(DropReason::LowConfidence { .. })
            ),
            "a NaN confidence was refused for the wrong reason: {err}"
        );

        // Infinities still compare: +inf is kept, -inf dropped.
        assert!(p
            .raw_item_for(&src, &camera(DOOR_CAM, "person", Some(f64::INFINITY), 10))
            .is_ok());
        assert!(p
            .raw_item_for(
                &src,
                &camera(DOOR_CAM, "person", Some(f64::NEG_INFINITY), 10)
            )
            .is_err());
    }

    /// Vacuity control for both worth-keeping rules.
    #[test]
    fn the_worth_keeping_rules_are_not_constant_functions() {
        assert!(!DISCRETE_SENSOR_TYPES.is_empty());
        assert!(!UNCLASSIFIED_CAMERA_EVENT_TYPES.is_empty());
        assert!(
            (0.0..=1.0).contains(&MIN_CAMERA_CONFIDENCE),
            "a confidence floor outside [0,1] either keeps everything or drops everything"
        );
        assert!(!DISCRETE_SENSOR_TYPES.contains(&"temperature"));
        assert!(DISCRETE_SENSOR_TYPES.contains(&"motion"));
    }

    // ── The toggle ──────────────────────────────────────────────────────────

    /// The same event is produced once the toggle is on, so this is about the toggle.
    #[test]
    fn the_producer_is_off_on_a_default_pond() {
        let event = reading(HALL_PIR, "motion", 1.0, 10);
        let src = source(SourceKind::Sensor, HALL_PIR);

        let off = BusProducer::from_settings(&Settings::default());
        assert!(!off.is_enabled());
        assert_eq!(off.raw_item_for(&src, &event), Err(NotIngested::Disabled));
        assert!(
            off.items_for(std::slice::from_ref(&src), &event).is_empty(),
            "the batch entry point ignored the toggle the single one honours"
        );

        assert!(producer().raw_item_for(&src, &event).is_ok());
        assert_eq!(
            producer()
                .items_for(std::slice::from_ref(&src), &event)
                .len(),
            1
        );
    }

    // ── Addressing ──────────────────────────────────────────────────────────

    #[test]
    fn an_event_from_another_device_belongs_to_no_source() {
        let p = producer();
        let src = source(SourceKind::Sensor, HALL_PIR);
        assert_eq!(
            p.raw_item_for(&src, &reading("porch-pir", "motion", 1.0, 10)),
            Err(NotIngested::DifferentDevice {
                follows: HALL_PIR.to_string(),
                came_from: "porch-pir".to_string(),
            })
        );
    }

    #[test]
    fn a_camera_event_cannot_feed_a_sensor_source() {
        let p = producer();
        // Same provider string on purpose, so only the family differs.
        let src = source(SourceKind::Sensor, DOOR_CAM);
        assert_eq!(
            p.raw_item_for(&src, &camera(DOOR_CAM, "person", Some(0.9), 10)),
            Err(NotIngested::WrongFamilyForSource {
                source_kind: "sensor",
                event: "camera",
            })
        );
        let src = source(SourceKind::Camera, HALL_PIR);
        assert_eq!(
            p.raw_item_for(&src, &reading(HALL_PIR, "motion", 1.0, 10)),
            Err(NotIngested::WrongFamilyForSource {
                source_kind: "camera",
                event: "sensor",
            })
        );
    }

    /// `profile_id` isn't an `Option`, so a shared device is two sources.
    #[test]
    fn two_members_following_one_camera_each_get_an_item() {
        let p = producer();
        let following = |id: &str, owner: &str| {
            ContextSource::from_parts(SourceParts {
                id: id.into(),
                kind: SourceKind::Camera,
                provider: DOOR_CAM.into(),
                profile_id: owner.into(),
                scopes: vec![],
                cursor: None,
                last_sync: None,
                status: SourceStatus::Connected,
                secret_ref: None,
                created_at: at(0),
            })
            .expect("valid source")
        };
        let jerry = following("src-jerry-cam", "jerry");
        let liz = following("src-liz-cam", "liz");

        let unrelated = source(SourceKind::Camera, "garage-cam");
        let sources = vec![jerry, liz, unrelated];
        let produced = p.items_for(&sources, &camera(DOOR_CAM, "person", Some(0.9), 10));

        assert_eq!(
            produced.len(),
            2,
            "one camera event reached {} of three sources; two follow it and one does not",
            produced.len()
        );
        let owners: Vec<&str> = produced.iter().map(|(s, _)| s.profile_id()).collect();
        assert_eq!(owners, vec!["jerry", "liz"]);
        assert_eq!(
            produced[0].1.external_id, produced[1].1.external_id,
            "the external id is the EVENT's identity; the owner is the source's, and the stored \
             id combines both"
        );
    }

    // ── The item itself ─────────────────────────────────────────────────────

    /// `snapshot_path` especially must stay out: it goes wrong the moment the file is pruned.
    #[test]
    fn a_camera_item_carries_the_event_and_not_the_adapters_payload() {
        let p = producer();
        let src = source(SourceKind::Camera, DOOR_CAM);
        let item = p
            .raw_item_for(&src, &camera(DOOR_CAM, "package", Some(0.83), 10))
            .expect("produced");

        assert_eq!(item.kind, ItemKind::Event);
        assert_eq!(item.occurred_at, at(10));
        assert!(item.title.contains("package") && item.title.contains(DOOR_CAM));
        assert!(item.body.contains("package") && item.body.contains("0.83"));
        assert!(
            !item.body.contains("snap.jpg"),
            "a filesystem path reached the corpus: {}",
            item.body
        );
        assert!(
            !item.body.contains("changed_fraction"),
            "adapter metadata reached the corpus: {}",
            item.body
        );
        assert!(
            item.participants.is_empty(),
            "a camera event named a participant; nothing here identifies a person"
        );
    }

    #[test]
    fn a_sensor_item_reads_as_a_sentence_and_keeps_its_unit() {
        let p = producer();
        let src = source(SourceKind::Sensor, HALL_PIR);
        let item = p
            .raw_item_for(&src, &reading(HALL_PIR, "occupancy", 1.0, 10))
            .expect("produced");
        assert_eq!(item.kind, ItemKind::Event);
        assert_eq!(item.occurred_at, at(10));
        assert_eq!(item.body, format!("{HALL_PIR} reported occupancy = 1 bool"));
        assert!(item.participants.is_empty());

        // A blank unit must not leave a trailing space in the prose.
        let blank_unit = BusEvent::Sensor(SensorReading {
            device_id: HALL_PIR.into(),
            sensor_type: "motion".into(),
            value: 1.0,
            unit: "  ".into(),
            recorded_at: at(10),
        });
        let item = p.raw_item_for(&src, &blank_unit).expect("produced");
        assert_eq!(item.body, format!("{HALL_PIR} reported motion = 1"));
    }
}
