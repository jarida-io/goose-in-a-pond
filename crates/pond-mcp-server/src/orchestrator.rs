//! Orchestrator MCP server: `delegate` and `check_task`. A caller is identified only by the
//! engine session id the engine stamps into `_meta`, which the model cannot forge.

use pond_core::mcp::domain::tool_group::ORCHESTRATOR_EXTENSION;
use pond_core::shared::domain::orchestration::{
    AgentRole, DelegationAuthority, RoleError, TaskRequest, TaskRun, TaskSpec, TaskStatus,
};
use pond_core::shared::ports::orchestrator::Orchestrator;
use pond_core::shared::services::turn_authority::TurnAuthorityRegistry;
use pond_core::user_data::ports::recipe::AgentRecipeRepository;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

// ── Parameter struct ────────────────────────────────────────────────────────

/// The wire shape of a `delegate` call.
///
/// **This is a schema carrier, not the parser.** The authority on what a
/// delegation may say is [`TaskRequest`], which is `deny_unknown_fields` and has
/// no scope, tool, depth or session field precisely so that a model trying to
/// widen its own authority gets an error it can read instead of a silently
/// dropped field. [`into_request`] rebuilds the payload — declared fields AND
/// the extras bag — and hands the whole thing to `TaskRequest` to parse, so that
/// property is preserved rather than re-implemented here.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DelegateParams {
    /// The exact name of the saved role to run.
    pub role: Option<String>,
    /// What this particular agent is being asked to do, in plain words.
    pub instructions: Option<String>,
    /// Optional structured inputs, passed through to the agent unchanged.
    pub inputs: Option<serde_json::Value>,
    /// Run without waiting for the answer. Only on a pond whose model runs
    /// somewhere else; on this device it is refused. Default false.
    // Plain `//` so it stays out of the schema. A `Value` schema'd as bool: as `Option<bool>`,
    // `"true"` fails the whole call in rmcp, and a small model retries a protocol error verbatim.
    #[serde(default)]
    #[schemars(with = "Option<bool>")]
    pub background: Option<serde_json::Value>,
    /// Catch-all for unexpected fields.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Names a small model invents for `role` and `instructions`. Only synonyms of those two: an
/// alias for a scope, tool set, depth or session would bypass `deny_unknown_fields`.
const ROLE_ALIASES: [&str; 4] = ["name", "agent", "role_name", "agent_name"];
const INSTRUCTION_ALIASES: [&str; 4] = ["task", "instruction", "prompt", "request"];

/// Rebuild the payload for [`TaskRequest`] to parse. Aliases are MOVED out of the extras, so
/// whatever is left is truly unknown and hits `deny_unknown_fields`.
fn into_request(mut params: DelegateParams) -> Result<TaskRequest, Refusal> {
    let mut obj = serde_json::Map::new();

    let role = params
        .role
        .take()
        .or_else(|| take_alias(&mut params.extra, &ROLE_ALIASES));
    if let Some(role) = role {
        obj.insert("role".into(), serde_json::Value::String(role));
    }
    let instructions = params
        .instructions
        .take()
        .or_else(|| take_alias(&mut params.extra, &INSTRUCTION_ALIASES));
    if let Some(instructions) = instructions {
        obj.insert(
            "instructions".into(),
            serde_json::Value::String(instructions),
        );
    }
    if let Some(inputs) = params.inputs.take() {
        obj.insert("inputs".into(), inputs);
    }
    if let Some(background) = params.background.take() {
        // Refused here, not by `TaskRequest`: serde's type error doesn't name the field.
        let Some(background) = coerce_bool(&background) else {
            return Err(Refusal::Malformed(format!(
                "`background` must be true or false, not `{background}`"
            )));
        };
        obj.insert("background".into(), serde_json::Value::Bool(background));
    }
    // The rest goes in so `TaskRequest` refuses it rather than this dropping it.
    for (k, v) in params.extra {
        obj.insert(k, v);
    }

    serde_json::from_value::<TaskRequest>(serde_json::Value::Object(obj))
        .map_err(|e| Refusal::Malformed(e.to_string()))
}

/// The wire shape of a `check_task` call — PAI-6 P8.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CheckTaskParams {
    /// The id you were given when you started the task.
    pub task_id: Option<String>,
    /// Catch-all, so an invented argument name can be recovered rather than
    /// costing a turn.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Names a small model invents for `task_id`. Harmless: [`authorise_task`] gates every id.
const TASK_ID_ALIASES: [&str; 4] = ["id", "task", "taskId", "task-id"];

/// The task id of a `check_task` call; blank or missing refuses, never defaults to "the last".
pub fn check_task_id(mut params: CheckTaskParams) -> Result<String, Refusal> {
    let id = params
        .task_id
        .take()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .or_else(|| take_alias(&mut params.extra, &TASK_ID_ALIASES));
    id.ok_or_else(|| {
        Refusal::Malformed(
            "check_task needs the `task_id` you were given when the task started".to_string(),
        )
    })
}

/// Recover a small model's spellings of true/false; anything else is `None`, never `false`.
fn coerce_bool(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "y" | "1" => Some(true),
            "false" | "no" | "n" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Remove the first present alias, if it holds a non-blank string.
fn take_alias(extra: &mut HashMap<String, serde_json::Value>, aliases: &[&str]) -> Option<String> {
    for alias in aliases {
        if let Some(value) = extra.get(*alias).and_then(|v| v.as_str()) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                extra.remove(*alias);
                return Some(value);
            }
        }
    }
    None
}

// ── Refusals ────────────────────────────────────────────────────────────────

/// Why a `delegate` or `check_task` call was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// No live turn holds an authority for this caller. `trace` is kept out of the message:
    /// the four inputs landing here must look identical to the model, but not in the log.
    Unauthorised { trace: &'static str },
    /// The caller is an unidentified speaker.
    Guest,
    /// The caller has already spent its one level of delegation.
    DepthExhausted,
    /// The payload did not parse as a [`TaskRequest`].
    Malformed(String),
    /// No recipe by that name.
    UnknownRole(String),
    /// A recipe exists but carries no `giap_role` block — an ordinary routine.
    NotARole(String),
    /// A recipe exists and its role block will not parse.
    UnreadableRole { role: String, message: String },
    /// The repository could not be read at all.
    RoleLookupFailed(String),
    /// The narrowing itself refused (depth, role mismatch, empty instructions).
    Narrowing(String),
    /// The orchestrator refused or failed to start the run.
    SpawnFailed(String),
    /// No task by that id in this conversation. A foreign id and a never-issued one must read
    /// the same, or a caller could probe which ids exist; only `trace` separates them.
    UnknownTask {
        task_id: String,
        trace: &'static str,
    },
    /// The orchestrator could not be asked.
    TaskLookupFailed(String),
}

impl Refusal {
    /// The model-facing text; each ends with what to do next, or a small model retries verbatim.
    pub fn message(&self) -> String {
        match self {
            // ONE string for all four unauthorised inputs. See the type's doc.
            Refusal::Unauthorised { .. } => "Delegation is not available in this conversation. \
                 Do the work yourself and tell the user what you found."
                .to_string(),
            Refusal::Guest => "Delegation is not available until this conversation is identified \
                 as a member of the household. Answer directly instead."
                .to_string(),
            Refusal::DepthExhausted => "You are already running as a delegated agent, and a \
                 delegated agent may not delegate again. Finish the task you were given."
                .to_string(),
            Refusal::Malformed(detail) => format!(
                "That is not a valid delegation: {detail}. Call delegate with `role` (the exact \
                 name of a saved role) and `instructions` (what it should do), and nothing else \
                 besides the optional `inputs` and `background`. You cannot choose the agent's \
                 permissions, tools or identity; they are derived from yours."
            ),
            Refusal::UnknownRole(role) => format!(
                "There is no saved role named `{role}` on this device, so there is nobody to \
                 delegate to. Do the work yourself."
            ),
            Refusal::NotARole(role) => format!(
                "`{role}` is a saved routine, not an agent role -- it has no `giap_role` block, \
                 so it cannot be delegated to. Do the work yourself."
            ),
            Refusal::UnreadableRole { role, message } => format!(
                "The saved role `{role}` cannot be read: {message}. Do not guess at what it was \
                 meant to say -- tell the user the role needs fixing, and do the work yourself."
            ),
            Refusal::RoleLookupFailed(detail) => format!(
                "The saved roles could not be read ({detail}), so this delegation cannot be \
                 checked. Do the work yourself."
            ),
            Refusal::Narrowing(detail) => format!("Delegation refused: {detail}."),
            Refusal::SpawnFailed(detail) => format!(
                "The delegated agent could not be started: {detail}. Do the work yourself and \
                 tell the user."
            ),
            // ONE string for both unknown-task inputs. See the variant's doc.
            Refusal::UnknownTask { .. } => "There is no delegated task by that id in this \
                 conversation. If you started one, use the id you were given; otherwise there is \
                 nothing to check."
                .to_string(),
            Refusal::TaskLookupFailed(detail) => format!(
                "The delegated task could not be checked ({detail}). Tell the user, and do not \
                 guess at what it found."
            ),
        }
    }

    /// A stable label for the log line. Never shown to the model.
    pub fn trace(&self) -> &'static str {
        match self {
            Refusal::Unauthorised { trace } => trace,
            Refusal::Guest => "guest",
            Refusal::DepthExhausted => "depth_exhausted",
            Refusal::Malformed(_) => "malformed_request",
            Refusal::UnknownRole(_) => "unknown_role",
            Refusal::NotARole(_) => "not_a_role",
            Refusal::UnreadableRole { .. } => "unreadable_role",
            Refusal::RoleLookupFailed(_) => "role_lookup_failed",
            Refusal::Narrowing(_) => "narrowing_refused",
            Refusal::SpawnFailed(_) => "spawn_failed",
            Refusal::UnknownTask { trace, .. } => trace,
            Refusal::TaskLookupFailed(_) => "task_lookup_failed",
        }
    }
}

// ── The decision, as pure functions ─────────────────────────────────────────

/// May this caller delegate at all? Runs BEFORE the recipe lookup, so an unauthorised caller
/// cannot make the pond read its database; the guest check backs up `groups_denied_to_guests`.
pub fn authorise(authority: Option<DelegationAuthority>) -> Result<DelegationAuthority, Refusal> {
    let Some(authority) = authority else {
        return Err(Refusal::Unauthorised {
            trace: "no_live_turn",
        });
    };
    if authority.profile_scope().excludes_everything() {
        return Err(Refusal::Guest);
    }
    if !authority.may_delegate() {
        return Err(Refusal::DepthExhausted);
    }
    Ok(authority)
}

/// What reading the named recipe produced. [`Unreadable`](Self::Unreadable) must refuse:
/// answering a broken role block with a default role is the widening path.
#[derive(Debug, Clone, PartialEq)]
pub enum RoleLookup {
    /// No recipe by that name.
    Missing,
    /// A recipe with no `giap_role` block: an ordinary routine.
    NotARole,
    /// A recipe whose `giap_role` block will not parse or will not validate.
    Unreadable(RoleError),
    Found(AgentRole),
}

impl RoleLookup {
    /// Read a role out of a recipe's YAML, or say why not.
    pub fn from_recipe(name: &str, yaml: Option<&str>) -> Self {
        let Some(yaml) = yaml else {
            return RoleLookup::Missing;
        };
        match AgentRole::from_recipe_yaml(name, yaml) {
            Ok(Some(role)) => RoleLookup::Found(role),
            Ok(None) => RoleLookup::NotARole,
            Err(e) => RoleLookup::Unreadable(e),
        }
    }
}

/// Turn an authorised caller, a role lookup and a request into a [`TaskSpec`]; all narrowing
/// is [`DelegationAuthority::delegate`]'s.
pub fn decide(
    authority: &DelegationAuthority,
    lookup: RoleLookup,
    request: TaskRequest,
) -> Result<TaskSpec, Refusal> {
    let role = match lookup {
        RoleLookup::Missing => return Err(Refusal::UnknownRole(request.role)),
        RoleLookup::NotARole => return Err(Refusal::NotARole(request.role)),
        RoleLookup::Unreadable(e) => {
            return Err(Refusal::UnreadableRole {
                role: request.role,
                message: e.to_string(),
            })
        }
        RoleLookup::Found(role) => role,
    };
    authority
        .delegate(&role, request)
        .map_err(|e| Refusal::Narrowing(e.to_string()))
}

/// The run, only if it belongs to the caller's session. `Orchestrator::poll` is keyed by id
/// alone, so without this `check_task` could read another household member's delegation.
pub fn authorise_task(
    run: Option<TaskRun>,
    caller_session_id: &str,
    task_id: &str,
) -> Result<TaskRun, Refusal> {
    let Some(run) = run else {
        return Err(Refusal::UnknownTask {
            task_id: task_id.to_string(),
            trace: "no_such_task",
        });
    };
    if run.parent_session_id != caller_session_id {
        return Err(Refusal::UnknownTask {
            task_id: task_id.to_string(),
            trace: "task_of_another_session",
        });
    }
    Ok(run)
}

/// What the parent is told about a run. Only via [`TaskRun::result_for_parent`]: Goose hands
/// back partial text on cancel and the max-turns sentence on budget exhaustion as "answers".
pub fn describe_run(run: &TaskRun) -> String {
    if let Some(answer) = run.result_for_parent() {
        return format!("The `{}` agent reports:\n\n{answer}", run.role);
    }
    match run.status {
        TaskStatus::Completed => format!(
            "The `{}` agent finished without producing anything to report.",
            run.role
        ),
        TaskStatus::Cancelled => format!(
            "The `{}` agent was stopped before it finished. Nothing it produced can be relied on.",
            run.role
        ),
        TaskStatus::TurnBudgetExhausted => format!(
            "The `{}` agent ran out of the actions it was allowed and did not reach an answer. \
             Do not treat this as a result -- either do the work yourself or ask the user a \
             narrower question.",
            run.role
        ),
        TaskStatus::Failed => format!(
            "The `{}` agent failed: {}.",
            run.role,
            run.error.as_deref().unwrap_or("no reason given")
        ),
        TaskStatus::Queued | TaskStatus::Running => format!(
            "The `{}` agent is still working (task {}). Tell the user it is running.",
            run.role, run.id
        ),
    }
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct OrchestratorMcpServer {
    /// `None` where no agent was built (tests, some entry points): every call then refuses.
    deps: Option<OrchestratorDeps>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl OrchestratorMcpServer {
    /// All tools, without constructing the server; the generated `tool_router()` is private.
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(deps: Option<OrchestratorDeps>) -> Self {
        Self {
            deps,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Hand work to a saved specialist agent and wait for its findings. `role` = exact saved role \
name, `instructions` = what to do. It runs with a narrower, derived tool set -- its scope is \
not yours to choose or query. Delegate multi-step work; do simple things yourself. \
`background: true` only for long work you do not need now: returns a task_id for check_task, \
and is refused when the model runs on this device (one agent at a time).")]
    async fn delegate(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<DelegateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("delegate");
        let text = match self.run_delegation(&ctx.meta, params.0).await {
            Ok(text) => text,
            Err(refusal) => {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "delegation_refused",
                    reason = refusal.trace(),
                    "a delegate call was refused"
                );
                refusal.message()
            }
        };
        // Refusals are tool SUCCESS text: a protocol error makes small models retry verbatim.
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// The whole decision, with the two I/O steps in the middle.
    async fn run_delegation(
        &self,
        meta: &rmcp::model::Meta,
        params: DelegateParams,
    ) -> Result<String, Refusal> {
        let Some(deps) = &self.deps else {
            return Err(Refusal::Unauthorised {
                trace: "deps_not_installed",
            });
        };
        // No `_meta` is the fourth unauthorised input; same variant, so the same message.
        let Some(session) = crate::session_from_meta(meta) else {
            return Err(Refusal::Unauthorised {
                trace: "no_engine_session",
            });
        };
        let authority = authorise(deps.authorities.authority_for_engine_session(&session))?;

        // Only now is the payload parsed and the database touched.
        let request = into_request(params)?;
        let recipe = deps
            .recipes
            .get_by_name(&request.role)
            .await
            .map_err(|e| Refusal::RoleLookupFailed(e.to_string()))?;
        let lookup =
            RoleLookup::from_recipe(&request.role, recipe.as_ref().map(|r| r.yaml.as_str()));

        let spec = decide(&authority, lookup, request)?;
        tracing::info!(
            target: "giap::trace",
            kind = "delegation_authorised",
            task_id = %spec.id(),
            role = %spec.role(),
            parent_session_id = %spec.parent_session_id(),
            groups = ?spec.tool_groups(),
            depth = spec.depth().get(),
            max_turns = spec.max_turns(),
            background = spec.background(),
        );
        let run = deps
            .orchestrator
            .spawn(spec)
            .await
            .map_err(|e| Refusal::SpawnFailed(e.to_string()))?;
        Ok(describe_run(&run))
    }

    #[tool(description = "\
Check a background delegation by `task_id`: what the agent is doing, or its findings if \
finished. Only tasks started in this conversation.")]
    async fn check_task(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<CheckTaskParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("check_task");
        let text = match self.run_check(&ctx.meta, params.0).await {
            Ok(text) => text,
            Err(refusal) => {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "check_task_refused",
                    reason = refusal.trace(),
                    "a check_task call was refused"
                );
                refusal.message()
            }
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// All of `check_task`: authorise the CALLER exactly as `delegate` does, then the TASK.
    async fn run_check(
        &self,
        meta: &rmcp::model::Meta,
        params: CheckTaskParams,
    ) -> Result<String, Refusal> {
        let Some(deps) = &self.deps else {
            return Err(Refusal::Unauthorised {
                trace: "deps_not_installed",
            });
        };
        let Some(session) = crate::session_from_meta(meta) else {
            return Err(Refusal::Unauthorised {
                trace: "no_engine_session",
            });
        };
        let authority = authorise(deps.authorities.authority_for_engine_session(&session))?;

        let task_id = check_task_id(params)?;
        let run = deps
            .orchestrator
            .poll(&task_id)
            .await
            .map_err(|e| Refusal::TaskLookupFailed(e.to_string()))?;
        let run = authorise_task(run, authority.session_id(), &task_id)?;
        Ok(describe_run(&run))
    }
}

#[tool_handler]
impl ServerHandler for OrchestratorMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                ORCHESTRATOR_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Orchestrator MCP server — hand work to a saved specialist agent.\n\n\
                 A delegated agent runs on this device with a narrower set of tools than you \
                 have and no ability to delegate further. You choose only which saved role to \
                 run and what to ask it; everything else is derived. If there is no suitable \
                 saved role, do the work yourself rather than inventing one.",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

/// What `delegate` needs; none of it exists at registration time.
#[derive(Clone)]
pub struct OrchestratorDeps {
    orchestrator: Arc<dyn Orchestrator>,
    authorities: Arc<TurnAuthorityRegistry>,
    recipes: Arc<dyn AgentRecipeRepository + Send + Sync>,
}

impl OrchestratorDeps {
    pub fn new(
        orchestrator: Arc<dyn Orchestrator>,
        authorities: Arc<TurnAuthorityRegistry>,
        recipes: Arc<dyn AgentRecipeRepository + Send + Sync>,
    ) -> Self {
        Self {
            orchestrator,
            authorities,
            recipes,
        }
    }

    /// The orchestrator delegations run through.
    pub fn orchestrator(&self) -> Arc<dyn Orchestrator> {
        self.orchestrator.clone()
    }

    /// The registry the adapter publishes each live turn's authority into.
    pub fn authorities(&self) -> Arc<TurnAuthorityRegistry> {
        self.authorities.clone()
    }
}

static ORCHESTRATOR_DEPS: OnceLock<OrchestratorDeps> = OnceLock::new();

/// Install the deps once, AFTER the agent adapter exists. The registry must be the SAME one
/// the adapter publishes into (`GooseAdapter::turn_authorities()`), or every call refuses.
pub fn init_orchestrator_deps(deps: OrchestratorDeps) {
    let _ = ORCHESTRATOR_DEPS.set(deps);
}

/// The installed deps, for a caller that delegates without a tool call (the proactive
/// reviewer), so it shares the one registry. `None` (mock agent) means no review at all.
pub fn installed_orchestrator_deps() -> Option<OrchestratorDeps> {
    ORCHESTRATOR_DEPS.get().cloned()
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_orchestrator_server(reader: DuplexStream, writer: DuplexStream) {
    // Deps may be absent (installed after registration); the server then refuses every call.
    let server = OrchestratorMcpServer::new(ORCHESTRATOR_DEPS.get().cloned());
    crate::serve_builtin("giap-orchestrator", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::mcp::domain::tool_group::{groups_denied_to_subagents, ORCHESTRATOR_EXTENSION};
    use pond_core::user_data::domain::profile::ProfileScope;
    use std::collections::BTreeSet;

    fn params(json: serde_json::Value) -> DelegateParams {
        serde_json::from_value(json).expect("DelegateParams accepts any object")
    }

    fn groups(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn root(scope: ProfileScope) -> DelegationAuthority {
        DelegationAuthority::root("giap-session", scope, groups(&["giap-weather"]))
    }

    fn researcher_yaml() -> &'static str {
        "title: Researcher\n\
         giap_role:\n  \
           tool_groups: [giap-weather]\n  \
           instructions: Look it up and stop.\n  \
           max_turns: 3\n"
    }

    // ── authorise ──────────────────────────────────────────────────────────

    /// Asserted on the strings, not the variant: the property is what the model can tell apart.
    #[test]
    fn every_unauthorised_input_is_refused_with_the_same_words() {
        let from_registry = authorise(None).unwrap_err();
        let no_meta = Refusal::Unauthorised {
            trace: "no_engine_session",
        };
        let no_deps = Refusal::Unauthorised {
            trace: "deps_not_installed",
        };
        assert_eq!(from_registry.message(), no_meta.message());
        assert_eq!(from_registry.message(), no_deps.message());
        // ... while the log can still tell them apart.
        assert_ne!(from_registry.trace(), no_meta.trace());
        assert_ne!(no_meta.trace(), no_deps.trace());
    }

    #[test]
    fn a_guest_turn_is_refused_even_though_its_authority_is_a_root_one() {
        let guest = root(ProfileScope::Guest);
        // Vacuity control: the guest could otherwise delegate, so the refusal is about SCOPE.
        assert!(guest.may_delegate());
        assert!(!guest.tool_groups().is_empty());
        assert_eq!(authorise(Some(guest)).unwrap_err(), Refusal::Guest);
    }

    #[test]
    fn an_identified_turn_is_authorised() {
        for scope in [ProfileScope::Household, ProfileScope::Owner("jerry".into())] {
            assert!(
                authorise(Some(root(scope.clone()))).is_ok(),
                "{scope:?} must be able to delegate, or the feature does not exist"
            );
        }
    }

    #[test]
    fn a_subagents_authority_is_refused_on_depth() {
        let parent = root(ProfileScope::Household);
        let role = AgentRole::new(
            "researcher",
            "go",
            groups(&["giap-weather"]),
            Default::default(),
            3,
            0.5,
        )
        .unwrap();
        let spec = parent
            .delegate(
                &role,
                TaskRequest {
                    role: "researcher".into(),
                    instructions: "look it up".into(),
                    inputs: serde_json::Value::Null,
                    background: false,
                },
            )
            .unwrap();
        let child = spec.child_authority("child-session");
        assert_eq!(
            authorise(Some(child)).unwrap_err(),
            Refusal::DepthExhausted,
            "a delegated agent must not be able to delegate again"
        );
    }

    /// Withholding and refusal are both needed; neither implies the other.
    #[test]
    fn the_delegation_group_is_withheld_from_subagents_as_well_as_refused() {
        assert!(
            groups_denied_to_subagents().contains(&ORCHESTRATOR_EXTENSION),
            "a role naming giap-orchestrator under a parent that holds it would put a tool in a \
             child's prompt whose every call authorise() refuses"
        );
    }

    // ── into_request ───────────────────────────────────────────────────────

    #[test]
    fn a_well_formed_call_parses() {
        let request = into_request(params(serde_json::json!({
            "role": "researcher",
            "instructions": "find out when the bins go out"
        })))
        .expect("a role and instructions is all a caller supplies");
        assert_eq!(request.role, "researcher");
        assert_eq!(request.instructions, "find out when the bins go out");
        assert_eq!(request.inputs, serde_json::Value::Null);
    }

    #[test]
    fn the_aliases_a_small_model_invents_are_recovered() {
        for role_alias in ROLE_ALIASES {
            for instruction_alias in INSTRUCTION_ALIASES {
                let request = into_request(params(serde_json::json!({
                    role_alias: "researcher",
                    instruction_alias: "go"
                })))
                .unwrap_or_else(|e| {
                    panic!("{role_alias}/{instruction_alias} was not recovered: {e:?}")
                });
                assert_eq!(request.role, "researcher");
                assert_eq!(request.instructions, "go");
            }
        }
    }

    /// Guards the alias lists: adding e.g. "session_id" to either must fail here.
    #[test]
    fn a_widening_field_is_refused_under_every_spelling() {
        for widening in [
            "profile_scope",
            "scope",
            "tool_groups",
            "tools",
            "depth",
            "session_id",
            "parent_session_id",
            "max_turns",
            "context_fraction",
        ] {
            let refusal = into_request(params(serde_json::json!({
                "role": "researcher",
                "instructions": "go",
                widening: "anything at all"
            })))
            .unwrap_err();
            assert!(
                matches!(refusal, Refusal::Malformed(_)),
                "a delegate call carrying `{widening}` was accepted; the caller must not be able \
                 to name any part of the child's authority"
            );
            assert!(
                refusal.message().contains(widening),
                "the refusal for `{widening}` does not name the offending field, so a model \
                 cannot correct it: {}",
                refusal.message()
            );
        }
    }

    #[test]
    fn a_call_with_no_instructions_is_refused_rather_than_defaulted() {
        let refusal =
            into_request(params(serde_json::json!({ "role": "researcher" }))).unwrap_err();
        assert!(matches!(refusal, Refusal::Malformed(_)));
        assert!(refusal.message().contains("instructions"));
    }

    // ── RoleLookup / decide ────────────────────────────────────────────────

    fn request() -> TaskRequest {
        TaskRequest {
            role: "researcher".into(),
            instructions: "look it up".into(),
            inputs: serde_json::Value::Null,
            background: false,
        }
    }

    #[test]
    fn a_recipe_carrying_a_role_produces_a_narrowed_spec() {
        let authority = root(ProfileScope::Household);
        let lookup = RoleLookup::from_recipe("researcher", Some(researcher_yaml()));
        assert!(matches!(lookup, RoleLookup::Found(_)));
        let spec = decide(&authority, lookup, request()).expect("a valid role delegates");
        assert_eq!(spec.role(), "researcher");
        assert_eq!(spec.depth().get(), 1);
        assert!(spec.grants_tool("giap-weather__get_forecast"));
        assert!(!spec.child_authority("c").may_delegate());
        for denied in groups_denied_to_subagents() {
            assert!(!spec.tool_groups().contains(*denied));
        }
    }

    #[test]
    fn a_recipe_that_is_not_a_role_is_refused_rather_than_run() {
        let authority = root(ProfileScope::Household);
        let lookup = RoleLookup::from_recipe("morning_brief", Some("title: Morning Brief\n"));
        assert_eq!(lookup, RoleLookup::NotARole);
        assert!(matches!(
            decide(&authority, lookup, request()),
            Err(Refusal::NotARole(_))
        ));
    }

    #[test]
    fn a_missing_recipe_is_refused() {
        let authority = root(ProfileScope::Household);
        assert_eq!(RoleLookup::from_recipe("nope", None), RoleLookup::Missing);
        assert!(matches!(
            decide(&authority, RoleLookup::Missing, request()),
            Err(Refusal::UnknownRole(_))
        ));
    }

    #[test]
    fn an_unreadable_role_refuses_instead_of_substituting_a_default() {
        let authority = root(ProfileScope::Household);
        for (label, yaml) in [
            // `deny_unknown_fields` makes a typo'd required field a parse error.
            (
                "misspelled tool_groups",
                "giap_role:\n  toolgroups: [giap-weather]\n  instructions: go\n",
            ),
            // Present, parses, fails validation.
            (
                "turn budget past the cap",
                "giap_role:\n  tool_groups: [giap-weather]\n  instructions: go\n  max_turns: 99\n",
            ),
            (
                "no instructions anywhere",
                "giap_role:\n  tool_groups: [giap-weather]\n",
            ),
        ] {
            let lookup = RoleLookup::from_recipe("researcher", Some(yaml));
            assert!(
                matches!(lookup, RoleLookup::Unreadable(_)),
                "{label}: expected an unreadable role, got {lookup:?}"
            );
            let refusal = decide(&authority, lookup, request()).unwrap_err();
            assert!(
                matches!(refusal, Refusal::UnreadableRole { .. }),
                "{label}: {refusal:?}"
            );
        }
    }

    /// Vacuity control for the test above.
    #[test]
    fn the_same_reader_accepts_a_valid_role() {
        assert!(matches!(
            RoleLookup::from_recipe("researcher", Some(researcher_yaml())),
            RoleLookup::Found(_)
        ));
    }

    // ── describe_run ───────────────────────────────────────────────────────

    fn run_with(status: TaskStatus, result: Option<&str>) -> TaskRun {
        TaskRun {
            id: "task-1".into(),
            role: "researcher".into(),
            parent_session_id: "giap-session".into(),
            status,
            result: result.map(str::to_string),
            error: None,
            started_at: chrono::Utc::now(),
            finished_at: None,
        }
    }

    #[test]
    fn only_a_completed_run_hands_its_text_to_the_parent() {
        const SECRET: &str = "the bins go out on Thursday";
        for status in [
            TaskStatus::Cancelled,
            TaskStatus::TurnBudgetExhausted,
            TaskStatus::Failed,
            TaskStatus::Queued,
            TaskStatus::Running,
        ] {
            let described = describe_run(&run_with(status, Some(SECRET)));
            assert!(
                !described.contains(SECRET),
                "{status:?} leaked the child's partial text to the parent: {described}"
            );
        }
        // Vacuity control: a completed run's text does come through.
        let completed = describe_run(&run_with(TaskStatus::Completed, Some(SECRET)));
        assert!(completed.contains(SECRET));
    }

    // ── background + check_task ────────────────────────────────────────────

    #[test]
    fn a_delegation_is_synchronous_unless_the_caller_asks_otherwise() {
        let request = into_request(params(serde_json::json!({
            "role": "researcher",
            "instructions": "go"
        })))
        .unwrap();
        assert!(
            !request.background,
            "a caller that said nothing about background got a run that outlives its turn"
        );
    }

    #[test]
    fn the_boolean_spellings_a_small_model_emits_are_recovered() {
        for (spelling, expected) in [
            (serde_json::json!(true), true),
            (serde_json::json!("true"), true),
            (serde_json::json!("True"), true),
            (serde_json::json!("yes"), true),
            (serde_json::json!("1"), true),
            (serde_json::json!(false), false),
            (serde_json::json!("false"), false),
            (serde_json::json!("no"), false),
        ] {
            let request = into_request(params(serde_json::json!({
                "role": "researcher",
                "instructions": "go",
                "background": spelling
            })))
            .unwrap_or_else(|e| panic!("`background: {spelling}` was not recovered: {e:?}"));
            assert_eq!(request.background, expected, "background: {spelling}");
        }
    }

    /// A `match`, not `unwrap_err()`: the `unwrap_or(false)` mutation must reach the message below.
    #[test]
    fn an_unreadable_background_value_refuses_rather_than_defaulting() {
        for nonsense in [
            serde_json::json!("later"),
            serde_json::json!(7),
            serde_json::json!({"when": "later"}),
        ] {
            let refusal = match into_request(params(serde_json::json!({
                "role": "researcher",
                "instructions": "go",
                "background": nonsense
            }))) {
                Err(refusal) => refusal,
                Ok(request) => panic!(
                    "`background: {nonsense}` was accepted and silently became \
                     background={}, so a caller that asked for one thing was given the other \
                     with nothing to tell it apart",
                    request.background
                ),
            };
            assert!(
                matches!(refusal, Refusal::Malformed(_)),
                "`background: {nonsense}` was refused, but not as Malformed: {refusal:?}"
            );
            assert!(
                refusal.message().contains("background"),
                "the refusal does not name the field, so the model cannot correct it: {}",
                refusal.message()
            );
        }
    }

    fn run_of(session: &str, id: &str) -> TaskRun {
        TaskRun {
            id: id.into(),
            role: "researcher".into(),
            parent_session_id: session.into(),
            status: TaskStatus::Completed,
            result: Some("the bins go out on Thursday".into()),
            error: None,
            started_at: chrono::Utc::now(),
            finished_at: None,
        }
    }

    #[test]
    fn a_task_belonging_to_another_conversation_is_not_readable() {
        const SECRET: &str = "the bins go out on Thursday";
        let theirs = run_of("someone-elses-session", "task-1");
        let refusal = authorise_task(Some(theirs), "my-session", "task-1").unwrap_err();
        assert!(
            matches!(refusal, Refusal::UnknownTask { .. }),
            "{refusal:?}"
        );
        assert!(
            !refusal.message().contains(SECRET),
            "the refusal leaked the other conversation's result: {}",
            refusal.message()
        );
    }

    /// Vacuity control for the test above.
    #[test]
    fn a_task_of_this_conversation_is_readable() {
        let mine = run_of("my-session", "task-1");
        let run = authorise_task(Some(mine), "my-session", "task-1").expect("my own task");
        assert_eq!(run.id, "task-1");
        assert!(describe_run(&run).contains("the bins go out on Thursday"));
    }

    #[test]
    fn a_missing_task_and_somebody_elses_are_indistinguishable_to_the_caller() {
        let missing = authorise_task(None, "my-session", "task-1").unwrap_err();
        let theirs =
            authorise_task(Some(run_of("other", "task-1")), "my-session", "task-1").unwrap_err();
        assert_eq!(missing.message(), theirs.message());
        assert_ne!(
            missing.trace(),
            theirs.trace(),
            "the log cannot tell a hallucinated id from a probe of another conversation"
        );
    }

    #[test]
    fn a_check_with_no_id_is_refused_rather_than_guessed_at() {
        let refusal = check_task_id(CheckTaskParams::default()).unwrap_err();
        assert!(matches!(refusal, Refusal::Malformed(_)));
        assert!(refusal.message().contains("task_id"));
    }

    #[test]
    fn the_task_id_aliases_a_small_model_invents_are_recovered() {
        for alias in TASK_ID_ALIASES {
            let parsed: CheckTaskParams =
                serde_json::from_value(serde_json::json!({ alias: "task-1" }))
                    .expect("CheckTaskParams accepts any object");
            assert_eq!(
                check_task_id(parsed).unwrap_or_else(|e| panic!("{alias}: {e:?}")),
                "task-1"
            );
        }
    }

    #[test]
    fn a_running_background_task_is_reported_with_its_id_and_no_answer() {
        let mut run = run_of("my-session", "task-7");
        run.status = TaskStatus::Queued;
        let described = describe_run(&run);
        assert!(described.contains("task-7"), "{described}");
        assert!(described.contains("still working"), "{described}");
        assert!(
            !described.contains("the bins go out on Thursday"),
            "a queued run handed the parent text it has not earned: {described}"
        );
    }

    #[test]
    fn an_exhausted_budget_is_not_reported_as_an_answer() {
        let described = describe_run(&run_with(TaskStatus::TurnBudgetExhausted, None));
        assert!(described.contains("ran out"));
        assert!(described.contains("Do not treat this as a result"));
    }

    // ── check_task at its CALL SITE ────────────────────────────────────────
    // Drives `run_check` (all of `check_task` but the wrapper; a `RequestContext` can't be
    // built in a test) so dropping its `authorise_task` call fails a test.

    struct FakeOrchestrator {
        run: Option<TaskRun>,
    }

    #[async_trait::async_trait]
    impl Orchestrator for FakeOrchestrator {
        async fn spawn(&self, _spec: TaskSpec) -> anyhow::Result<TaskRun> {
            unreachable!("check_task does not spawn")
        }
        /// Keyed by task id alone, like the real one, so ownership can't be checked here.
        async fn poll(&self, _task_id: &str) -> anyhow::Result<Option<TaskRun>> {
            Ok(self.run.clone())
        }
        async fn cancel(&self, _task_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn list(&self, _parent_session_id: &str) -> anyhow::Result<Vec<TaskRun>> {
            Ok(Vec::new())
        }
        async fn cancel_children_of(&self, _parent_session_id: &str) -> anyhow::Result<usize> {
            Ok(0)
        }
    }

    struct NoRecipes;

    #[async_trait::async_trait]
    impl pond_core::user_data::ports::recipe::AgentRecipeRepository for NoRecipes {
        async fn list(
            &self,
        ) -> anyhow::Result<Vec<pond_core::user_data::domain::recipe::AgentRecipe>> {
            Ok(Vec::new())
        }
        async fn get_by_name(
            &self,
            _name: &str,
        ) -> anyhow::Result<Option<pond_core::user_data::domain::recipe::AgentRecipe>> {
            Ok(None)
        }
        async fn get_by_id(
            &self,
            _id: &str,
        ) -> anyhow::Result<Option<pond_core::user_data::domain::recipe::AgentRecipe>> {
            Ok(None)
        }
        async fn upsert(
            &self,
            _recipe: &pond_core::user_data::domain::recipe::AgentRecipe,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn meta_for(engine_session: &str) -> rmcp::model::Meta {
        let mut m = rmcp::model::Meta::new();
        m.0.insert(
            crate::SESSION_ID_META_KEY.to_string(),
            serde_json::Value::String(engine_session.to_string()),
        );
        m
    }

    /// A server with one live turn in session `my-session` whose `poll` always returns `run`.
    /// Hold the lease: dropping it revokes the authority, and every check would pass vacuously.
    fn server_with(
        run: Option<TaskRun>,
    ) -> (
        OrchestratorMcpServer,
        pond_core::shared::services::turn_authority::TurnAuthorityLease,
    ) {
        server_with_scope(ProfileScope::Household, run)
    }

    fn server_with_scope(
        scope: ProfileScope,
        run: Option<TaskRun>,
    ) -> (
        OrchestratorMcpServer,
        pond_core::shared::services::turn_authority::TurnAuthorityLease,
    ) {
        let authorities = Arc::new(TurnAuthorityRegistry::new());
        let lease = authorities.publish(
            "engine-session-1",
            DelegationAuthority::root("my-session", scope, groups(&["giap-weather"])),
            // `Default::default()` because the token type is from `tokio-util`, not a dependency.
            Default::default(),
        );
        let deps = OrchestratorDeps::new(
            Arc::new(FakeOrchestrator { run }),
            authorities,
            Arc::new(NoRecipes),
        );
        (OrchestratorMcpServer::new(Some(deps)), lease)
    }

    fn check_for(task_id: &str) -> CheckTaskParams {
        CheckTaskParams {
            task_id: Some(task_id.to_string()),
            extra: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn check_task_does_not_read_a_task_belonging_to_another_conversation() {
        const SECRET: &str = "the bins go out on Thursday";
        let (server, _lease) = server_with(Some(run_of("someone-elses-session", "task-1")));

        let answer = server
            .run_check(&meta_for("engine-session-1"), check_for("task-1"))
            .await;

        match answer {
            Err(refusal) => {
                assert!(
                    matches!(refusal, Refusal::UnknownTask { .. }),
                    "expected the unknown-task refusal, got {refusal:?}"
                );
                assert!(
                    !refusal.message().contains(SECRET),
                    "the refusal leaked the other conversation's result: {}",
                    refusal.message()
                );
            }
            Ok(text) => panic!(
                "check_task answered a task belonging to another conversation - on this pond that \
                 is another household member's delegation result, read back from a task id: \
                 {text}"
            ),
        }
    }

    /// Vacuity control for the test above; also proves the fixture reaches `describe_run`.
    #[tokio::test]
    async fn check_task_answers_a_task_of_this_conversation() {
        let (server, _lease) = server_with(Some(run_of("my-session", "task-1")));

        let text = server
            .run_check(&meta_for("engine-session-1"), check_for("task-1"))
            .await
            .expect("my own task is readable");
        assert!(
            text.contains("the bins go out on Thursday"),
            "the caller's own finished task did not hand back its answer: {text}"
        );
    }

    #[tokio::test]
    async fn check_task_refuses_an_id_nothing_knows_in_the_same_words() {
        let (server, _lease) = server_with(None);
        let unknown = server
            .run_check(&meta_for("engine-session-1"), check_for("task-1"))
            .await
            .expect_err("nothing is running under that id");

        let (foreign_server, _foreign_lease) =
            server_with(Some(run_of("someone-elses-session", "task-1")));
        let foreign = foreign_server
            .run_check(&meta_for("engine-session-1"), check_for("task-1"))
            .await
            .expect_err("that task belongs to another conversation");

        assert_eq!(unknown.message(), foreign.message());
        assert_ne!(
            unknown.trace(),
            foreign.trace(),
            "the log cannot tell a hallucinated id from a probe of another conversation"
        );
    }

    /// Without this, the tests above would pass against a `run_check` that dropped `authorise`.
    #[tokio::test]
    async fn check_task_refuses_a_caller_with_no_live_turn() {
        let (server, lease) = server_with(Some(run_of("my-session", "task-1")));
        drop(lease);

        let refusal = server
            .run_check(&meta_for("engine-session-1"), check_for("task-1"))
            .await
            .expect_err("the turn that could have asked is over");
        assert!(
            matches!(refusal, Refusal::Unauthorised { .. }),
            "expected the unauthorised refusal, got {refusal:?}"
        );

        // ... and so is a call carrying no engine session at all.
        let (server, _lease) = server_with(Some(run_of("my-session", "task-1")));
        let refusal = server
            .run_check(&rmcp::model::Meta::new(), check_for("task-1"))
            .await
            .expect_err("a call with no `_meta` has no caller to authorise");
        assert!(
            matches!(refusal, Refusal::Unauthorised { .. }),
            "expected the unauthorised refusal, got {refusal:?}"
        );
    }

    /// Catches `authorise(...)` in `run_check` being swapped for a bare `.ok_or(Unauthorised)`.
    #[tokio::test]
    async fn check_task_refuses_a_guest_turn_that_owns_the_task() {
        let (server, _lease) = server_with_scope(
            ProfileScope::Guest,
            // The guest's OWN task, so the refusal is the caller's, not the ownership check's.
            Some(run_of("my-session", "task-1")),
        );
        let refusal = server
            .run_check(&meta_for("engine-session-1"), check_for("task-1"))
            .await
            .expect_err("a guest turn may not run or read a delegation");
        assert_eq!(
            refusal,
            Refusal::Guest,
            "a guest turn reached a delegated agent's answer"
        );
    }
}
