use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose::agents::extension::Envs;
use goose::agents::{Agent as GooseAgent, ExtensionConfig};
use goose::config::GooseMode;
use goose::session::session_manager::{SessionManager, SessionType};
use pond_core::mcp::ports::extension_manager::{
    AddExtensionRequest, ExtensionInfo, ExtensionManagerPort, ToolInfo,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Reserved name of the engine session that owns MCP extension state.
///
/// Extensions are added to and removed from this one session; chat sessions
/// inherit them. The name — not an id — is the identity, for two reasons.
/// It lets [`resolve_extension_session`] find the row again after a restart
/// instead of latching onto whichever chat session happens to be newest. And
/// it makes the row un-deletable by any user action: `GooseAdapter::forget_session`
/// only ever deletes engine rows paired with a GIAP session UUID, so a row
/// named this can never be collateral of a deleted chat.
pub const EXTENSION_SESSION_NAME: &str = "giap-extensions";

/// Resolves the engine session that owns extension state, creating it if it
/// does not exist yet.
///
/// Extension subprocesses spawn in the returned session's `working_dir`, so an
/// existing row's is re-pinned to `working_dir` whenever it has drifted — a
/// long-lived session's directory is otherwise frozen at whatever the cwd was
/// when it was first created, potentially days and several restarts ago.
pub async fn resolve_extension_session(
    session_manager: &SessionManager,
    working_dir: &Path,
) -> Result<String> {
    let existing = session_manager.list_sessions().await?;

    if let Some(session) = existing.iter().find(|s| s.name == EXTENSION_SESSION_NAME) {
        if session.working_dir != working_dir {
            session_manager
                .update(&session.id)
                .working_dir(working_dir.to_path_buf())
                .apply()
                .await
                .map_err(|e| anyhow!("Failed to refresh extension session working_dir: {e}"))?;
            tracing::info!(
                session_id = %session.id,
                working_dir = %working_dir.display(),
                "re-pinned the extension session's working_dir"
            );
        }
        return Ok(session.id.clone());
    }

    let session = session_manager
        .create_session(
            working_dir.to_path_buf(),
            EXTENSION_SESSION_NAME.to_string(),
            SessionType::User,
            GooseMode::Auto,
        )
        .await
        .map_err(|e| anyhow!("Failed to create the extension session: {e}"))?;

    tracing::info!(
        session_id = %session.id,
        working_dir = %working_dir.display(),
        "created the extension session"
    );
    Ok(session.id)
}

pub struct GiapGooseExtensionManager {
    agent: Arc<GooseAgent>,
    session_manager: Arc<SessionManager>,
    /// Id of the engine session extensions are added to, resolved on first use
    /// and re-resolved whenever it stops being valid. Never pinned for the
    /// process lifetime: the row it names can be deleted or wiped underneath
    /// us, and a dangling id fails every method on this port.
    session_id: Arc<RwLock<Option<String>>>,
    /// Names of extensions that have been disabled by the user.
    /// Kept in memory; persists until server restart.
    disabled: Arc<RwLock<HashSet<String>>>,
    /// Stored configurations for re-enabling extensions.
    extension_configs: Arc<RwLock<HashMap<String, ExtensionConfig>>>,
    /// Tracks last error per extension name.
    errors: Arc<RwLock<HashMap<String, String>>>,
}

impl GiapGooseExtensionManager {
    pub fn new(agent: Arc<GooseAgent>, session_manager: Arc<SessionManager>) -> Self {
        Self {
            agent,
            session_manager,
            session_id: Arc::new(RwLock::new(None)),
            disabled: Arc::new(RwLock::new(HashSet::new())),
            extension_configs: Arc::new(RwLock::new(HashMap::new())),
            errors: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// The engine session id to add extensions to, re-resolving it if the
    /// cached one no longer names a live extension session.
    ///
    /// The name check on top of a successful lookup guards against a recycled
    /// id: engine ids are `MAX(per-day counter) + 1`, so an id freed by a
    /// delete is handed to the next session created that day. Without the
    /// check a dangling id could silently resolve to a stranger's chat and
    /// spawn extensions in its working directory.
    async fn session(&self) -> Result<String> {
        // Fast path: a cached id that still names the extension session.
        if let Some(cached) = self.session_id.read().await.clone() {
            match self.session_manager.get_session(&cached, false).await {
                Ok(session) if session.name == EXTENSION_SESSION_NAME => return Ok(cached),
                Ok(session) => tracing::warn!(
                    session_id = %cached,
                    found_name = %session.name,
                    "the cached extension session id names a different session — re-resolving"
                ),
                Err(e) => tracing::warn!(
                    session_id = %cached,
                    error = %e,
                    "the cached extension session is gone — re-resolving"
                ),
            }
        }

        // Slow path. The write guard is held across the resolve so concurrent
        // callers cannot each create a session; re-checking the cache under it
        // means the losers of that race reuse what the winner resolved.
        let mut guard = self.session_id.write().await;
        if let Some(cached) = guard.clone() {
            if let Ok(session) = self.session_manager.get_session(&cached, false).await {
                if session.name == EXTENSION_SESSION_NAME {
                    return Ok(cached);
                }
            }
        }

        let working_dir = std::env::current_dir()
            .map_err(|e| anyhow!("Failed to read the current directory: {e}"))?;
        let resolved = resolve_extension_session(&self.session_manager, &working_dir).await?;
        tracing::info!(
            previous_session_id = ?guard.as_deref(),
            session_id = %resolved,
            "bound the extension manager to a session"
        );
        *guard = Some(resolved.clone());
        Ok(resolved)
    }

    pub async fn register_config(&self, name: String, config: ExtensionConfig) {
        self.extension_configs.write().await.insert(name, config);
    }
}

/// GIAP tool names are fully-qualified as `"ext_name__tool_name"`. Splits a
/// tool's name into (extension, bare tool name), falling back to the
/// `"default"` extension bucket for tools with no `__` separator.
fn split_tool_name(full_name: &str) -> (String, String) {
    match full_name.find("__") {
        Some(sep) => (
            full_name[..sep].to_string(),
            full_name[sep + 2..].to_string(),
        ),
        None => ("default".to_string(), full_name.to_string()),
    }
}

#[async_trait]
impl ExtensionManagerPort for GiapGooseExtensionManager {
    async fn list_extensions(&self) -> Result<Vec<ExtensionInfo>> {
        let session_id = self.session().await?;
        let tools = self.agent.list_tools(&session_id, None).await;

        // Group tools by extension name prefix (format: "ext_name__tool_name")
        let mut ext_map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for tool in &tools {
            let (ext_name, tool_name) = split_tool_name(tool.name.as_ref());
            ext_map.entry(ext_name).or_default().push(tool_name);
        }

        let disabled = self.disabled.read().await;
        let errors = self.errors.read().await;

        Ok(ext_map
            .into_iter()
            .map(|(name, tools)| {
                let (status, last_error) = if let Some(err) = errors.get(&name) {
                    ("error".to_string(), Some(err.clone()))
                } else if tools.is_empty() {
                    ("loading".to_string(), None)
                } else {
                    ("connected".to_string(), None)
                };
                ExtensionInfo {
                    kind: "builtin".to_string(),
                    description: String::new(),
                    enabled: !disabled.contains(&name),
                    name,
                    tools,
                    status,
                    last_error,
                }
            })
            .collect())
    }

    async fn add_extension(&self, request: AddExtensionRequest) -> Result<ExtensionInfo> {
        // --- Phase 1.5: Pre-add validation ---

        // Stdio: check that the command exists in PATH
        if request.kind == "stdio" {
            if let Some(cmd) = &request.command {
                let which_check = std::process::Command::new("which")
                    .arg(cmd)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output();
                match which_check {
                    Ok(output) if output.status.success() => {} // found in PATH
                    _ => {
                        return Err(anyhow!(
                            "Command '{}' not found in PATH. Install it first.",
                            cmd
                        ))
                    }
                }
            }
        }

        // HTTP: attempt a lightweight connectivity check with a timeout
        if matches!(request.kind.as_str(), "http" | "streamable_http") {
            if let Some(uri) = &request.uri {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(5))
                    .build()
                    .unwrap_or_default();
                // PAI-2 P6a: the URI here is typed by whoever is adding the
                // extension, so this is the most attacker-influenced
                // destination in the tree. The gate runs before the probe --
                // refusing after the packet has left is not a refusal.
                let call = pond_core::shared::services::egress::begin(uri, "GET")
                    .map_err(|e| anyhow!("Cannot reach MCP server at {}: {}", uri, e))?;
                let probed = client.get(uri).send().await;
                call.finish(probed.as_ref().ok().map(|r| r.status().as_u16()));
                match probed {
                    Ok(_) => {} // reachable
                    Err(e) => return Err(anyhow!("Cannot reach MCP server at {}: {}", uri, e)),
                }
            }
        }

        // --- Build Goose ExtensionConfig ---

        let config = match request.kind.as_str() {
            "builtin" => ExtensionConfig::Builtin {
                name: request.name.clone(),
                description: request.description.clone(),
                display_name: None,
                timeout: None,
                bundled: Some(false),
                available_tools: vec![],
            },
            "stdio" => {
                let cmd = request
                    .command
                    .clone()
                    .ok_or_else(|| anyhow!("stdio extension requires 'command'"))?;
                let mut env_map = std::collections::HashMap::new();
                for (k, v) in &request.env {
                    env_map.insert(k.clone(), v.clone());
                }
                ExtensionConfig::Stdio {
                    name: request.name.clone(),
                    description: request.description.clone(),
                    cmd,
                    args: request.args.clone(),
                    envs: Envs::new(env_map),
                    env_keys: vec![],
                    timeout: None,
                    cwd: None,
                    bundled: None,
                    available_tools: vec![],
                }
            }
            "http" | "streamable_http" => {
                let uri = request
                    .uri
                    .clone()
                    .ok_or_else(|| anyhow!("http extension requires 'uri'"))?;
                ExtensionConfig::StreamableHttp {
                    name: request.name.clone(),
                    description: request.description.clone(),
                    uri,
                    envs: Envs::default(),
                    env_keys: vec![],
                    headers: std::collections::HashMap::new(),
                    timeout: None,
                    bundled: None,
                    available_tools: vec![],
                    socket: None,
                }
            }
            other => return Err(anyhow!("Unknown extension kind: {}", other)),
        };

        // Store config for possible re-enabling later
        self.extension_configs
            .write()
            .await
            .insert(request.name.clone(), config.clone());

        // --- Add to Goose agent ---

        let session_id = self.session().await?;

        match self.agent.add_extension(config, &session_id).await {
            Ok(()) => {
                // Clear any previous error for this extension
                self.errors.write().await.remove(&request.name);
            }
            Err(e) => {
                let error_msg = format!("Failed to add extension: {}", e);
                self.errors
                    .write()
                    .await
                    .insert(request.name.clone(), error_msg.clone());
                return Err(anyhow!(error_msg));
            }
        }

        // --- Phase 1.4: Poll for tools after add ---

        let mut discovered_tools = vec![];
        let prefix = format!("{}__", request.name);
        for _ in 0..3 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let all_tools = self.agent.list_tools(&session_id, None).await;
            discovered_tools = all_tools
                .iter()
                .filter_map(|t| {
                    let name = t.name.as_ref();
                    name.strip_prefix(&prefix).map(|s| s.to_string())
                })
                .collect::<Vec<_>>();
            if !discovered_tools.is_empty() {
                break;
            }
        }

        let status = if discovered_tools.is_empty() {
            "loading".to_string()
        } else {
            "connected".to_string()
        };

        Ok(ExtensionInfo {
            name: request.name,
            kind: request.kind,
            description: request.description,
            tools: discovered_tools,
            enabled: true,
            status,
            last_error: None,
        })
    }

    async fn remove_extension(&self, name: &str) -> Result<()> {
        self.extension_configs.write().await.remove(name);
        self.disabled.write().await.remove(name);
        self.errors.write().await.remove(name);
        let session_id = self.session().await?;
        self.agent
            .remove_extension(name, &session_id)
            .await
            .map_err(|e| anyhow!("Failed to remove extension: {}", e))
    }

    async fn list_tools(&self) -> Result<Vec<String>> {
        let session_id = self.session().await?;
        let tools = self.agent.list_tools(&session_id, None).await;
        Ok(tools.iter().map(|t| t.name.as_ref().to_string()).collect())
    }

    async fn list_tools_detailed(&self) -> Result<Vec<ToolInfo>> {
        let session_id = self.session().await?;
        let tools = self.agent.list_tools(&session_id, None).await;
        Ok(tools
            .iter()
            .map(|t| {
                let (extension, name) = split_tool_name(t.name.as_ref());
                ToolInfo {
                    extension,
                    name,
                    description: t.description.as_ref().map(|d| d.to_string()),
                }
            })
            .collect())
    }

    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let session_id = self.session().await?;
        let mut disabled = self.disabled.write().await;
        if enabled {
            if disabled.contains(name) {
                // Re-enable: re-add to Goose agent
                let configs = self.extension_configs.read().await;
                if let Some(config) = configs.get(name) {
                    self.agent
                        .add_extension(config.clone(), &session_id)
                        .await
                        .map_err(|e| anyhow!("Failed to re-enable extension: {}", e))?;
                }
                disabled.remove(name);
            }
        } else {
            if !disabled.contains(name) {
                // Disable: remove from Goose agent
                self.agent
                    .remove_extension(name, &session_id)
                    .await
                    .map_err(|e| anyhow!("Failed to disable extension: {}", e))?;
                disabled.insert(name.to_string());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goose::agents::{AgentConfig, GoosePlatform};

    /// A manager backed by an isolated on-disk session store, so a test can
    /// delete rows out from under it the way a user deleting a chat does.
    fn manager(data_dir: &Path) -> (GiapGooseExtensionManager, Arc<SessionManager>) {
        let session_manager = Arc::new(SessionManager::new(data_dir.to_path_buf()));
        let config = AgentConfig::new(
            session_manager.clone(),
            goose::config::permission::PermissionManager::instance(),
            None,
            GooseMode::Auto,
            true,
            GoosePlatform::GooseCli,
        );
        let agent = Arc::new(GooseAgent::with_config(config));
        (
            GiapGooseExtensionManager::new(agent, session_manager.clone()),
            session_manager,
        )
    }

    #[tokio::test]
    async fn resolves_the_named_session_over_a_newer_chat() {
        let data_dir = tempfile::tempdir().unwrap();
        let session_manager = SessionManager::new(data_dir.path().to_path_buf());
        let cwd = data_dir.path().to_path_buf();

        let extensions = resolve_extension_session(&session_manager, &cwd)
            .await
            .unwrap();

        // A chat created afterwards is the most recently active session, which
        // is exactly what the old `list_sessions().first()` selection latched
        // onto. Resolving by name has to ignore it.
        let chat = session_manager
            .create_session(
                cwd.clone(),
                "1a7e1234-9e64-44c4-84f7-4c48481f5bec".to_string(),
                SessionType::User,
                GooseMode::Auto,
            )
            .await
            .unwrap();

        let again = resolve_extension_session(&session_manager, &cwd)
            .await
            .unwrap();

        assert_eq!(
            again, extensions,
            "resolve must return the extension session, not the newest chat"
        );
        assert_ne!(again, chat.id);
    }

    #[tokio::test]
    async fn re_pins_a_stale_working_dir() {
        let data_dir = tempfile::tempdir().unwrap();
        let session_manager = SessionManager::new(data_dir.path().to_path_buf());
        let first = data_dir.path().join("first");
        let second = data_dir.path().join("second");

        let id = resolve_extension_session(&session_manager, &first)
            .await
            .unwrap();
        let same = resolve_extension_session(&session_manager, &second)
            .await
            .unwrap();

        assert_eq!(same, id, "the same row must be reused across cwd changes");
        assert_eq!(
            session_manager
                .get_session(&id, false)
                .await
                .unwrap()
                .working_dir,
            second,
            "extension subprocesses spawn here, so it must track the current cwd"
        );
    }

    #[tokio::test]
    async fn heals_after_its_session_is_deleted() {
        let data_dir = tempfile::tempdir().unwrap();
        let (mgr, session_manager) = manager(data_dir.path());

        let first = mgr.session().await.unwrap();
        assert_eq!(
            mgr.session().await.unwrap(),
            first,
            "a valid id must be served from cache"
        );

        // What a user deleting the bound chat used to do to the old pinned id:
        // every subsequent call failed with "Session not found" until restart.
        session_manager.delete_session(&first).await.unwrap();
        assert!(session_manager.get_session(&first, false).await.is_err());

        // A fresh row, not the error the old pinned id produced forever. Its
        // id may well be `first` again — the per-day counter is `MAX + 1`, so
        // deleting the only row frees the id — which is why the assertion that
        // matters is that it now resolves to a live extension session.
        let healed = mgr.session().await.unwrap();
        assert_eq!(
            session_manager
                .get_session(&healed, false)
                .await
                .unwrap()
                .name,
            EXTENSION_SESSION_NAME
        );
    }

    #[tokio::test]
    async fn rejects_a_recycled_id_belonging_to_a_chat() {
        let data_dir = tempfile::tempdir().unwrap();
        let (mgr, session_manager) = manager(data_dir.path());

        let bound = mgr.session().await.unwrap();
        session_manager.delete_session(&bound).await.unwrap();

        // Engine ids are `MAX(per-day counter) + 1`, so the id just freed is
        // handed to the next session created that day — here a chat. Serving
        // the cached id back would spawn extensions in a stranger's session.
        let chat = session_manager
            .create_session(
                data_dir.path().to_path_buf(),
                "e339de38-9bd8-45f3-8cf3-429b8b8982f9".to_string(),
                SessionType::User,
                GooseMode::Auto,
            )
            .await
            .unwrap();
        assert_eq!(chat.id, bound, "precondition: the id was recycled");

        let healed = mgr.session().await.unwrap();
        assert_ne!(
            healed, chat.id,
            "must not resolve to the chat that took the id"
        );
        assert_eq!(
            session_manager
                .get_session(&healed, false)
                .await
                .unwrap()
                .name,
            EXTENSION_SESSION_NAME
        );
    }
}
