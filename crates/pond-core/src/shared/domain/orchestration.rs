//! Subagent policy: what a child agent may do, under whose authority; the adapter runs it.
//!
//! A child never exceeds its parent's profile scope or tools, and depth is capped; types enforce
//! both ([`ChildScope`], [`DelegationDepth`]) rather than deletable runtime checks.
//!
//! Network mode is not narrowed here: children share it only because it is process-global
//! ([`crate::shared::services::egress`]); an out-of-process child needs a [`TaskSpec`] field.

use crate::mcp::domain::tool_group::{
    group_of_tool, groups_denied_to_guests, groups_denied_to_subagents,
};
#[cfg(test)]
use crate::mcp::domain::tool_group::{ORCHESTRATOR_EXTENSION, TOOLKIT_EXTENSION};
use crate::models::services::context::model_class::{provider_locality, ProviderLocality};
use crate::user_data::domain::profile::ProfileScope;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

/// Depth cap: root is 0, so 1 bars subagents from delegating; each level multiplies GPU load.
pub const MAX_DELEGATION_DEPTH: u8 = 1;

/// Default role turn budget; Goose's 25 would block the parent turn for minutes on an Orin.
pub const DEFAULT_ROLE_MAX_TURNS: u32 = 6;

/// Largest turn budget a stored role may ask for; above it the role is refused, not clamped.
pub const MAX_ROLE_MAX_TURNS: u32 = 12;

/// Share of the parent's history budget reserved while its child runs, for a role stating none.
pub const DEFAULT_CONTEXT_FRACTION: f32 = 0.5;

/// GIAP's key in a recipe's YAML; Goose's `Recipe` and `pond-api`'s `RecipePrompt` ignore it.
pub const ROLE_YAML_KEY: &str = "giap_role";

/// Concurrent subagents allowed on `provider`; only a known-hosted one gets more than one.
/// Unrecognised names (`mock`, goose's `lmstudio`) may serve from this device's GPU.
pub fn max_concurrent_subagents(provider: &str) -> usize {
    match provider_locality(provider) {
        ProviderLocality::Hosted => REMOTE_SUBAGENT_CONCURRENCY,
        ProviderLocality::OnDevice | ProviderLocality::Unknown => 1,
    }
}

/// Concurrent subagents allowed on a hosted provider; a guess, never measured.
pub const REMOTE_SUBAGENT_CONCURRENCY: usize = 3;

// ── Per-role model ──────────────────────────────────────────────────────────

/// Which model a child runs on; a role's model is honoured only on a hosted provider.
/// Elsewhere the child runs on the resident model, as a second costs a reload plus re-prefill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildModel {
    /// The role named no model. The child runs on whatever the parent runs on.
    Inherited,
    /// The role's model, honoured because the provider is known to be hosted.
    Assigned(String),
    /// The role's model, refused because the provider runs on this device.
    RefusedOnDevice { requested: String, provider: String },
    /// The role's model, refused because the provider is unrecognised; separate so logs say so.
    RefusedUnknownProvider { requested: String, provider: String },
}

impl ChildModel {
    /// Decide from the role's request and the parent's provider.
    pub fn resolve(requested: Option<&str>, provider: &str) -> Self {
        let Some(requested) = requested.map(str::trim).filter(|m| !m.is_empty()) else {
            return ChildModel::Inherited;
        };
        match provider_locality(provider) {
            ProviderLocality::Hosted => ChildModel::Assigned(requested.to_string()),
            ProviderLocality::OnDevice => ChildModel::RefusedOnDevice {
                requested: requested.to_string(),
                provider: provider.to_string(),
            },
            ProviderLocality::Unknown => ChildModel::RefusedUnknownProvider {
                requested: requested.to_string(),
                provider: provider.to_string(),
            },
        }
    }

    /// Model for the child's `ModelConfig`, or `None` to inherit; adapters should use only this.
    pub fn assigned(&self) -> Option<&str> {
        match self {
            ChildModel::Assigned(model) => Some(model),
            ChildModel::Inherited
            | ChildModel::RefusedOnDevice { .. }
            | ChildModel::RefusedUnknownProvider { .. } => None,
        }
    }

    /// Why the role's model was refused, or `None` if it was not.
    pub fn refusal(&self) -> Option<String> {
        match self {
            ChildModel::RefusedOnDevice {
                requested,
                provider,
            } => Some(format!(
                "role asked for model `{requested}` but `{provider}` runs on this device, where a \
                 second model is a load plus a re-prefill the parent's next turn pays for - \
                 running the child on the resident model instead"
            )),
            ChildModel::RefusedUnknownProvider {
                requested,
                provider,
            } => Some(format!(
                "role asked for model `{requested}` but this pond does not know where `{provider}` \
                 runs, and a second model on THIS device is a load plus a re-prefill the parent's \
                 next turn pays for - running the child on the resident model instead. Add \
                 `{provider}` to HOSTED_PROVIDERS if it is served from another machine"
            )),
            ChildModel::Inherited | ChildModel::Assigned(_) => None,
        }
    }
}

// ── Background delegation ───────────────────────────────────────────────────

/// Whether a delegation on `provider` may run in the background.
/// With a single permit, a background child would only hold up the parent's next turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundAvailability {
    Available,
    RefusedOnDevice {
        provider: String,
    },
    /// The provider is unrecognised; refused as on-device, with a different sentence.
    RefusedUnknownProvider {
        provider: String,
    },
}

impl BackgroundAvailability {
    /// Decided by [`max_concurrent_subagents`], so it cannot drift from the semaphore.
    /// Locality only picks the refusal's wording.
    pub fn for_provider(provider: &str) -> Self {
        if max_concurrent_subagents(provider) > 1 {
            return BackgroundAvailability::Available;
        }
        if provider_locality(provider) == ProviderLocality::Unknown {
            return BackgroundAvailability::RefusedUnknownProvider {
                provider: provider.to_string(),
            };
        }
        BackgroundAvailability::RefusedOnDevice {
            provider: provider.to_string(),
        }
    }

    /// Refusal the model reads; it says what to do instead, or a 2-4B model retries verbatim.
    pub fn refusal(&self) -> Option<String> {
        match self {
            BackgroundAvailability::Available => None,
            BackgroundAvailability::RefusedOnDevice { provider } => Some(format!(
                "this pond runs its model on the device itself (`{provider}`), where one agent \
                 can use it at a time - a background helper would simply be holding up your own \
                 next reply. Call delegate again without `background` and wait for the answer, or \
                 do the work yourself"
            )),
            BackgroundAvailability::RefusedUnknownProvider { provider } => Some(format!(
                "this pond cannot tell where `{provider}` runs its model, so it assumes the \
                 device itself, where one agent can use it at a time - a background helper would \
                 simply be holding up your own next reply. Call delegate again without \
                 `background` and wait for the answer, or do the work yourself"
            )),
        }
    }
}

// ── Depth ───────────────────────────────────────────────────────────────────

/// How many delegations deep a turn already is.
/// No `Deserialize` or `Default`: a defaulted 0 would claim to be a root turn, which may spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DelegationDepth(u8);

impl DelegationDepth {
    /// A turn the user started.
    pub const ROOT: Self = DelegationDepth(0);

    pub fn get(self) -> u8 {
        self.0
    }

    pub fn is_root(self) -> bool {
        self.0 == 0
    }

    /// One level down, or refused at the cap; private so only a real parent can mint a depth.
    fn deeper(self) -> Result<Self, DelegationRefused> {
        let next = self.0.saturating_add(1);
        if next > MAX_DELEGATION_DEPTH {
            return Err(DelegationRefused::DepthExceeded {
                requested: next,
                cap: MAX_DELEGATION_DEPTH,
            });
        }
        Ok(DelegationDepth(next))
    }
}

// ── Role ────────────────────────────────────────────────────────────────────

/// What a role may do with the speaker's personal data: inherit the parent's scope or go Guest.
/// Not the boundary: [`ChildScope`] clamps whatever [`narrow`](Self::narrow) returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RolePersonalData {
    /// Run with the parent's scope, unchanged.
    #[default]
    Inherit,
    /// Run with no personal data at all, whatever the parent may reach.
    Deny,
}

impl RolePersonalData {
    /// Every variant, for guards to iterate.
    /// A `#[serde(skip)]` variant evades the check on this list, so it is not a safety boundary.
    pub const ALL: [RolePersonalData; 2] = [RolePersonalData::Inherit, RolePersonalData::Deny];

    /// The scope this role asks for, before [`ChildScope::for_role`] clamps it to the parent.
    /// Add any new variant to [`ALL`](Self::ALL) too, or no guard exercises it.
    fn narrow(self, parent: &ProfileScope) -> ProfileScope {
        match self {
            RolePersonalData::Inherit => parent.clone(),
            RolePersonalData::Deny => ProfileScope::Guest,
        }
    }
}

/// A named, reusable agent persona whose limits the constructor has validated.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRole {
    name: String,
    instructions: String,
    tool_groups: BTreeSet<String>,
    personal_data: RolePersonalData,
    max_turns: u32,
    context_fraction: f32,
    /// The model this role wants, resolved by [`ChildModel`]; `None` means the conversation's.
    model: Option<String>,
}

impl AgentRole {
    /// Build and validate a role.
    pub fn new(
        name: impl Into<String>,
        instructions: impl Into<String>,
        tool_groups: BTreeSet<String>,
        personal_data: RolePersonalData,
        max_turns: u32,
        context_fraction: f32,
    ) -> Result<Self, RoleError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(RoleError::MissingName);
        }
        let instructions = instructions.into();
        if instructions.trim().is_empty() {
            return Err(RoleError::MissingInstructions { role: name });
        }
        if max_turns == 0 || max_turns > MAX_ROLE_MAX_TURNS {
            return Err(RoleError::MaxTurnsOutOfRange {
                role: name,
                requested: max_turns,
                cap: MAX_ROLE_MAX_TURNS,
            });
        }
        if !(context_fraction.is_finite() && context_fraction > 0.0 && context_fraction <= 1.0) {
            return Err(RoleError::ContextFractionOutOfRange {
                role: name,
                requested: context_fraction,
            });
        }
        Ok(Self {
            name,
            instructions,
            tool_groups,
            personal_data,
            max_turns,
            context_fraction,
            model: None,
        })
    }

    /// Attach the model this role asks for; a blank name is refused, not treated as absent.
    pub fn with_model(mut self, model: Option<String>) -> Result<Self, RoleError> {
        match model {
            None => {
                self.model = None;
                Ok(self)
            }
            Some(model) if model.trim().is_empty() => {
                Err(RoleError::EmptyModel { role: self.name })
            }
            Some(model) => {
                self.model = Some(model.trim().to_string());
                Ok(self)
            }
        }
    }

    /// Read a role from a recipe's YAML; `Ok(None)` if it has no [`ROLE_YAML_KEY`] block.
    /// A malformed block is an `Err`, never a default role, which could widen a child's tools.
    pub fn from_recipe_yaml(recipe_name: &str, yaml: &str) -> Result<Option<Self>, RoleError> {
        let doc: RecipeRoleDocument = serde_yaml::from_str(yaml).map_err(|e| RoleError::Yaml {
            role: recipe_name.to_string(),
            message: e.to_string(),
        })?;
        let Some(block) = doc.giap_role else {
            return Ok(None);
        };
        let instructions = block
            .instructions
            .or(doc.instructions)
            .or(doc.prompt)
            .unwrap_or_default();
        Self::new(
            recipe_name,
            instructions,
            block.tool_groups.into_iter().collect(),
            block.personal_data,
            block.max_turns,
            block.context_fraction,
        )?
        .with_model(block.model)
        .map(Some)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// The groups this role wants; [`DelegationAuthority::delegate`] narrows what a child gets.
    pub fn requested_tool_groups(&self) -> &BTreeSet<String> {
        &self.tool_groups
    }

    pub fn personal_data(&self) -> RolePersonalData {
        self.personal_data
    }

    pub fn max_turns(&self) -> u32 {
        self.max_turns
    }

    pub fn context_fraction(&self) -> f32 {
        self.context_fraction
    }

    /// The model this role asks for; resolve it with [`ChildModel::resolve`] before use.
    pub fn requested_model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

/// Top-level recipe shape; not `deny_unknown_fields`, since recipes carry Goose's own fields.
#[derive(Debug, Deserialize)]
struct RecipeRoleDocument {
    #[serde(default)]
    giap_role: Option<RoleBlock>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
}

/// The `giap_role` block; `deny_unknown_fields` since an unknown key in it is a typo.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleBlock {
    /// Required: a default of no tools reads as a bug, and the parent's tools would widen.
    tool_groups: Vec<String>,
    #[serde(default)]
    personal_data: RolePersonalData,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default = "default_role_max_turns")]
    max_turns: u32,
    #[serde(default = "default_role_context_fraction")]
    context_fraction: f32,
    /// Absent means the conversation's model.
    /// No `provider` key on purpose: naming one could send a local pond's data off the machine.
    #[serde(default)]
    model: Option<String>,
}

fn default_role_max_turns() -> u32 {
    DEFAULT_ROLE_MAX_TURNS
}

fn default_role_context_fraction() -> f32 {
    DEFAULT_CONTEXT_FRACTION
}

/// Why a stored role could not be read.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum RoleError {
    #[error("a role must have a name")]
    MissingName,
    #[error("role `{role}` has no instructions, and its recipe has no prompt either")]
    MissingInstructions { role: String },
    #[error("role `{role}` asks for {requested} turns; the cap is {cap}")]
    MaxTurnsOutOfRange {
        role: String,
        requested: u32,
        cap: u32,
    },
    #[error("role `{role}` asks for a context fraction of {requested}; it must be in (0.0, 1.0]")]
    ContextFractionOutOfRange { role: String, requested: f32 },
    #[error(
        "role `{role}` has an empty `model`; leave the key out to use the conversation's model"
    )]
    EmptyModel { role: String },
    #[error("role `{role}` has an unreadable `{ROLE_YAML_KEY}` block: {message}")]
    Yaml { role: String, message: String },
}

// ── The request, the authority, and the spec ────────────────────────────────

/// What the model asks for when it calls `delegate`; it carries no authority fields.
/// `deny_unknown_fields` gives a model that sends one a parse error rather than a silent drop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRequest {
    /// Which stored role to run. Must match an [`AgentRole::name`].
    pub role: String,
    /// What this particular child is being asked to do.
    pub instructions: String,
    /// Opaque structured inputs, passed through to the child's prompt.
    #[serde(default)]
    pub inputs: serde_json::Value,
    /// Run without holding the caller's turn open; grants no authority.
    /// It outlives the turn, so the parent session owns its cancellation (`cancel_children_of`).
    #[serde(default)]
    pub background: bool,
}

/// What a running turn may do, and so the ceiling for anything it delegates to.
#[derive(Debug, Clone)]
pub struct DelegationAuthority {
    session_id: String,
    scope: ProfileScope,
    tool_groups: BTreeSet<String>,
    depth: DelegationDepth,
}

impl DelegationAuthority {
    /// The authority of a turn the user started; `tool_groups` is what it got, not the catalog.
    /// Safe as `pub`: `TurnAuthorityRegistry` only spawns for live turns.
    pub fn root(
        session_id: impl Into<String>,
        scope: ProfileScope,
        tool_groups: BTreeSet<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            scope,
            tool_groups,
            depth: DelegationDepth::ROOT,
        }
    }

    /// The authority of a turn, derived from the exact tool names it was given.
    /// Names with no group prefix (Goose plumbing such as `final_output`) are dropped.
    pub fn for_turn<'a>(
        session_id: impl Into<String>,
        scope: ProfileScope,
        allowed_tools: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let tool_groups: BTreeSet<String> = allowed_tools
            .into_iter()
            .filter_map(|tool| group_of_tool(tool).map(str::to_string))
            .collect();
        Self::root(session_id, scope, tool_groups)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn profile_scope(&self) -> &ProfileScope {
        &self.scope
    }

    pub fn tool_groups(&self) -> &BTreeSet<String> {
        &self.tool_groups
    }

    pub fn depth(&self) -> DelegationDepth {
        self.depth
    }

    /// Whether to offer the `delegate` tool at all; [`delegate`](Self::delegate) enforces.
    pub fn may_delegate(&self) -> bool {
        self.depth.deeper().is_ok()
    }

    /// Derive a child's task from this authority and a role; the request supplies only its words.
    pub fn delegate(
        &self,
        role: &AgentRole,
        request: TaskRequest,
    ) -> Result<TaskSpec, DelegationRefused> {
        if request.role != role.name {
            return Err(DelegationRefused::RoleMismatch {
                requested: request.role,
                supplied: role.name.clone(),
            });
        }
        let instructions = request.instructions.trim().to_string();
        if instructions.is_empty() {
            return Err(DelegationRefused::EmptyInstructions {
                role: role.name.clone(),
            });
        }
        // After the request checks, so a subagent sending nonsense is told that, not the depth cap.
        let depth = self.depth.deeper()?;
        let scope = ChildScope::for_role(role.personal_data, &self.scope);
        // The child's scope, not the parent's: a Deny role's Guest child must lose `giap-memory`.
        let tool_groups = narrow_child_groups(&role.tool_groups, &self.tool_groups, scope.get());
        Ok(TaskSpec {
            id: uuid::Uuid::new_v4().to_string(),
            role: role.name.clone(),
            instructions,
            inputs: request.inputs,
            parent_session_id: self.session_id.clone(),
            scope,
            tool_groups,
            depth,
            max_turns: role.max_turns,
            context_fraction: role.context_fraction,
            // From the role only: a caller could pick the model easiest to jailbreak.
            model: role.model.clone(),
            // From the request: it describes this delegation, not the persona.
            background: request.background,
        })
    }
}

/// A module so `ChildScope`'s field is private even from `delegate` (privacy is per-module).
mod child_scope {
    use super::{ProfileScope, RolePersonalData};

    /// A child's scope: the role's request if the parent contains it, else `Guest`.
    /// Keep the field private to this module despite rustc's hint, or the clamp is skippable.
    #[derive(Debug, Clone)]
    pub(super) struct ChildScope(ProfileScope);

    impl ChildScope {
        /// Only production constructor; taking the role, not a scope, rules out `clamp(c, &c)`.
        pub(super) fn for_role(personal_data: RolePersonalData, parent: &ProfileScope) -> Self {
            Self::clamp(personal_data.narrow(parent), parent)
        }

        /// Pure so tests can sweep every scope pair, including ones no role can produce yet.
        fn clamp(candidate: ProfileScope, parent: &ProfileScope) -> Self {
            if candidate.is_within(parent) {
                Self(candidate)
            } else {
                Self(ProfileScope::Guest)
            }
        }

        /// Test-only: in production a two-argument form invites the identity call `clamp(c, &c)`.
        #[cfg(test)]
        pub(super) fn clamp_unpaired(candidate: ProfileScope, parent: &ProfileScope) -> Self {
            Self::clamp(candidate, parent)
        }

        /// Read the clamped scope; deliberately no `into_inner` or `From`.
        pub(super) fn get(&self) -> &ProfileScope {
            &self.0
        }
    }
}

use self::child_scope::ChildScope;

/// The child's groups: the role's within the parent's, minus [`groups_denied_to_subagents`].
/// Also minus what the CHILD's scope denies, since `giap-memory` tools ignore scope.
fn narrow_child_groups(
    requested: &BTreeSet<String>,
    parent: &BTreeSet<String>,
    child: &ProfileScope,
) -> BTreeSet<String> {
    let mut groups: BTreeSet<String> = requested.intersection(parent).cloned().collect();
    if child.excludes_everything() {
        for denied in groups_denied_to_guests() {
            groups.remove(*denied);
        }
    }
    for denied in groups_denied_to_subagents() {
        groups.remove(*denied);
    }
    groups
}

/// An authorised, not yet started child run; only [`DelegationAuthority::delegate`] makes one.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    id: String,
    role: String,
    instructions: String,
    inputs: serde_json::Value,
    parent_session_id: String,
    /// A [`ChildScope`], not a `ProfileScope`, so only a clamped scope fits.
    scope: ChildScope,
    tool_groups: BTreeSet<String>,
    depth: DelegationDepth,
    max_turns: u32,
    context_fraction: f32,
    /// The role's model, unresolved until [`ChildModel::resolve`] sees the provider.
    model: Option<String>,
    /// From the request; the adapter refuses it on an on-device provider.
    background: bool,
}

impl TaskSpec {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    pub fn inputs(&self) -> &serde_json::Value {
        &self.inputs
    }

    pub fn parent_session_id(&self) -> &str {
        &self.parent_session_id
    }

    pub fn profile_scope(&self) -> &ProfileScope {
        self.scope.get()
    }

    /// The groups the child may use; empty means none. Prefer [`grants_tool`](Self::grants_tool).
    /// Goose's `available_tools` reads an empty list as "all tools", so do not forward it raw.
    pub fn tool_groups(&self) -> &BTreeSet<String> {
        &self.tool_groups
    }

    pub fn depth(&self) -> DelegationDepth {
        self.depth
    }

    pub fn max_turns(&self) -> u32 {
        self.max_turns
    }

    pub fn context_fraction(&self) -> f32 {
        self.context_fraction
    }

    /// The role's model, unresolved; pass it through [`ChildModel::resolve`] before use.
    pub fn requested_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Whether the delegation asked to run in the background; see [`BackgroundAvailability`].
    pub fn background(&self) -> bool {
        self.background
    }

    /// May the child call this fully-qualified tool?
    /// Stricter than `filter_tools_by_groups`: unprefixed and user-added MCP tools are denied.
    pub fn grants_tool(&self, tool_name: &str) -> bool {
        match group_of_tool(tool_name) {
            Some(extension) => self.tool_groups.contains(extension),
            None => false,
        }
    }

    /// The authority the child runs under, once the adapter has created its session.
    /// The only public producer of a non-root [`DelegationAuthority`], so depth cannot be forged.
    pub fn child_authority(&self, child_session_id: impl Into<String>) -> DelegationAuthority {
        DelegationAuthority {
            session_id: child_session_id.into(),
            // Unwrapped: this is the parent scope for the next `delegate`, which clamps afresh.
            scope: self.scope.get().clone(),
            tool_groups: self.tool_groups.clone(),
            depth: self.depth,
        }
    }
}

/// Why a delegation was refused before it ever started.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum DelegationRefused {
    #[error("delegation depth {requested} exceeds the cap of {cap}")]
    DepthExceeded { requested: u8, cap: u8 },
    #[error("delegation asked for role `{requested}` but was given role `{supplied}`")]
    RoleMismatch { requested: String, supplied: String },
    #[error("delegation to role `{role}` carried no instructions")]
    EmptyInstructions { role: String },
    #[error("no role named `{0}` exists")]
    UnknownRole(String),
}

// ── The run ─────────────────────────────────────────────────────────────────

/// Where a child agent run got to.
/// Goose's `run_subagent_task` is `Ok` on cancel and at `max_turns`; the adapter tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Authorised, waiting for a concurrency permit.
    Queued,
    Running,
    /// Finished on its own terms with an answer.
    Completed,
    /// Stopped by the parent, or by the parent itself being cancelled.
    Cancelled,
    /// Ran out of turns; any text returned is Goose's budget message, not an answer.
    TurnBudgetExhausted,
    Failed,
}

impl TaskStatus {
    /// No further transition is possible.
    pub fn is_terminal(self) -> bool {
        !matches!(self, TaskStatus::Queued | TaskStatus::Running)
    }

    /// Whether the run's text may be handed to the parent as a result.
    pub fn produced_an_answer(self) -> bool {
        matches!(self, TaskStatus::Completed)
    }
}

/// A child run as the parent sees it: results only, never the child's transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: String,
    pub role: String,
    pub parent_session_id: String,
    pub status: TaskStatus,
    /// The child's answer; read it via [`result_for_parent`](Self::result_for_parent).
    pub result: Option<String>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl TaskRun {
    /// Start a run from an authorised spec.
    pub fn started(spec: &TaskSpec, at: DateTime<Utc>) -> Self {
        Self {
            id: spec.id().to_string(),
            role: spec.role().to_string(),
            parent_session_id: spec.parent_session_id().to_string(),
            status: TaskStatus::Running,
            result: None,
            error: None,
            started_at: at,
            finished_at: None,
        }
    }

    /// The text the parent may see: `None` unless `Completed`, whatever the engine returned.
    pub fn result_for_parent(&self) -> Option<&str> {
        if self.status.produced_an_answer() {
            self.result.as_deref()
        } else {
            None
        }
    }
}

/// The groups no subagent may hold, each with the control a subagent would bypass.
/// Kept apart from [`groups_denied_to_subagents`] so a removal from that list fails a test.
#[cfg(test)]
const GROUPS_NO_SUBAGENT_MAY_HOLD: [(&str, &str); 5] = [
    (
        TOOLKIT_EXTENSION,
        "enable_tool_group WIDENS an allow-set keyed by the process-global current_session_id(), \
         which a child does not own -- so a child holding it could widen its own narrowing, or \
         its parent's",
    ),
    (
        "giap-device-control",
        "actuates the house, and a subagent is forced to GooseMode::Auto with no approval path \
         left once draft is withheld",
    ),
    (
        "giap-system",
        "send_notification reaches the member directly, with no approval path (the file and \
         shell tools this also covered were removed on 2026-09-10)",
    ),
    (
        "giap-schedule",
        "schedules future work that runs with the household's authority long after the \
         delegation that created it has ended",
    ),
    (
        ORCHESTRATOR_EXTENSION,
        "delegate is refused for a child anyway -- may_delegate() is false at depth 1 -- so this \
         one is withheld for the OTHER reason the list exists: a tool a 2-4B model can see but \
         cannot use costs turns off a budget of six discovering that",
    ),
];

#[cfg(test)]
mod depth_tests {
    use super::*;

    #[test]
    fn a_root_turn_may_delegate_and_its_child_may_not() {
        let root = DelegationAuthority::root(
            "s1",
            ProfileScope::Household,
            ["giap-weather".to_string()].into_iter().collect(),
        );
        assert!(root.may_delegate());
        assert!(root.depth().is_root());

        let role = weather_role();
        let spec = root
            .delegate(&role, request("researcher", "look it up"))
            .unwrap();
        assert_eq!(spec.depth().get(), 1);

        let child = spec.child_authority("s1-child");
        assert!(
            !child.may_delegate(),
            "a subagent must not be able to spawn another"
        );
        let refused = child
            .delegate(&role, request("researcher", "and again"))
            .unwrap_err();
        assert_eq!(
            refused,
            DelegationRefused::DepthExceeded {
                requested: 2,
                cap: MAX_DELEGATION_DEPTH
            }
        );
    }

    #[test]
    fn a_child_is_refused_on_depth_even_when_nothing_else_narrowed() {
        let groups: BTreeSet<String> = ["giap-weather".to_string()].into_iter().collect();
        let root = DelegationAuthority::root("s1", ProfileScope::Household, groups.clone());
        let role = weather_role();
        let spec = root.delegate(&role, request("researcher", "go")).unwrap();
        let child = spec.child_authority("s1-child");

        assert_eq!(child.profile_scope(), &ProfileScope::Household);
        assert_eq!(child.tool_groups(), &groups);
        assert!(matches!(
            child.delegate(&role, request("researcher", "go")),
            Err(DelegationRefused::DepthExceeded { .. })
        ));
    }

    #[test]
    fn the_depth_cap_is_one() {
        assert_eq!(MAX_DELEGATION_DEPTH, 1);
    }

    fn weather_role() -> AgentRole {
        AgentRole::new(
            "researcher",
            "Answer the question and stop.",
            ["giap-weather".to_string()].into_iter().collect(),
            RolePersonalData::Inherit,
            3,
            0.4,
        )
        .unwrap()
    }

    fn request(role: &str, instructions: &str) -> TaskRequest {
        TaskRequest {
            role: role.to_string(),
            instructions: instructions.to_string(),
            inputs: serde_json::Value::Null,
            background: false,
        }
    }
}

#[cfg(test)]
mod forged_authority_tests {
    use super::*;
    use crate::mcp::domain::tool_group::TOOL_GROUPS;

    fn request() -> TaskRequest {
        TaskRequest {
            role: "r".to_string(),
            instructions: "go".to_string(),
            inputs: serde_json::Value::Null,
            background: false,
        }
    }

    /// Why [`DelegationAuthority::root`] may stay `pub`: even a forged one is only a ceiling.
    #[test]
    fn the_widest_authority_anyone_could_forge_still_cannot_widen_a_child() {
        let every_group: BTreeSet<String> = TOOL_GROUPS
            .iter()
            .map(|g| g.extension.to_string())
            .collect();
        let forged = DelegationAuthority::root(
            "a-session-id-that-nobody-checked",
            ProfileScope::Household,
            every_group.clone(),
        );
        let role = AgentRole::new(
            "r",
            "go",
            every_group.clone(),
            RolePersonalData::Inherit,
            3,
            0.5,
        )
        .unwrap();
        let spec = forged.delegate(&role, request()).unwrap();

        // Independent of `groups_denied_to_subagents()`, so a removal there cannot shrink this.
        for (denied, mechanism) in GROUPS_NO_SUBAGENT_MAY_HOLD {
            assert!(
                !spec.tool_groups().contains(denied),
                "a forged root authority handed a child `{denied}`, which no subagent may hold: \
                 {mechanism}"
            );
            assert!(!spec.grants_tool(&format!("{denied}__anything")));
        }
        // And the shared list, so a newly added group is covered too.
        for denied in groups_denied_to_subagents() {
            assert!(
                !spec.tool_groups().contains(*denied),
                "a forged root authority handed a child `{denied}`, which the subagent denylist \
                 names"
            );
        }

        assert_eq!(spec.depth().get(), 1);
        let child = spec.child_authority("child");
        assert!(!child.may_delegate());
        assert!(matches!(
            child.delegate(&role, request()),
            Err(DelegationRefused::DepthExceeded { .. })
        ));

        // Vacuity control: the child really was granted something.
        assert!(spec.grants_tool("giap-weather__get_forecast"));
        assert!(!spec.tool_groups().is_empty());
    }
}

#[cfg(test)]
mod scope_inheritance_tests {
    use super::*;

    fn role(personal_data: RolePersonalData) -> AgentRole {
        AgentRole::new(
            "r",
            "do the thing",
            ["giap-weather".to_string()].into_iter().collect(),
            personal_data,
            3,
            0.5,
        )
        .unwrap()
    }

    fn request() -> TaskRequest {
        TaskRequest {
            role: "r".to_string(),
            instructions: "go".to_string(),
            inputs: serde_json::Value::Null,
            background: false,
        }
    }

    /// Every parent shape; not a literal, so a new `ProfileScope` variant is covered.
    fn parents() -> Vec<ProfileScope> {
        ProfileScope::every_shape()
    }

    /// Derives the variant list from serde's `unknown variant` error, generated from the enum.
    #[test]
    fn all_lists_every_variant_the_enum_actually_has() {
        let message = serde_json::from_str::<RolePersonalData>("\"definitely_not_a_variant\"")
            .expect_err("that is not a variant")
            .to_string();
        // serde's message is a run of backticked names, the first being the bad one.
        let from_the_enum: Vec<&str> = message.split('`').skip(1).step_by(2).skip(1).collect();
        assert!(
            !from_the_enum.is_empty(),
            "no variant names could be read out of serde's message {message:?}, so this test \
             can no longer see a new variant at all"
        );
        let listed: Vec<String> = RolePersonalData::ALL
            .iter()
            .map(|v| {
                serde_json::to_string(v)
                    .expect("a unit variant serializes")
                    .trim_matches('"')
                    .to_string()
            })
            .collect();
        for variant in &from_the_enum {
            assert!(
                listed.iter().any(|l| l == variant),
                "RolePersonalData has a variant `{variant}` that RolePersonalData::ALL does not \
                 list. Every scope guard in this module iterates ALL, so an unlisted variant is \
                 one that nothing tests for widening"
            );
        }
        assert_eq!(
            from_the_enum.len(),
            listed.len(),
            "RolePersonalData::ALL and the enum disagree on how many variants exist: \
             {from_the_enum:?} against {listed:?}"
        );
    }

    /// Every scope shape plus a second owner, since two members are incomparable scopes.
    fn candidate_scopes() -> Vec<ProfileScope> {
        let mut scopes = ProfileScope::every_shape();
        scopes.push(ProfileScope::Owner(
            crate::user_data::domain::profile::SECOND_EXEMPLAR_OWNER_ID.to_string(),
        ));
        scopes
    }

    /// Every scope pair, not just today's roles': a `#[serde(skip)]` variant never reaches `ALL`.
    #[test]
    fn no_candidate_scope_survives_a_parent_that_does_not_contain_it() {
        let mut clamped_incomparable_owners = 0;
        let mut passed_through = 0;
        for candidate in candidate_scopes() {
            for parent in candidate_scopes() {
                let clamped = ChildScope::clamp_unpaired(candidate.clone(), &parent);
                let result = clamped.get();
                assert!(
                    result.is_within(&parent),
                    "the clamp of {candidate:?} under {parent:?} gave {result:?}, which the \
                     parent does not contain"
                );
                if candidate.is_within(&parent) {
                    assert_eq!(
                        result, &candidate,
                        "the clamp narrowed {candidate:?} under {parent:?}, which already \
                         contained it -- a role that inherits must inherit"
                    );
                    passed_through += 1;
                } else {
                    assert_eq!(
                        result,
                        &ProfileScope::Guest,
                        "the clamp of {candidate:?} under {parent:?} answered something other \
                         than Guest for a candidate the parent does not contain. On failure, \
                         access narrows"
                    );
                    if let (ProfileScope::Owner(a), ProfileScope::Owner(b)) = (&candidate, &parent)
                    {
                        assert_ne!(a, b, "is_within says one owner is outside itself");
                        clamped_incomparable_owners += 1;
                    }
                }
            }
        }
        // Vacuity controls; the narrowing one counts different-owner pairs specifically.
        assert!(
            clamped_incomparable_owners > 0,
            "the sweep never clamped a pair of DIFFERENT owners, so it did not exercise the \
             input class the clamp exists for. Either `candidate_scopes` stopped producing two \
             distinct owners, or two members stopped being incomparable"
        );
        assert!(
            passed_through > 0,
            "every pair was clamped, so the clamp could be `|_, _| Guest` and pass"
        );
    }

    #[test]
    fn one_members_scope_is_not_reachable_from_anothers() {
        let jerry = ProfileScope::Owner("jerry".into());
        let liz = ProfileScope::Owner("liz".into());
        let clamp = |candidate: ProfileScope, parent: &ProfileScope| {
            ChildScope::clamp_unpaired(candidate, parent).get().clone()
        };
        assert_eq!(clamp(liz.clone(), &jerry), ProfileScope::Guest);
        assert_eq!(clamp(jerry.clone(), &liz), ProfileScope::Guest);
        assert_eq!(clamp(ProfileScope::Household, &jerry), ProfileScope::Guest);
        // And the control: within is left alone, so the clamp is not a blanket.
        assert_eq!(clamp(jerry.clone(), &jerry), jerry);
        assert_eq!(clamp(jerry.clone(), &ProfileScope::Household), jerry);
    }

    /// No behavioural test can see a skipped clamp, so this fails to compile if the field widens.
    #[test]
    fn the_specs_scope_can_only_be_a_value_that_went_through_the_clamp() {
        fn only_compiles_while_the_field_is_clamped(spec: &TaskSpec) -> &ChildScope {
            &spec.scope
        }

        for personal_data in RolePersonalData::ALL {
            for parent_scope in parents() {
                let parent = DelegationAuthority::root(
                    "s1",
                    parent_scope.clone(),
                    ["giap-weather".to_string()].into_iter().collect(),
                );
                let spec = parent.delegate(&role(personal_data), request()).unwrap();
                assert_eq!(
                    only_compiles_while_the_field_is_clamped(&spec).get(),
                    spec.profile_scope(),
                    "TaskSpec's clamped field and its accessor disagree"
                );
            }
        }
    }

    /// Iterates the shared lists, not literals, so a variant added later is covered.
    #[test]
    fn no_role_setting_can_widen_the_parents_scope() {
        for personal_data in RolePersonalData::ALL {
            for parent_scope in parents() {
                let parent = DelegationAuthority::root(
                    "s1",
                    parent_scope.clone(),
                    ["giap-weather".to_string()].into_iter().collect(),
                );
                let spec = parent.delegate(&role(personal_data), request()).unwrap();
                assert!(
                    spec.profile_scope().is_within(&parent_scope),
                    "role setting {personal_data:?} widened {parent_scope:?} to {:?}",
                    spec.profile_scope()
                );
            }
        }
    }

    /// Vacuity control: `is_within` is reflexive, so copying the parent passes the test above.
    #[test]
    fn deny_actually_drops_to_guest_and_inherit_actually_copies() {
        let parent = DelegationAuthority::root(
            "s1",
            ProfileScope::Owner("jerry".into()),
            ["giap-weather".to_string()].into_iter().collect(),
        );
        let inherited = parent
            .delegate(&role(RolePersonalData::Inherit), request())
            .unwrap();
        assert_eq!(
            inherited.profile_scope(),
            &ProfileScope::Owner("jerry".into())
        );

        let denied = parent
            .delegate(&role(RolePersonalData::Deny), request())
            .unwrap();
        assert_eq!(denied.profile_scope(), &ProfileScope::Guest);
    }

    #[test]
    fn a_guest_parent_produces_a_guest_child() {
        let parent = DelegationAuthority::root(
            "s1",
            ProfileScope::Guest,
            ["giap-weather".to_string()].into_iter().collect(),
        );
        for personal_data in RolePersonalData::ALL {
            let spec = parent.delegate(&role(personal_data), request()).unwrap();
            assert_eq!(spec.profile_scope(), &ProfileScope::Guest);
        }
    }

    /// `child_authority` is a second copy of the scope, so a second chance to widen.
    #[test]
    fn a_childs_own_authority_is_the_specs_and_never_wider() {
        // Wider than the role asks, so the spec's set differs from the parent's.
        let parent_groups: BTreeSet<String> = ["giap-weather".to_string(), "giap-news".to_string()]
            .into_iter()
            .collect();
        for personal_data in RolePersonalData::ALL {
            for parent_scope in parents() {
                let parent =
                    DelegationAuthority::root("s1", parent_scope.clone(), parent_groups.clone());
                let spec = parent.delegate(&role(personal_data), request()).unwrap();
                let child = spec.child_authority("s1-child");

                assert_eq!(
                    child.profile_scope(),
                    spec.profile_scope(),
                    "a {personal_data:?} child of a {parent_scope:?} parent runs under a scope \
                     its own spec never granted"
                );
                assert!(
                    child.profile_scope().is_within(&parent_scope),
                    "child_authority widened {parent_scope:?} to {:?}",
                    child.profile_scope()
                );
                assert_eq!(
                    child.tool_groups(),
                    spec.tool_groups(),
                    "a child's authority holds tools its spec did not"
                );
                assert!(!child.tool_groups().contains("giap-news"));
                assert_eq!(child.depth(), spec.depth());
                assert_eq!(child.session_id(), "s1-child");
            }
        }
    }
}

#[cfg(test)]
mod tool_narrowing_tests {
    use super::*;

    fn groups(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn role_wanting(names: &[&str]) -> AgentRole {
        AgentRole::new("r", "go", groups(names), RolePersonalData::Inherit, 3, 0.5).unwrap()
    }

    fn request() -> TaskRequest {
        TaskRequest {
            role: "r".to_string(),
            instructions: "go".to_string(),
            inputs: serde_json::Value::Null,
            background: false,
        }
    }

    fn parent(names: &[&str]) -> DelegationAuthority {
        DelegationAuthority::root("s1", ProfileScope::Household, groups(names))
    }

    #[test]
    fn a_role_cannot_reach_a_group_its_parent_lacks() {
        let spec = parent(&["giap-weather"])
            .delegate(&role_wanting(&["giap-weather", "giap-memory"]), request())
            .unwrap();
        assert_eq!(spec.tool_groups(), &groups(&["giap-weather"]));
        assert!(!spec.grants_tool("giap-memory__recall_memories"));
        assert!(spec.grants_tool("giap-weather__get_forecast"));
    }

    #[test]
    fn a_role_narrower_than_its_parent_keeps_only_what_it_asked_for() {
        let spec = parent(&["giap-weather", "giap-memory", "giap-news"])
            .delegate(&role_wanting(&["giap-weather"]), request())
            .unwrap();
        assert_eq!(spec.tool_groups(), &groups(&["giap-weather"]));
        assert!(!spec.grants_tool("giap-news__top_headlines"));
        assert!(!spec.grants_tool("giap-memory__recall_memories"));
    }

    /// Goose reads an empty `available_tools` as everything; here empty must grant nothing.
    #[test]
    fn an_empty_group_set_grants_nothing() {
        let spec = parent(&["giap-weather"])
            .delegate(&role_wanting(&["giap-news"]), request())
            .unwrap();
        assert!(spec.tool_groups().is_empty());
        for tool in [
            "giap-weather__get_forecast",
            "giap-memory__recall_memories",
            "giap-news__top_headlines",
        ] {
            assert!(!spec.grants_tool(tool), "empty set granted {tool}");
        }
    }

    #[test]
    fn unknown_shapes_are_denied_rather_than_kept() {
        let spec = parent(&["giap-weather"])
            .delegate(&role_wanting(&["giap-weather"]), request())
            .unwrap();
        assert!(!spec.grants_tool("platform__manage_schedule"));
        assert!(!spec.grants_tool("unprefixed"));
        assert!(!spec.grants_tool("some-user-mcp-server__do_it"));
    }

    #[test]
    fn a_child_dropped_to_guest_loses_the_groups_a_guest_is_denied() {
        let spec = parent(&["giap-memory", "giap-weather", "giap-vision"])
            .delegate(
                &AgentRole::new(
                    "r",
                    "go",
                    groups(&["giap-memory", "giap-weather", "giap-sensors"]),
                    RolePersonalData::Deny,
                    3,
                    0.5,
                )
                .unwrap(),
                request(),
            )
            .unwrap();

        assert_eq!(spec.profile_scope(), &ProfileScope::Guest);
        assert_eq!(spec.tool_groups(), &groups(&["giap-weather"]));
        assert!(!spec.grants_tool("giap-memory__recall_memories"));
        assert!(!spec.grants_tool("giap-memory__forget_memory"));
        assert!(!spec.grants_tool("giap-sensors__list_sensors"));
    }

    /// Vacuity control: the guest subtraction must not apply to every child.
    #[test]
    fn an_inheriting_child_of_a_household_parent_keeps_memory() {
        let spec = parent(&["giap-memory", "giap-weather"])
            .delegate(&role_wanting(&["giap-memory", "giap-weather"]), request())
            .unwrap();
        assert_eq!(spec.profile_scope(), &ProfileScope::Household);
        assert!(spec.grants_tool("giap-memory__recall_memories"));
    }

    #[test]
    fn no_subagent_gets_a_group_the_subagent_denylist_names() {
        let denied: Vec<&str> = groups_denied_to_subagents().to_vec();
        let spec = parent(&denied)
            .delegate(&role_wanting(&denied), request())
            .unwrap();
        assert!(
            spec.tool_groups().is_empty(),
            "a role asked for the whole subagent denylist and kept {:?}",
            spec.tool_groups()
        );
        for group in &denied {
            assert!(
                !spec.grants_tool(&format!("{group}__anything")),
                "{group} is on the subagent denylist and reached a child anyway"
            );
        }
    }

    /// Driven by the independent list, so removing an entry from the shared one still fails.
    #[test]
    fn the_named_groups_never_reach_a_child_however_wide_the_parent() {
        for (group, mechanism) in GROUPS_NO_SUBAGENT_MAY_HOLD {
            let spec = parent(&[group, "giap-weather"])
                .delegate(&role_wanting(&[group, "giap-weather"]), request())
                .unwrap();
            assert!(
                !spec.tool_groups().contains(group),
                "a Household parent holding `{group}` handed it to a child. It is withheld \
                 because {mechanism}"
            );
            assert!(
                !spec.grants_tool(&format!("{group}__anything")),
                "a child could call a `{group}` tool. It is withheld because {mechanism}"
            );
            assert!(
                spec.grants_tool("giap-weather__get_forecast"),
                "the fixture produced an empty child, so the `{group}` assertion above proves \
                 nothing"
            );
        }
    }

    /// Over-denial is guarded by `tool_group.rs::a_subagent_keeps_the_read_only_research_groups`.
    #[test]
    fn the_independently_named_groups_are_exactly_the_shared_denylist() {
        let named: BTreeSet<&str> = GROUPS_NO_SUBAGENT_MAY_HOLD
            .iter()
            .map(|(group, _)| *group)
            .collect();
        let shared: BTreeSet<&str> = groups_denied_to_subagents().iter().copied().collect();
        assert_eq!(
            named, shared,
            "the subagent denylist and the mechanisms recorded for it have diverged. If a group \
             was added, record why a subagent may not hold it; if one was removed, say which \
             control now covers it -- withholding IS the control for these, there is no second \
             one"
        );
    }

    /// A subagent runs in `GooseMode::Auto`, so device actuation would have no confirmation step.
    #[test]
    fn a_subagent_can_never_reach_a_device_actuation_tool() {
        for parent_scope in ProfileScope::every_shape() {
            let authority = DelegationAuthority::root(
                "s1",
                parent_scope.clone(),
                groups(&["giap-device-control", "giap-weather"]),
            );
            for personal_data in RolePersonalData::ALL {
                let role = AgentRole::new(
                    "r",
                    "go",
                    groups(&["giap-device-control", "giap-weather"]),
                    personal_data,
                    3,
                    0.5,
                )
                .unwrap();
                let spec = authority.delegate(&role, request()).unwrap();
                assert!(
                    !spec.grants_tool("giap-device-control__set_device_state"),
                    "a {parent_scope:?} parent with {personal_data:?} handed a child the \
                     ability to actuate the house"
                );
                // Vacuity control: the child was granted something.
                assert!(spec.grants_tool("giap-weather__get_forecast"));
            }
        }
    }

    #[test]
    fn a_turn_authority_is_exactly_the_groups_of_the_tools_that_turn_held() {
        let authority = DelegationAuthority::for_turn(
            "s1",
            ProfileScope::Guest,
            [
                "giap-weather__get_forecast",
                "giap-weather__current_conditions",
                "giap-knowledge__lookup",
                // Goose plumbing: belongs to no group and must not become one.
                "final_output",
                "platform__manage_schedule",
            ],
        );
        assert_eq!(
            authority.tool_groups(),
            &groups(&["giap-weather", "giap-knowledge", "platform"])
        );
        assert!(!authority.tool_groups().contains("final_output"));
    }

    /// One tool per catalog group; the guest fixture filters it the way production does.
    fn a_full_turns_tools() -> Vec<String> {
        use crate::mcp::domain::tool_group::{TOOL_GROUPS, TOOL_NAME_SEPARATOR};
        TOOL_GROUPS
            .iter()
            .map(|g| format!("{}{TOOL_NAME_SEPARATOR}a_tool", g.extension))
            .collect()
    }

    /// Filters via `subtract_guest_denied_tools`, the call `chat_stream` makes, not by hand.
    #[test]
    fn a_guest_turns_authority_cannot_contain_what_the_turn_was_denied() {
        let full = a_full_turns_tools();
        let guest_turn_tools =
            crate::mcp::services::tool_selection::subtract_guest_denied_tools(full.iter());
        let authority = DelegationAuthority::for_turn(
            "s1",
            ProfileScope::Guest,
            guest_turn_tools.iter().map(String::as_str),
        );
        for denied in groups_denied_to_guests() {
            assert!(
                !authority.tool_groups().contains(*denied),
                "{denied} was subtracted from the turn and reappeared in its authority"
            );
        }
        let spec = authority
            .delegate(&role_wanting(&["giap-memory", "giap-weather"]), request())
            .unwrap();
        assert!(!spec.grants_tool("giap-memory__recall_memories"));
        assert!(spec.grants_tool("giap-weather__get_forecast"));
    }

    /// Vacuity control: `for_turn` itself must not drop personal-data groups.
    #[test]
    fn the_same_surface_without_the_subtraction_does_carry_memory() {
        let full = a_full_turns_tools();
        let authority = DelegationAuthority::for_turn(
            "s1",
            ProfileScope::Household,
            full.iter().map(String::as_str),
        );
        for denied in groups_denied_to_guests() {
            assert!(
                authority.tool_groups().contains(*denied),
                "{denied} is missing from a FULL turn's authority, so the guest test next door \
                 proves nothing about the subtraction"
            );
        }
        let spec = authority
            .delegate(&role_wanting(&["giap-memory"]), request())
            .unwrap();
        assert!(spec.grants_tool("giap-memory__recall_memories"));
    }

    #[test]
    fn a_mistyped_group_name_grants_nothing_extra() {
        let spec = parent(&["giap-weather", "giap-news"])
            .delegate(&role_wanting(&["giap-wether"]), request())
            .unwrap();
        assert!(spec.tool_groups().is_empty());
        assert!(!spec.grants_tool("giap-weather__get_forecast"));
    }
}

#[cfg(test)]
mod request_shape_tests {
    use super::*;

    #[test]
    fn a_request_carrying_an_authority_field_is_refused() {
        for hostile in [
            r#"{"role":"r","instructions":"go","profile_scope":"household"}"#,
            r#"{"role":"r","instructions":"go","tool_groups":["giap-memory"]}"#,
            r#"{"role":"r","instructions":"go","depth":0}"#,
            r#"{"role":"r","instructions":"go","session_id":"other"}"#,
        ] {
            let parsed: Result<TaskRequest, _> = serde_json::from_str(hostile);
            assert!(
                parsed.is_err(),
                "a delegate call was allowed to carry its own authority: {hostile}"
            );
        }
    }

    /// Vacuity control: the refusals above are about the extra field.
    #[test]
    fn an_ordinary_request_still_parses() {
        let ok: TaskRequest =
            serde_json::from_str(r#"{"role":"r","instructions":"look up the weather"}"#).unwrap();
        assert_eq!(ok.role, "r");
        assert_eq!(ok.inputs, serde_json::Value::Null);
    }

    #[test]
    fn a_request_with_no_instructions_is_refused_at_delegation() {
        let parent = DelegationAuthority::root(
            "s1",
            ProfileScope::Household,
            ["giap-weather".to_string()].into_iter().collect(),
        );
        let role = AgentRole::new(
            "r",
            "go",
            ["giap-weather".to_string()].into_iter().collect(),
            RolePersonalData::Inherit,
            3,
            0.5,
        )
        .unwrap();
        let refused = parent
            .delegate(
                &role,
                TaskRequest {
                    role: "r".into(),
                    instructions: "   ".into(),
                    inputs: serde_json::Value::Null,
                    background: false,
                },
            )
            .unwrap_err();
        assert_eq!(
            refused,
            DelegationRefused::EmptyInstructions { role: "r".into() }
        );
    }

    #[test]
    fn a_request_naming_a_different_role_than_the_one_supplied_is_refused() {
        let parent = DelegationAuthority::root("s1", ProfileScope::Household, BTreeSet::new());
        let role = AgentRole::new(
            "researcher",
            "go",
            BTreeSet::new(),
            RolePersonalData::Inherit,
            3,
            0.5,
        )
        .unwrap();
        let refused = parent
            .delegate(
                &role,
                TaskRequest {
                    role: "housekeeper".into(),
                    instructions: "go".into(),
                    inputs: serde_json::Value::Null,
                    background: false,
                },
            )
            .unwrap_err();
        assert!(matches!(refused, DelegationRefused::RoleMismatch { .. }));
    }
}

#[cfg(test)]
mod role_yaml_tests {
    use super::*;

    const ROLE_YAML: &str = r#"
title: Researcher
description: Looks things up
instructions: Answer the question from the tools you have, then stop.
giap_role:
  tool_groups:
    - giap-knowledge
    - giap-weather
  personal_data: deny
  max_turns: 4
  context_fraction: 0.3
"#;

    #[test]
    fn a_role_block_is_read_out_of_an_ordinary_recipe() {
        let role = AgentRole::from_recipe_yaml("researcher", ROLE_YAML)
            .unwrap()
            .expect("this recipe declares a role");
        assert_eq!(role.name(), "researcher");
        assert_eq!(
            role.requested_tool_groups(),
            &["giap-knowledge".to_string(), "giap-weather".to_string()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        assert_eq!(role.personal_data(), RolePersonalData::Deny);
        assert_eq!(role.max_turns(), 4);
        assert!((role.context_fraction() - 0.3).abs() < f32::EPSILON);
        assert_eq!(
            role.instructions(),
            "Answer the question from the tools you have, then stop."
        );
    }

    #[test]
    fn a_recipe_without_the_block_is_not_a_role() {
        let plain = "title: Morning Brief\nprompt: Give me the weather.\n";
        assert_eq!(
            AgentRole::from_recipe_yaml("morning_brief", plain).unwrap(),
            None
        );
    }

    #[test]
    fn a_malformed_block_refuses_rather_than_defaulting() {
        let cases = [
            // `tool_groups` absent: it is required, with no safe default.
            "giap_role:\n  personal_data: deny\n",
            // A typo on the one field that does the narrowing.
            "giap_role:\n  toolgroups:\n    - giap-weather\n",
            // Wrong type.
            "giap_role:\n  tool_groups: giap-weather\n",
            // Not YAML at all.
            "giap_role:\n  tool_groups: [\n",
        ];
        for yaml in cases {
            let full = format!("instructions: go\n{yaml}");
            let parsed = AgentRole::from_recipe_yaml("r", &full);
            assert!(
                matches!(parsed, Err(RoleError::Yaml { .. })),
                "malformed role block parsed instead of refusing: {yaml:?} gave {parsed:?}"
            );
        }
    }

    /// Pins `deny_unknown_fields`: the cases above all fail on the required `tool_groups` anyway.
    #[test]
    fn a_typo_on_an_optional_field_refuses_rather_than_silently_defaulting() {
        for (typo, field, consequence) in [
            (
                "personaldata: deny",
                "personal_data",
                "the child keeps the parent's entire personal scope",
            ),
            (
                "personal-data: deny",
                "personal_data",
                "the child keeps the parent's entire personal scope",
            ),
            (
                "max_turn: 4",
                "max_turns",
                "the child gets the default turn budget instead of the one written down",
            ),
            (
                "contextfraction: 0.3",
                "context_fraction",
                "the child claims the default share of the parent's window",
            ),
            (
                "instruction: go",
                "instructions",
                "the child runs the recipe's prompt instead of the role's",
            ),
        ] {
            let yaml = format!("instructions: go\ngiap_role:\n  tool_groups: []\n  {typo}\n");
            let parsed = AgentRole::from_recipe_yaml("r", &yaml);
            assert!(
                matches!(parsed, Err(RoleError::Yaml { .. })),
                "`{typo}` was accepted as a role block, so `{field}` is unset and defaults: \
                 {consequence}. Got {parsed:?}"
            );
        }
    }

    /// Vacuity control for the typo test above.
    #[test]
    fn the_correctly_spelled_optional_fields_parse_and_take_effect() {
        let yaml = "instructions: from the recipe\n\
                    giap_role:\n  \
                      tool_groups: []\n  \
                      personal_data: deny\n  \
                      max_turns: 4\n  \
                      context_fraction: 0.3\n  \
                      instructions: from the block\n";
        let role = AgentRole::from_recipe_yaml("r", yaml).unwrap().unwrap();
        assert_eq!(role.personal_data(), RolePersonalData::Deny);
        assert_eq!(role.max_turns(), 4);
        assert!((role.context_fraction() - 0.3).abs() < f32::EPSILON);
        assert_eq!(role.instructions(), "from the block");
    }

    #[test]
    fn the_well_formed_sibling_of_the_malformed_cases_parses() {
        let yaml = "instructions: go\ngiap_role:\n  tool_groups:\n    - giap-weather\n";
        let role = AgentRole::from_recipe_yaml("r", yaml).unwrap().unwrap();
        assert_eq!(
            role.requested_tool_groups(),
            &["giap-weather".to_string()].into_iter().collect()
        );
        // Unstated limits fall to the documented defaults, not to Goose's 25.
        assert_eq!(role.max_turns(), DEFAULT_ROLE_MAX_TURNS);
        assert_eq!(role.personal_data(), RolePersonalData::Inherit);
    }

    #[test]
    fn an_explicitly_empty_group_list_is_allowed_and_stays_empty() {
        let yaml = "instructions: go\ngiap_role:\n  tool_groups: []\n";
        let role = AgentRole::from_recipe_yaml("r", yaml).unwrap().unwrap();
        assert!(role.requested_tool_groups().is_empty());
    }

    #[test]
    fn the_role_turn_budget_and_its_cap_are_the_numbers_the_design_chose() {
        assert_eq!(
            DEFAULT_ROLE_MAX_TURNS, 6,
            "the unstated turn budget moved. Goose's own default is 25, which is minutes of \
             wall clock on an Orin with the parent's turn blocked behind it — if this is now \
             25, say why in the constant's doc"
        );
        assert_eq!(
            MAX_ROLE_MAX_TURNS, 12,
            "the largest budget a stored role may ask for moved. It is a refusal rather than a \
             clamp, so raising it is a decision about how long a subagent may hold the GPU"
        );
        assert!(
            DEFAULT_ROLE_MAX_TURNS <= MAX_ROLE_MAX_TURNS,
            "the default turn budget is above its own cap, so every role that states nothing \
             is refused"
        );
    }

    #[test]
    fn an_unstated_context_fraction_is_half_the_parents_budget() {
        assert!(
            (DEFAULT_CONTEXT_FRACTION - 0.5).abs() < f32::EPSILON,
            "the default context fraction moved to {DEFAULT_CONTEXT_FRACTION}. It is spent, not \
             carried: TaskSpec::context_fraction -> DeviceLedger::reserve -> \
             GooseAdapter::turn_profile -> CompactionProfile::with_history_reserved, so at 1.0 a \
             role that states nothing takes the parent's whole history budget"
        );
        let role =
            AgentRole::from_recipe_yaml("r", "instructions: go\ngiap_role:\n  tool_groups: []\n")
                .unwrap()
                .unwrap();
        assert!(
            (role.context_fraction() - DEFAULT_CONTEXT_FRACTION).abs() < f32::EPSILON,
            "a role that states no context_fraction did not get the documented default"
        );
    }

    #[test]
    fn a_role_asking_for_more_turns_than_the_cap_is_refused() {
        let yaml = format!(
            "instructions: go\ngiap_role:\n  tool_groups: []\n  max_turns: {}\n",
            MAX_ROLE_MAX_TURNS + 1
        );
        assert!(matches!(
            AgentRole::from_recipe_yaml("r", &yaml),
            Err(RoleError::MaxTurnsOutOfRange { .. })
        ));
        assert!(matches!(
            AgentRole::from_recipe_yaml(
                "r",
                "instructions: go\ngiap_role:\n  tool_groups: []\n  max_turns: 0\n"
            ),
            Err(RoleError::MaxTurnsOutOfRange { .. })
        ));
    }

    #[test]
    fn a_context_fraction_outside_the_unit_interval_is_refused() {
        for bad in ["0.0", "1.5", "-0.2"] {
            let yaml = format!(
                "instructions: go\ngiap_role:\n  tool_groups: []\n  context_fraction: {bad}\n"
            );
            assert!(
                matches!(
                    AgentRole::from_recipe_yaml("r", &yaml),
                    Err(RoleError::ContextFractionOutOfRange { .. })
                ),
                "context_fraction {bad} was accepted"
            );
        }
    }

    #[test]
    fn a_role_with_no_instructions_anywhere_is_refused() {
        let yaml = "title: t\ngiap_role:\n  tool_groups: []\n";
        assert!(matches!(
            AgentRole::from_recipe_yaml("r", yaml),
            Err(RoleError::MissingInstructions { .. })
        ));
    }

    #[test]
    fn the_role_block_leaves_the_rest_of_the_recipe_readable() {
        let doc: serde_yaml::Value = serde_yaml::from_str(ROLE_YAML).unwrap();
        assert_eq!(
            doc.get("title").and_then(|v| v.as_str()),
            Some("Researcher")
        );
        assert!(doc.get(ROLE_YAML_KEY).is_some());
        assert!(doc.get("instructions").and_then(|v| v.as_str()).is_some());
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;

    fn run(status: TaskStatus) -> TaskRun {
        TaskRun {
            id: "t1".into(),
            role: "r".into(),
            parent_session_id: "s1".into(),
            status,
            result: Some("here is the answer".into()),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
        }
    }

    #[test]
    fn only_a_completed_run_yields_a_result_to_the_parent() {
        assert_eq!(
            run(TaskStatus::Completed).result_for_parent(),
            Some("here is the answer")
        );
        for status in [
            TaskStatus::Queued,
            TaskStatus::Running,
            TaskStatus::Cancelled,
            TaskStatus::TurnBudgetExhausted,
            TaskStatus::Failed,
        ] {
            assert_eq!(
                run(status).result_for_parent(),
                None,
                "{status:?} was allowed to report a result to the parent"
            );
        }
    }

    #[test]
    fn terminal_states_are_the_four_that_stop() {
        assert!(!TaskStatus::Queued.is_terminal());
        assert!(!TaskStatus::Running.is_terminal());
        for status in [
            TaskStatus::Completed,
            TaskStatus::Cancelled,
            TaskStatus::TurnBudgetExhausted,
            TaskStatus::Failed,
        ] {
            assert!(status.is_terminal(), "{status:?} should be terminal");
        }
    }

    #[test]
    fn a_run_starts_from_its_spec_and_carries_no_transcript() {
        let parent = DelegationAuthority::root(
            "s1",
            ProfileScope::Household,
            ["giap-weather".to_string()].into_iter().collect(),
        );
        let role = AgentRole::new(
            "r",
            "go",
            ["giap-weather".to_string()].into_iter().collect(),
            RolePersonalData::Inherit,
            3,
            0.5,
        )
        .unwrap();
        let spec = parent
            .delegate(
                &role,
                TaskRequest {
                    role: "r".into(),
                    instructions: "go".into(),
                    inputs: serde_json::Value::Null,
                    background: false,
                },
            )
            .unwrap();
        let started = TaskRun::started(&spec, Utc::now());
        assert_eq!(started.id, spec.id());
        assert_eq!(started.parent_session_id, "s1");
        assert_eq!(started.status, TaskStatus::Running);
        assert_eq!(started.result_for_parent(), None);
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use crate::models::services::context::model_class::ON_DEVICE_PROVIDERS;

    #[test]
    fn nothing_that_runs_on_this_device_gets_parallelism() {
        for provider in ON_DEVICE_PROVIDERS {
            assert_eq!(
                max_concurrent_subagents(provider),
                1,
                "{provider} runs on this box and must not run subagents in parallel"
            );
        }
        // HTTP providers, but served from 127.0.0.1 on the same GPU.
        assert_eq!(max_concurrent_subagents("ollama"), 1);
        assert_eq!(max_concurrent_subagents("llamafile"), 1);
    }

    /// Vacuity control: a constant 1 would pass the test above.
    #[test]
    fn a_hosted_provider_is_allowed_more_than_one() {
        assert!(max_concurrent_subagents("anthropic") > 1);
        assert_eq!(
            max_concurrent_subagents("openai"),
            REMOTE_SUBAGENT_CONCURRENCY
        );
    }

    #[test]
    fn the_remote_concurrency_number_is_the_one_that_was_written_down() {
        assert_eq!(
            REMOTE_SUBAGENT_CONCURRENCY, 3,
            "the hosted-provider concurrency moved. Nothing has measured it, so a change here \
             is a guess replacing a guess and belongs in the constant's doc"
        );
    }

    #[test]
    fn the_predicate_is_case_insensitive_like_the_one_it_delegates_to() {
        assert_eq!(max_concurrent_subagents("Ollama"), 1);
        assert_eq!(max_concurrent_subagents("GGUF"), 1);
        // Unknown also yields 1, so check the locality to prove the trim happened.
        assert_eq!(max_concurrent_subagents(" ollama "), 1);
        assert_eq!(provider_locality(" ollama "), ProviderLocality::OnDevice);
    }

    /// Not hypothetical: `mock` ships with GIAP and the goose ones serve from localhost.
    #[test]
    fn a_provider_this_pond_cannot_place_gets_no_parallelism() {
        for provider in [
            "",
            "  ",
            "mock",
            "lmstudio",
            "llama_swap",
            "omlx",
            "pond-spark",
        ] {
            assert_eq!(
                max_concurrent_subagents(provider),
                1,
                "`{provider}` is in neither provider list, so nothing knows whose GPU pays for a \
                 second child - it must take the on-device answer"
            );
        }
    }
}

#[cfg(test)]
mod child_model_tests {
    use super::*;
    use crate::models::services::context::model_class::{HOSTED_PROVIDERS, ON_DEVICE_PROVIDERS};

    #[test]
    fn a_role_model_is_refused_on_every_provider_that_runs_on_this_device() {
        for provider in ON_DEVICE_PROVIDERS {
            let decided = ChildModel::resolve(Some("qwen3-14b"), provider);
            assert_eq!(
                decided,
                ChildModel::RefusedOnDevice {
                    requested: "qwen3-14b".to_string(),
                    provider: provider.to_string(),
                },
                "{provider} runs on this device, so a per-role model is a GGUF load plus a \
                 re-prefill the parent's next turn pays for"
            );
            assert_eq!(
                decided.assigned(),
                None,
                "{provider}: a refused model must not reach an engine call site"
            );
            let refusal = decided.refusal().expect("a refusal says why");
            assert!(
                refusal.contains("qwen3-14b") && refusal.contains(provider),
                "the refusal names neither the model nor the provider: {refusal}"
            );
        }
    }

    /// Vacuity control: refusing everything would pass the test above.
    #[test]
    fn a_role_model_is_honoured_when_the_model_is_somebody_elses_problem() {
        for provider in HOSTED_PROVIDERS {
            let decided = ChildModel::resolve(Some("claude-haiku-4"), provider);
            assert_eq!(
                decided,
                ChildModel::Assigned("claude-haiku-4".to_string()),
                "{provider} runs somewhere else, so the model is a field in a request body"
            );
            assert_eq!(decided.assigned(), Some("claude-haiku-4"));
            assert_eq!(
                decided.refusal(),
                None,
                "{provider}: nothing was refused, so there is nothing to explain"
            );
        }
    }

    #[test]
    fn a_role_that_names_no_model_inherits_the_conversations() {
        for provider in ON_DEVICE_PROVIDERS.iter().chain(HOSTED_PROVIDERS.iter()) {
            assert_eq!(ChildModel::resolve(None, provider), ChildModel::Inherited);
            assert_eq!(
                ChildModel::resolve(Some("   "), provider),
                ChildModel::Inherited,
                "{provider}: whitespace is not a model name"
            );
            assert_eq!(ChildModel::resolve(None, provider).refusal(), None);
        }
    }

    #[test]
    fn the_refusal_survives_the_casing_a_human_would_type() {
        assert!(matches!(
            ChildModel::resolve(Some("m"), "Ollama"),
            ChildModel::RefusedOnDevice { .. }
        ));
        assert!(matches!(
            ChildModel::resolve(Some("m"), "GGUF"),
            ChildModel::RefusedOnDevice { .. }
        ));
        assert!(
            matches!(
                ChildModel::resolve(Some("m"), " ollama "),
                ChildModel::RefusedOnDevice { .. }
            ),
            "a padded provider name escaped the refusal"
        );
    }

    #[test]
    fn a_model_is_not_honoured_for_a_provider_this_pond_cannot_place() {
        for provider in [
            "",
            "  ",
            "mock",
            "lmstudio",
            "llama_swap",
            "omlx",
            "pond-spark",
        ] {
            let decided = ChildModel::resolve(Some("qwen3-14b"), provider);
            assert_eq!(
                decided,
                ChildModel::RefusedUnknownProvider {
                    requested: "qwen3-14b".to_string(),
                    provider: provider.to_string(),
                },
                "`{provider}` is in neither provider list, so nothing knows that a second model \
                 there is somebody else's problem"
            );
            assert_eq!(
                decided.assigned(),
                None,
                "`{provider}`: an unplaceable provider let a role's model reach an engine call \
                 site"
            );
            let refusal = decided.refusal().expect("a refusal says why");
            assert!(
                refusal.contains("qwen3-14b") && refusal.contains("does not know where"),
                "the refusal does not say what actually happened: {refusal}"
            );
        }
    }

    #[test]
    fn a_roles_model_reaches_the_spec_it_produces() {
        let role = AgentRole::new(
            "researcher",
            "look it up",
            ["giap-weather".to_string()].into_iter().collect(),
            RolePersonalData::Inherit,
            3,
            0.5,
        )
        .unwrap()
        .with_model(Some("claude-haiku-4".to_string()))
        .unwrap();
        assert_eq!(role.requested_model(), Some("claude-haiku-4"));

        let spec = DelegationAuthority::root(
            "s",
            ProfileScope::Household,
            ["giap-weather".to_string()].into_iter().collect(),
        )
        .delegate(
            &role,
            TaskRequest {
                role: "researcher".into(),
                instructions: "go".into(),
                inputs: serde_json::Value::Null,
                background: false,
            },
        )
        .unwrap();
        assert_eq!(
            spec.requested_model(),
            Some("claude-haiku-4"),
            "the role's model must survive the narrowing, or P7 is inert"
        );
    }

    #[test]
    fn a_caller_cannot_name_a_model() {
        let refused = serde_json::from_value::<TaskRequest>(serde_json::json!({
            "role": "researcher",
            "instructions": "go",
            "model": "some-cheaper-model"
        }));
        assert!(
            refused.is_err(),
            "a delegate call naming a model was accepted; the model is the ROLE's to state"
        );
    }

    #[test]
    fn an_empty_model_in_a_role_block_refuses_rather_than_meaning_nothing() {
        let err = AgentRole::from_recipe_yaml(
            "researcher",
            "giap_role:\n  tool_groups: [giap-weather]\n  instructions: go\n  model: '  '\n",
        )
        .unwrap_err();
        assert!(
            matches!(err, RoleError::EmptyModel { .. }),
            "expected an EmptyModel refusal, got {err:?}"
        );
    }

    #[test]
    fn a_role_block_can_state_a_model_and_usually_does_not() {
        let with_model = AgentRole::from_recipe_yaml(
            "researcher",
            "giap_role:\n  tool_groups: [giap-weather]\n  instructions: go\n  model: gpt-5-mini\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(with_model.requested_model(), Some("gpt-5-mini"));

        let without = AgentRole::from_recipe_yaml(
            "researcher",
            "giap_role:\n  tool_groups: [giap-weather]\n  instructions: go\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            without.requested_model(),
            None,
            "every role in every pond today states no model, and that must stay the quiet case"
        );
    }
}

#[cfg(test)]
mod background_tests {
    use super::*;
    use crate::models::services::context::model_class::ON_DEVICE_PROVIDERS;

    #[test]
    fn background_is_refused_on_every_provider_that_runs_on_this_device() {
        for provider in ON_DEVICE_PROVIDERS {
            let availability = BackgroundAvailability::for_provider(provider);
            assert_eq!(
                availability,
                BackgroundAvailability::RefusedOnDevice {
                    provider: provider.to_string()
                },
                "{provider} has one model slot, so a background child IS the parent's next turn"
            );
            let refusal = availability.refusal().expect("a refusal says why");
            assert!(
                refusal.contains(provider),
                "the refusal does not name the provider: {refusal}"
            );
            assert!(
                refusal.contains("without `background`"),
                "the refusal does not tell the model what to do instead, so it will retry the \
                 identical call: {refusal}"
            );
        }
    }

    /// Vacuity control: refusing everywhere would pass the test above.
    #[test]
    fn background_is_available_where_the_model_is_somebody_elses() {
        for provider in ["anthropic", "openai", "openrouter"] {
            assert_eq!(
                BackgroundAvailability::for_provider(provider),
                BackgroundAvailability::Available,
                "{provider} runs elsewhere, so a background child costs this pond nothing"
            );
            assert_eq!(
                BackgroundAvailability::for_provider(provider).refusal(),
                None
            );
        }
    }

    #[test]
    fn background_is_refused_for_a_provider_this_pond_cannot_place() {
        for provider in [
            "",
            "  ",
            "mock",
            "lmstudio",
            "llama_swap",
            "omlx",
            "pond-spark",
        ] {
            let availability = BackgroundAvailability::for_provider(provider);
            assert_eq!(
                availability,
                BackgroundAvailability::RefusedUnknownProvider {
                    provider: provider.to_string()
                },
                "`{provider}` is in neither provider list, so a background child may be running \
                 on the one GPU the parent's next turn needs"
            );
            let refusal = availability.refusal().expect("a refusal says why");
            assert!(
                refusal.contains("cannot tell where"),
                "the refusal claims to know something about `{provider}` that nothing does: \
                 {refusal}"
            );
            assert!(
                refusal.contains("without `background`"),
                "the refusal does not tell the model what to do instead: {refusal}"
            );
        }
    }

    #[test]
    fn availability_agrees_with_the_concurrency_limit_it_is_derived_from() {
        for provider in ON_DEVICE_PROVIDERS.iter().chain(
            [
                "anthropic",
                "openai",
                "Ollama",
                "GGUF",
                "",
                "mock",
                "lmstudio",
                "pond-spark",
            ]
            .iter(),
        ) {
            let available =
                BackgroundAvailability::for_provider(provider) == BackgroundAvailability::Available;
            assert_eq!(
                available,
                max_concurrent_subagents(provider) > 1,
                "{provider}: background availability and the concurrency limit disagree"
            );
        }
    }

    #[test]
    fn a_delegation_is_synchronous_unless_it_says_otherwise() {
        let parsed: TaskRequest = serde_json::from_value(serde_json::json!({
            "role": "researcher",
            "instructions": "go"
        }))
        .expect("role and instructions is a complete request");
        assert!(
            !parsed.background,
            "a caller that says nothing must get a synchronous run"
        );
    }

    #[test]
    fn the_background_flag_reaches_the_spec() {
        for asked in [true, false] {
            let role = AgentRole::new(
                "researcher",
                "look it up",
                ["giap-weather".to_string()].into_iter().collect(),
                RolePersonalData::Inherit,
                3,
                0.5,
            )
            .unwrap();
            let spec = DelegationAuthority::root(
                "s",
                ProfileScope::Household,
                ["giap-weather".to_string()].into_iter().collect(),
            )
            .delegate(
                &role,
                TaskRequest {
                    role: "researcher".into(),
                    instructions: "go".into(),
                    inputs: serde_json::Value::Null,
                    background: asked,
                },
            )
            .unwrap();
            assert_eq!(
                spec.background(),
                asked,
                "the request's background flag did not reach the spec"
            );
        }
    }

    #[test]
    fn asking_for_background_widens_nothing() {
        let role = AgentRole::new(
            "researcher",
            "look it up",
            ["giap-weather".to_string(), "giap-memory".to_string()]
                .into_iter()
                .collect(),
            RolePersonalData::Deny,
            3,
            0.5,
        )
        .unwrap();
        let authority = DelegationAuthority::root(
            "s",
            ProfileScope::Household,
            ["giap-weather".to_string(), "giap-memory".to_string()]
                .into_iter()
                .collect(),
        );
        let make = |background: bool| {
            authority
                .delegate(
                    &role,
                    TaskRequest {
                        role: "researcher".into(),
                        instructions: "go".into(),
                        inputs: serde_json::Value::Null,
                        background,
                    },
                )
                .unwrap()
        };
        let sync = make(false);
        let background = make(true);
        assert_eq!(sync.tool_groups(), background.tool_groups());
        assert_eq!(sync.profile_scope(), background.profile_scope());
        assert_eq!(sync.depth(), background.depth());
        assert_eq!(sync.max_turns(), background.max_turns());
        // Vacuity control: the role really narrows, so equality above is about the flag.
        assert_eq!(background.profile_scope(), &ProfileScope::Guest);
        assert!(!background.tool_groups().contains("giap-memory"));
    }
}
