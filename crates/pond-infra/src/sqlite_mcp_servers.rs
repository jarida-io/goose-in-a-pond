//! SQLite-backed `McpServerRepository`: saved MCP servers, auto-connected at startup.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::mcp_server::{McpServerConfig, McpServerRepository};
use sqlx::{Pool, Row, Sqlite};
use std::collections::HashMap;

pub struct SqliteMcpServerRepository {
    pool: Pool<Sqlite>,
}

impl SqliteMcpServerRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl McpServerRepository for SqliteMcpServerRepository {
    async fn list(&self) -> Result<Vec<McpServerConfig>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, description, command, args, env, uri, enabled, created_at \
             FROM mcp_servers ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                let args_json: String = row.try_get("args")?;
                let env_json: String = row.try_get("env")?;
                let args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
                let env: HashMap<String, String> =
                    serde_json::from_str(&env_json).unwrap_or_default();
                Ok(McpServerConfig {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    kind: row.try_get("kind")?,
                    description: row.try_get("description")?,
                    command: row.try_get("command")?,
                    args,
                    env,
                    uri: row.try_get("uri")?,
                    enabled: row.try_get::<i64, _>("enabled")? != 0,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    async fn save(&self, cfg: &McpServerConfig) -> Result<()> {
        let args_json = serde_json::to_string(&cfg.args)?;
        let env_json = serde_json::to_string(&cfg.env)?;
        sqlx::query(
            "INSERT INTO mcp_servers \
                (id, name, kind, description, command, args, env, uri, enabled, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(name) DO UPDATE SET \
                id          = excluded.id, \
                kind        = excluded.kind, \
                description = excluded.description, \
                command     = excluded.command, \
                args        = excluded.args, \
                env         = excluded.env, \
                uri         = excluded.uri, \
                enabled     = excluded.enabled",
        )
        .bind(&cfg.id)
        .bind(&cfg.name)
        .bind(&cfg.kind)
        .bind(&cfg.description)
        .bind(&cfg.command)
        .bind(&args_json)
        .bind(&env_json)
        .bind(&cfg.uri)
        .bind(if cfg.enabled { 1i64 } else { 0i64 })
        .bind(&cfg.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, name: &str) -> Result<()> {
        sqlx::query("DELETE FROM mcp_servers WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        sqlx::query("UPDATE mcp_servers SET enabled = ? WHERE name = ?")
            .bind(if enabled { 1i64 } else { 0i64 })
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
