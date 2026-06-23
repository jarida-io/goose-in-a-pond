use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose::agents::extension::Envs;
use goose::agents::{Agent as GooseAgent, ExtensionConfig};
use pond_core::mcp::ports::extension_manager::{
    AddExtensionRequest, ExtensionInfo, ExtensionManagerPort,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct GiapGooseExtensionManager {
    agent: Arc<GooseAgent>,
    session_id: String,
    /// Names of extensions that have been disabled by the user.
    /// Kept in memory; persists until server restart.
    disabled: Arc<RwLock<HashSet<String>>>,
    /// Stored configurations for re-enabling extensions.
    extension_configs: Arc<RwLock<HashMap<String, ExtensionConfig>>>,
    /// Tracks last error per extension name.
    errors: Arc<RwLock<HashMap<String, String>>>,
}

impl GiapGooseExtensionManager {
    pub fn new(agent: Arc<GooseAgent>, session_id: String) -> Self {
        Self {
            agent,
            session_id,
            disabled: Arc::new(RwLock::new(HashSet::new())),
            extension_configs: Arc::new(RwLock::new(HashMap::new())),
            errors: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register_config(&self, name: String, config: ExtensionConfig) {
        self.extension_configs.write().await.insert(name, config);
    }
}

#[async_trait]
impl ExtensionManagerPort for GiapGooseExtensionManager {
    async fn list_extensions(&self) -> Result<Vec<ExtensionInfo>> {
        let tools = self.agent.list_tools(&self.session_id, None).await;

        // Group tools by extension name prefix (format: "ext_name__tool_name")
        let mut ext_map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for tool in &tools {
            let name = tool.name.as_ref();
            if let Some(sep) = name.find("__") {
                let ext_name = &name[..sep];
                let tool_name = &name[sep + 2..];
                ext_map
                    .entry(ext_name.to_string())
                    .or_default()
                    .push(tool_name.to_string());
            } else {
                ext_map
                    .entry("default".to_string())
                    .or_default()
                    .push(name.to_string());
            }
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
                match client.get(uri).send().await {
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

        match self.agent.add_extension(config, &self.session_id).await {
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
            let all_tools = self.agent.list_tools(&self.session_id, None).await;
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
        self.agent
            .remove_extension(name, &self.session_id)
            .await
            .map_err(|e| anyhow!("Failed to remove extension: {}", e))
    }

    async fn list_tools(&self) -> Result<Vec<String>> {
        let tools = self.agent.list_tools(&self.session_id, None).await;
        Ok(tools.iter().map(|t| t.name.as_ref().to_string()).collect())
    }

    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let mut disabled = self.disabled.write().await;
        if enabled {
            if disabled.contains(name) {
                // Re-enable: re-add to Goose agent
                let configs = self.extension_configs.read().await;
                if let Some(config) = configs.get(name) {
                    self.agent
                        .add_extension(config.clone(), &self.session_id)
                        .await
                        .map_err(|e| anyhow!("Failed to re-enable extension: {}", e))?;
                }
                disabled.remove(name);
            }
        } else {
            if !disabled.contains(name) {
                // Disable: remove from Goose agent
                self.agent
                    .remove_extension(name, &self.session_id)
                    .await
                    .map_err(|e| anyhow!("Failed to disable extension: {}", e))?;
                disabled.insert(name.to_string());
            }
        }
        Ok(())
    }
}
