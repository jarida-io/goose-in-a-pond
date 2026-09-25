//! Scheduled automations (cron, one-shot or event-triggered) and sensor rule types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a scheduled task does when it fires.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskKind {
    /// Send the prompt to the LLM agent and store the response.
    AgentPrompt { prompt: String },
    /// POST to an external webhook URL (backward compat).
    Webhook { webhook_url: String },
    /// Fires on a matching bus event; never cron-registered, the rules engine calls `run_now`.
    SensorTrigger(SensorTriggerSpec),
}

impl TaskKind {
    /// Event-fired kinds, which the scheduler never registers with cron.
    pub fn is_event_triggered(&self) -> bool {
        matches!(self, TaskKind::SensorTrigger(_))
    }
}

/// Display-only `cron` sentinel for a one-shot (like `"@event"` for sensor rules); never parsed.
pub const CRON_ONCE: &str = "@once";

// ── Sensor/event-triggered rules ─────────────────────────────────────────────

/// Which bus-event family a rule listens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerSourceKind {
    Sensor,
    Camera,
    Device,
}

/// What the rule listens to. Unset fields match anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerSource {
    pub kind: TriggerSourceKind,
    /// `device_id` / `camera_id` to match; `None` = any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Sensor type, camera event type or device state key (e.g. `"motion"`); `None` = any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

/// Numeric comparison operator for [`TriggerCondition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareOp {
    Gt,
    Gte,
    Lt,
    Lte,
    Eq,
}

/// When a matching event fires the rule: every set part must hold; empty always matches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TriggerCondition {
    /// Compared against the event's value; only applies together with `value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<CompareOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// Local `"HH:MM"` bounds; wraps midnight if `after` > `before`. Malformed bounds never match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
}

/// One action a fired rule performs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerAction {
    /// Send a prompt to the LLM agent (same path as scheduled prompts).
    AgentPrompt { prompt: String },
    /// Switch a device on/off via the device-control port.
    DevicePower { device_id: String, on: bool },
    /// Push a notification to connected clients.
    Notify { title: String, body: String },
}

/// Event rule: if `source` emits an event matching `condition`, run `actions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensorTriggerSpec {
    pub source: TriggerSource,
    #[serde(default)]
    pub condition: TriggerCondition,
    pub actions: Vec<TriggerAction>,
    /// Minimum seconds between fires (debounce/cooldown). Default 60.
    #[serde(default = "SensorTriggerSpec::default_cooldown_secs")]
    pub cooldown_secs: u64,
}

/// A bus event's trigger fields, built by `BusEvent::trigger_view()`. `#[non_exhaustive]` stops
/// other crates faking a view for non-device events, which would fire every filterless rule.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TriggerEventView<'a> {
    pub kind: TriggerSourceKind,
    pub device_id: &'a str,
    pub signal: &'a str,
    /// Sensor value / camera confidence / numeric device state, if any.
    pub value: Option<f64>,
}

/// Why a sensor rule was refused: each case would otherwise be accepted and silently never work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleRejection {
    /// No actions: the executor bails at fire time on every single fire.
    NoActions,
    /// `after`/`before` not `HH:MM`, so the window fails closed and the rule never matches.
    MalformedTimeBound { field: &'static str, value: String },
    /// Only one of operator/threshold: `matches` ignores it, so the rule fires on every reading.
    HalfCondition,
    /// A `Some("")` filter, which matches no real event; `None` means "any".
    EmptyFilter { field: &'static str },
    /// An action with nothing to do: no device, prompt text or notification text.
    EmptyAction { index: usize, reason: &'static str },
}

impl std::fmt::Display for RuleRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuleRejection::NoActions => write!(
                f,
                "a rule needs at least one action; without one it would fire and do nothing"
            ),
            RuleRejection::MalformedTimeBound { field, value } => write!(
                f,
                "condition.{field} must be \"HH:MM\" (24-hour), got {value:?}; \
                 an unparseable bound makes the rule match nothing at all"
            ),
            RuleRejection::HalfCondition => write!(
                f,
                "condition.op and condition.value must be given together; \
                 one without the other is silently ignored and the rule fires on every reading"
            ),
            RuleRejection::EmptyFilter { field } => write!(
                f,
                "source.{field} is empty, which matches nothing; omit it to match anything"
            ),
            RuleRejection::EmptyAction { index, reason } => {
                write!(f, "actions[{index}]: {reason}")
            }
        }
    }
}

impl std::error::Error for RuleRejection {}

impl SensorTriggerSpec {
    pub fn default_cooldown_secs() -> u64 {
        60
    }

    /// Refuses rules that would never fire or always fail; id and cron are the scheduler's job.
    pub fn validate(&self) -> Result<(), RuleRejection> {
        if self.actions.is_empty() {
            return Err(RuleRejection::NoActions);
        }
        if self.source.device_id.as_deref().is_some_and(str::is_empty) {
            return Err(RuleRejection::EmptyFilter { field: "device_id" });
        }
        if self.source.signal.as_deref().is_some_and(str::is_empty) {
            return Err(RuleRejection::EmptyFilter { field: "signal" });
        }
        if self.condition.op.is_some() != self.condition.value.is_some() {
            return Err(RuleRejection::HalfCondition);
        }
        for (field, bound) in [
            ("after", self.condition.after.as_deref()),
            ("before", self.condition.before.as_deref()),
        ] {
            if let Some(v) = bound {
                if chrono::NaiveTime::parse_from_str(v, "%H:%M").is_err() {
                    return Err(RuleRejection::MalformedTimeBound {
                        field,
                        value: v.to_string(),
                    });
                }
            }
        }
        for (index, action) in self.actions.iter().enumerate() {
            let reason = match action {
                TriggerAction::AgentPrompt { prompt } if prompt.trim().is_empty() => {
                    Some("an agent action needs a prompt")
                }
                TriggerAction::DevicePower { device_id, .. } if device_id.trim().is_empty() => {
                    Some("a device action needs a device_id")
                }
                TriggerAction::Notify { title, body }
                    if title.trim().is_empty() && body.trim().is_empty() =>
                {
                    Some("a notification needs a title or a body")
                }
                _ => None,
            };
            if let Some(reason) = reason {
                return Err(RuleRejection::EmptyAction { index, reason });
            }
        }
        Ok(())
    }

    /// Whether `event` at local wall-clock `local_time` satisfies source and condition.
    pub fn matches(&self, event: &TriggerEventView<'_>, local_time: chrono::NaiveTime) -> bool {
        if self.source.kind != event.kind {
            return false;
        }
        if let Some(d) = &self.source.device_id {
            if d != event.device_id {
                return false;
            }
        }
        if let Some(s) = &self.source.signal {
            if s != event.signal {
                return false;
            }
        }

        if let (Some(op), Some(threshold)) = (self.condition.op, self.condition.value) {
            let Some(v) = event.value else {
                return false;
            };
            let ok = match op {
                CompareOp::Gt => v > threshold,
                CompareOp::Gte => v >= threshold,
                CompareOp::Lt => v < threshold,
                CompareOp::Lte => v <= threshold,
                CompareOp::Eq => (v - threshold).abs() < f64::EPSILON,
            };
            if !ok {
                return false;
            }
        }

        in_time_window(
            self.condition.after.as_deref(),
            self.condition.before.as_deref(),
            local_time,
        )
    }
}

/// Whether `t` is in the half-open `[after, before)` window, wrapping midnight; malformed → false.
fn in_time_window(after: Option<&str>, before: Option<&str>, t: chrono::NaiveTime) -> bool {
    let parse = |s: &str| chrono::NaiveTime::parse_from_str(s, "%H:%M").ok();
    match (after, before) {
        (None, None) => true,
        (Some(a), None) => parse(a).map(|a| t >= a).unwrap_or(false),
        (None, Some(b)) => parse(b).map(|b| t < b).unwrap_or(false),
        (Some(a), Some(b)) => match (parse(a), parse(b)) {
            (Some(a), Some(b)) if a <= b => t >= a && t < b,
            (Some(a), Some(b)) => t >= a || t < b, // wraps midnight
            _ => false,
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub label: String,
    /// 6-field cron (`<sec> <min> <hour> <dom> <month> <dow>`), or `"@event"`/`"@once"` sentinels.
    pub cron: String,
    /// One-shot fire time (cron has no year field). Absolute UTC, not a delay, because
    /// `tokio_cron_scheduler`'s monotonic `Instant` must be re-derived after a restart.
    #[serde(default)]
    pub fire_at: Option<DateTime<Utc>>,
    /// IANA timezone (e.g. `"Africa/Nairobi"`). Cron is evaluated in this zone.
    pub timezone: String,
    pub kind: TaskKind,
    pub paused: bool,
    pub currently_running: bool,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl Schedule {
    /// Fires once, then deletes itself. Not a `TaskKind` variant: what and when are orthogonal.
    pub fn is_one_shot(&self) -> bool {
        self.fire_at.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRun {
    pub id: String,
    pub schedule_id: String,
    pub status: RunStatus,
    /// The agent's response text, or webhook status message.
    pub result: Option<String>,
    /// Error message if `status == Failed`.
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
}

/// Event emitted when a scheduled task completes (for SSE broadcast / desktop notification).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleResultEvent {
    pub schedule_id: String,
    pub schedule_label: String,
    pub run_id: String,
    pub status: RunStatus,
    pub result: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_kind_serde_round_trip_agent() {
        let kind = TaskKind::AgentPrompt {
            prompt: "Good morning briefing".into(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"agent_prompt\""));
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn task_kind_serde_round_trip_webhook() {
        let kind = TaskKind::Webhook {
            webhook_url: "https://example.com/hook".into(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"webhook\""));
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn run_status_serde() {
        let s = RunStatus::Completed;
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"completed\"");
        let back: RunStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, RunStatus::Completed);
    }

    // ── Sensor-trigger rules ──────────────────────────────────────────────

    fn motion_after_sunset_rule() -> SensorTriggerSpec {
        SensorTriggerSpec {
            source: TriggerSource {
                kind: TriggerSourceKind::Sensor,
                device_id: Some("backyard-pir".into()),
                signal: Some("motion".into()),
            },
            condition: TriggerCondition {
                op: Some(CompareOp::Gte),
                value: Some(1.0),
                after: Some("18:30".into()),
                before: Some("06:00".into()),
            },
            actions: vec![
                TriggerAction::DevicePower {
                    device_id: "backyard-lights".into(),
                    on: true,
                },
                TriggerAction::Notify {
                    title: "Motion".into(),
                    body: "Backyard motion after sunset".into(),
                },
            ],
            cooldown_secs: 120,
        }
    }

    fn motion_event<'a>() -> TriggerEventView<'a> {
        TriggerEventView {
            kind: TriggerSourceKind::Sensor,
            device_id: "backyard-pir",
            signal: "motion",
            value: Some(1.0),
        }
    }

    fn at(hhmm: &str) -> chrono::NaiveTime {
        chrono::NaiveTime::parse_from_str(hhmm, "%H:%M").unwrap()
    }

    #[test]
    fn task_kind_serde_round_trip_sensor_trigger() {
        let kind = TaskKind::SensorTrigger(motion_after_sunset_rule());
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"sensor_trigger\""));
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
        assert!(back.is_event_triggered());
        assert!(!TaskKind::Webhook {
            webhook_url: "https://x".into()
        }
        .is_event_triggered());
    }

    #[test]
    fn sensor_trigger_defaults_deserialize() {
        // Minimal JSON: no condition, no cooldown → empty condition + 60s.
        let json = r#"{"type":"sensor_trigger","source":{"kind":"sensor"},"actions":[{"type":"notify","title":"t","body":"b"}]}"#;
        let TaskKind::SensorTrigger(spec) = serde_json::from_str::<TaskKind>(json).unwrap() else {
            panic!("wrong kind");
        };
        assert_eq!(spec.cooldown_secs, 60);
        assert_eq!(spec.condition, TriggerCondition::default());
        // Empty condition + no filters matches any sensor event, any time.
        assert!(spec.matches(&motion_event(), at("12:00")));
    }

    #[test]
    fn matches_motion_after_sunset_inside_wrapped_window() {
        let rule = motion_after_sunset_rule();
        // Evening and small hours are inside the 18:30→06:00 wrap…
        assert!(rule.matches(&motion_event(), at("22:15")));
        assert!(rule.matches(&motion_event(), at("05:59")));
        // …midday is outside.
        assert!(!rule.matches(&motion_event(), at("12:00")));
        assert!(!rule.matches(&motion_event(), at("06:00")));
    }

    #[test]
    fn matches_filters_source_and_value() {
        let rule = motion_after_sunset_rule();
        let evening = at("20:00");

        // Wrong device.
        let mut e = motion_event();
        e.device_id = "front-door";
        assert!(!rule.matches(&e, evening));

        // Wrong signal.
        let mut e = motion_event();
        e.signal = "temperature";
        assert!(!rule.matches(&e, evening));

        // Wrong family (camera event, same names).
        let mut e = motion_event();
        e.kind = TriggerSourceKind::Camera;
        assert!(!rule.matches(&e, evening));

        // Value below threshold, and value missing.
        let mut e = motion_event();
        e.value = Some(0.0);
        assert!(!rule.matches(&e, evening));
        e.value = None;
        assert!(!rule.matches(&e, evening));
    }

    #[test]
    fn compare_ops_evaluate_correctly() {
        let mk = |op, threshold| SensorTriggerSpec {
            source: TriggerSource {
                kind: TriggerSourceKind::Sensor,
                device_id: None,
                signal: None,
            },
            condition: TriggerCondition {
                op: Some(op),
                value: Some(threshold),
                after: None,
                before: None,
            },
            actions: vec![],
            cooldown_secs: 0,
        };
        let ev = |v| TriggerEventView {
            kind: TriggerSourceKind::Sensor,
            device_id: "d",
            signal: "s",
            value: Some(v),
        };
        let noon = at("12:00");
        assert!(mk(CompareOp::Gt, 20.0).matches(&ev(21.0), noon));
        assert!(!mk(CompareOp::Gt, 20.0).matches(&ev(20.0), noon));
        assert!(mk(CompareOp::Gte, 20.0).matches(&ev(20.0), noon));
        assert!(mk(CompareOp::Lt, 20.0).matches(&ev(19.9), noon));
        assert!(mk(CompareOp::Lte, 20.0).matches(&ev(20.0), noon));
        assert!(mk(CompareOp::Eq, 1.0).matches(&ev(1.0), noon));
        assert!(!mk(CompareOp::Eq, 1.0).matches(&ev(0.5), noon));
    }

    // ── Rule validation ───────────────────────────────────────────────────
    // Each test asserts the defect first, then the refusal.

    #[test]
    fn a_valid_rule_validates() {
        // Vacuity control: the fixture the tests below mutate starts out valid.
        assert_eq!(motion_after_sunset_rule().validate(), Ok(()));
    }

    #[test]
    fn a_malformed_time_bound_is_refused_because_it_matches_nothing() {
        let mut spec = motion_after_sunset_rule();
        spec.condition.after = Some("sunset".into());
        assert!(!spec.matches(&motion_event(), at("22:00")));
        assert_eq!(
            spec.validate(),
            Err(RuleRejection::MalformedTimeBound {
                field: "after",
                value: "sunset".into(),
            })
        );
        spec.condition.after = Some("18:30".into());
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn half_a_condition_is_refused_because_it_is_silently_no_condition() {
        let mut spec = motion_after_sunset_rule();
        spec.condition.value = None; // operator with no threshold
        let mut quiet = motion_event();
        quiet.value = Some(0.0);
        assert!(spec.matches(&quiet, at("22:00")));
        assert_eq!(spec.validate(), Err(RuleRejection::HalfCondition));
    }

    #[test]
    fn an_empty_filter_is_refused_because_no_device_is_called_empty_string() {
        let mut spec = motion_after_sunset_rule();
        spec.source.device_id = Some(String::new());
        assert!(!spec.matches(&motion_event(), at("22:00")));
        assert_eq!(
            spec.validate(),
            Err(RuleRejection::EmptyFilter { field: "device_id" })
        );
        // `None` is how "any device" is said, and it stays legal.
        spec.source.device_id = None;
        assert!(spec.validate().is_ok());
        assert!(spec.matches(&motion_event(), at("22:00")));
    }

    #[test]
    fn a_rule_with_no_actions_is_refused() {
        let mut spec = motion_after_sunset_rule();
        spec.actions.clear();
        // The defect is in the executor (a failed run per fire), so only the refusal is checked.
        assert_eq!(spec.validate(), Err(RuleRejection::NoActions));
    }

    #[test]
    fn an_action_that_cannot_act_is_refused_by_index() {
        let mut spec = motion_after_sunset_rule();
        spec.actions.push(TriggerAction::DevicePower {
            device_id: "  ".into(),
            on: true,
        });
        assert_eq!(
            spec.validate(),
            Err(RuleRejection::EmptyAction {
                index: 2,
                reason: "a device action needs a device_id",
            })
        );

        let mut spec = motion_after_sunset_rule();
        spec.actions = vec![TriggerAction::Notify {
            title: " ".into(),
            body: String::new(),
        }];
        assert!(matches!(
            spec.validate(),
            Err(RuleRejection::EmptyAction { index: 0, .. })
        ));
        // A body-only notification is legal.
        let spec = SensorTriggerSpec {
            actions: vec![TriggerAction::Notify {
                title: String::new(),
                body: "Backyard motion".into(),
            }],
            ..motion_after_sunset_rule()
        };
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn rejections_say_what_to_change() {
        // These are the body of a 400 response.
        let text = RuleRejection::MalformedTimeBound {
            field: "before",
            value: "6pm".into(),
        }
        .to_string();
        assert!(text.contains("condition.before"), "{text}");
        assert!(text.contains("HH:MM"), "{text}");
        assert!(RuleRejection::NoActions.to_string().contains("action"));
    }

    #[test]
    fn malformed_time_window_fails_closed() {
        let mut rule = motion_after_sunset_rule();
        rule.condition.after = Some("sunset".into()); // not HH:MM
        assert!(!rule.matches(&motion_event(), at("22:00")));
    }

    #[test]
    fn non_wrapping_window_and_half_open_bounds() {
        // 08:00–17:00 plain window.
        assert!(in_time_window(Some("08:00"), Some("17:00"), at("12:00")));
        assert!(!in_time_window(Some("08:00"), Some("17:00"), at("18:00")));
        // Only `after`.
        assert!(in_time_window(Some("18:30"), None, at("23:00")));
        assert!(!in_time_window(Some("18:30"), None, at("06:00")));
        // Only `before`.
        assert!(in_time_window(None, Some("06:00"), at("05:00")));
        assert!(!in_time_window(None, Some("06:00"), at("07:00")));
    }
}
