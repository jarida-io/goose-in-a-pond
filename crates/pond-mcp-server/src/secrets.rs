//! Process-global secret store for MCP tools that need third-party API keys. Exactly one
//! instance per process: `FileSecretRepository` caches `secrets.json` and rewrites it whole.

use pond_core::mcp::domain::tool_group::{required_secret, tools_missing_secrets};
use pond_core::security::ports::secret::SecretRepository;
use std::collections::{BTreeSet, HashSet};
use std::future::Future;
use std::sync::{Arc, OnceLock};

static SECRET_REPO: OnceLock<Arc<dyn SecretRepository + Send + Sync>> = OnceLock::new();

/// Install the secret store. Call once at startup, before any chat session.
pub fn init_secret_deps(repo: Arc<dyn SecretRepository + Send + Sync>) {
    let _ = SECRET_REPO.set(repo);
}

/// Read one secret; `None` if unset, unreadable or never installed (the CLI paths), so
/// every failure degrades a tool to its keyless fallback.
pub async fn secret(key: &str) -> Option<String> {
    let repo = SECRET_REPO.get()?;
    match repo.get(key).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(key, error = %e, "secret store read failed; treating the key as unset");
            None
        }
    }
}

/// The tools in `tools` a turn must not offer, because a secret each needs is not stored now.
/// Read per turn, so a key saved from any app reaches the next turn with no restart.
pub async fn tools_withheld_for_missing_secrets<'a>(
    tools: impl IntoIterator<Item = &'a str>,
) -> HashSet<String> {
    withheld_by(tools, secret).await
}

/// As [`tools_withheld_for_missing_secrets`], reading each secret through `lookup`; each
/// distinct secret is read once. A blank value counts as absent.
async fn withheld_by<'a, F, Fut>(
    tools: impl IntoIterator<Item = &'a str>,
    lookup: F,
) -> HashSet<String>
where
    F: Fn(&'static str) -> Fut,
    Fut: Future<Output = Option<String>>,
{
    let tools: Vec<&str> = tools.into_iter().collect();
    let needed: BTreeSet<&'static str> = tools.iter().filter_map(|t| required_secret(t)).collect();
    let mut stored: HashSet<&'static str> = HashSet::new();
    for key in needed {
        if lookup(key).await.is_some_and(|v| !v.trim().is_empty()) {
            stored.insert(key);
        }
    }
    tools_missing_secrets(tools, |key| stored.contains(key))
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    struct OneKey;

    #[async_trait::async_trait]
    impl SecretRepository for OneKey {
        async fn get(&self, key: &str) -> Result<Option<String>> {
            Ok((key == "GUARDIAN_API_KEY").then(|| "guardian-live-key".to_string()))
        }
        async fn set(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn list_keys(&self) -> Result<Vec<String>> {
            Ok(vec!["GUARDIAN_API_KEY".to_string()])
        }
        async fn has(&self, key: &str) -> Result<bool> {
            Ok(key == "GUARDIAN_API_KEY")
        }
    }

    /// A single test because `SECRET_REPO` is a process-global `OnceLock`; no other test in
    /// the crate may call `init_secret_deps`.
    #[tokio::test]
    async fn an_uninstalled_store_reads_as_unset_and_an_installed_one_reads_through() {
        assert!(
            secret("GUARDIAN_API_KEY").await.is_none(),
            "with no store installed a tool must see no key, so it takes its keyless fallback"
        );

        init_secret_deps(Arc::new(OneKey));

        assert_eq!(
            secret("GUARDIAN_API_KEY").await.as_deref(),
            Some("guardian-live-key")
        );
        assert!(secret("GNEWS_API_KEY").await.is_none());
    }

    /// A turn's tools, as the goose adapter's allow-set holds them.
    const TURN: &[&str] = &[
        "giap-memory__recall_memories",
        "giap-knowledge__get_wikipedia_article",
        "giap-knowledge__search_web",
    ];

    #[tokio::test]
    async fn the_web_search_tool_follows_its_key_turn_by_turn() {
        let store: std::sync::Mutex<std::collections::HashMap<&str, String>> = Default::default();
        let lookup = |key: &'static str| {
            let value = store.lock().unwrap().get(key).cloned();
            async move { value }
        };

        let withheld = withheld_by(TURN.iter().copied(), &lookup).await;
        assert_eq!(
            withheld,
            HashSet::from(["giap-knowledge__search_web".to_string()]),
            "no key, no tool"
        );

        store
            .lock()
            .unwrap()
            .insert("BRAVE_SEARCH_API_KEY", "a-key".to_string());
        assert!(
            withheld_by(TURN.iter().copied(), &lookup).await.is_empty(),
            "a key, the tool"
        );

        store.lock().unwrap().remove("BRAVE_SEARCH_API_KEY");
        assert_eq!(
            withheld_by(TURN.iter().copied(), &lookup).await,
            HashSet::from(["giap-knowledge__search_web".to_string()]),
            "key removed, tool gone"
        );

        store
            .lock()
            .unwrap()
            .insert("BRAVE_SEARCH_API_KEY", "   ".to_string());
        assert_eq!(
            withheld_by(TURN.iter().copied(), &lookup).await.len(),
            1,
            "a blank key is no key"
        );
    }

    #[tokio::test]
    async fn compute_answer_is_withheld_without_the_wolfram_app_id() {
        let turn = [
            "giap-knowledge__compute_answer",
            "giap-knowledge__search_web",
            "giap-knowledge__get_wikipedia_article",
        ];
        let only_brave = |key: &'static str| {
            let value = (key == "BRAVE_SEARCH_API_KEY").then(|| "a-key".to_string());
            async move { value }
        };
        assert_eq!(
            withheld_by(turn, only_brave).await,
            HashSet::from(["giap-knowledge__compute_answer".to_string()])
        );
        let both = |_key: &'static str| async { Some("a-key".to_string()) };
        assert!(withheld_by(turn, both).await.is_empty());
    }

    #[tokio::test]
    async fn a_turn_without_the_tool_reads_no_secret() {
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let lookup = |_key: &'static str| {
            reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { None }
        };
        let withheld = withheld_by(["giap-memory__recall_memories"], lookup).await;
        assert!(withheld.is_empty());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
