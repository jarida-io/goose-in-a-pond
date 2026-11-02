//! Which agents' prefills the start-up pass compiles. Default agents always; any other when it
//! is new, its definition changed, or it was used (or else compiled) within the retention. An
//! agent idle past it is skipped, and its next run compiles it again, cold once.

use crate::shared::domain::agent_prefill::PrefillRecord;
use crate::shared::domain::orchestration::AgentRole;
use crate::shared::services::long_task_roles::{
    COMPOSER_ROLE, PLANNER_ROLE, VERIFIER_ROLE, WORKER_ROLE,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

pub const DEFAULT_AGENT_PREFILL_RETENTION_DAYS: u32 = 14;

/// A retention of zero would skip every agent compiled a moment ago.
pub const MIN_AGENT_PREFILL_RETENTION_DAYS: u32 = 1;

/// The shipped agents every long task runs on; their prefills are compiled on every start.
pub fn is_default_agent(name: &str) -> bool {
    [PLANNER_ROLE, WORKER_ROLE, VERIFIER_ROLE, COMPOSER_ROLE].contains(&name)
}

/// 64-bit FNV-1a: stable across builds and releases, unlike `DefaultHasher`, since the hash is
/// stored and compared on the next start.
struct Fnv1a(u64);

impl Fnv1a {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Fnv1a(Self::OFFSET)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// What a role's prefill is built from: its name, instructions and tool groups. Fields are
/// separated by bytes no name or group holds, so moving text between fields changes the hash.
pub fn definition_hash(role: &AgentRole) -> String {
    let mut hash = Fnv1a::new();
    hash.write(role.name().as_bytes());
    hash.write(&[0x00]);
    hash.write(role.instructions().as_bytes());
    hash.write(&[0x00]);
    for (i, group) in role.requested_tool_groups().iter().enumerate() {
        if i > 0 {
            hash.write(&[0x1f]);
        }
        hash.write(group.as_bytes());
    }
    hash.hex()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefillDecision {
    Compile,
    Skip { reason: String },
}

/// Whether the start-up pass compiles `role`. With no time on its record at all it compiles:
/// knowing nothing, a warm first run is the better guess.
pub fn prefill_decision(
    role: &AgentRole,
    is_default: bool,
    record: Option<&PrefillRecord>,
    now: DateTime<Utc>,
    retention_days: u32,
) -> PrefillDecision {
    if is_default {
        return PrefillDecision::Compile;
    }
    let Some(record) = record else {
        return PrefillDecision::Compile;
    };
    if record.definition_hash != definition_hash(role) {
        return PrefillDecision::Compile;
    }
    let Some(last) = record.last_used_at.or(record.compiled_at) else {
        return PrefillDecision::Compile;
    };
    let days = retention_days.max(MIN_AGENT_PREFILL_RETENTION_DAYS);
    let idle = now - last;
    if idle <= chrono::Duration::days(i64::from(days)) {
        return PrefillDecision::Compile;
    }
    let what = if record.last_used_at.is_some() {
        "used"
    } else {
        "compiled"
    };
    PrefillDecision::Skip {
        reason: format!(
            "last {what} {} days ago, past the {days}-day retention",
            idle.num_days()
        ),
    }
}

/// Where an agent's prefill stands, as the status route reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefillState {
    Ready,
    Building,
    /// A rebuild was asked for, or the agent was edited: the next compile pass builds it.
    Queued,
    /// Unused past the retention: discarded, and built again on its next run.
    Idle,
    NotBuilt,
    Failed,
}

/// One agent's state from its record and what the compile pass is doing now.
#[allow(clippy::too_many_arguments)]
pub fn prefill_state(
    role: &AgentRole,
    is_default: bool,
    record: Option<&PrefillRecord>,
    building_now: bool,
    queued: bool,
    failed: bool,
    now: DateTime<Utc>,
    retention_days: u32,
) -> PrefillState {
    if building_now {
        return PrefillState::Building;
    }
    if queued {
        return PrefillState::Queued;
    }
    if failed {
        return PrefillState::Failed;
    }
    let Some(record) = record.filter(|r| r.compiled_at.is_some()) else {
        return PrefillState::NotBuilt;
    };
    if record.definition_hash != definition_hash(role) {
        return PrefillState::Queued;
    }
    match prefill_decision(role, is_default, Some(record), now, retention_days) {
        PrefillDecision::Compile => PrefillState::Ready,
        PrefillDecision::Skip { .. } => PrefillState::Idle,
    }
}

/// One start-up pass: what to compile, what to skip and why, and the rows of agents that no
/// longer exist.
#[derive(Debug)]
pub struct PrefillPlan<'a> {
    pub compile: Vec<&'a AgentRole>,
    pub skipped: Vec<(&'a str, String)>,
    pub forget: Vec<String>,
}

pub fn plan_prefills<'a>(
    roles: &'a [AgentRole],
    records: &HashMap<String, PrefillRecord>,
    now: DateTime<Utc>,
    retention_days: u32,
) -> PrefillPlan<'a> {
    let mut plan = PrefillPlan {
        compile: Vec::new(),
        skipped: Vec::new(),
        forget: Vec::new(),
    };
    for role in roles {
        match prefill_decision(
            role,
            is_default_agent(role.name()),
            records.get(role.name()),
            now,
            retention_days,
        ) {
            PrefillDecision::Compile => plan.compile.push(role),
            PrefillDecision::Skip { reason } => plan.skipped.push((role.name(), reason)),
        }
    }
    plan.forget = records
        .keys()
        .filter(|agent| !roles.iter().any(|role| role.name() == agent.as_str()))
        .cloned()
        .collect();
    plan.forget.sort();
    plan
}

#[cfg(test)]
mod tests;
