//! Shared HTTP client for the Knowledge-family MCP servers. Every outbound request MUST go
//! through [`traced_get`] / [`traced_get_with`], which gate it and record it as egress.

use reqwest::Client;
use std::time::Duration;

use pond_core::shared::services::egress;

const USER_AGENT: &str =
    "goose-in-a-pond/0.1 (GIAP MCP; https://github.com/jarida-io/goose-in-a-pond)";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

pub fn build_http_client() -> Client {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(DEFAULT_TIMEOUT)
        .pool_max_idle_per_host(4)
        .build()
        .expect("Failed to build HTTP client")
}

/// Traced `GET`. Returns `anyhow::Result` because the network gate can refuse before a socket
/// opens, and a refusal is not a `reqwest::Error`.
pub async fn traced_get(client: &Client, url: &str) -> anyhow::Result<reqwest::Response> {
    traced_get_with(client, url, |b| b).await
}

/// [`traced_get`] with a hook to customise the request builder (headers, timeout, query).
pub async fn traced_get_with<F>(
    client: &Client,
    url: &str,
    customize: F,
) -> anyhow::Result<reqwest::Response>
where
    F: FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
{
    let builder = customize(client.get(url));
    send_traced(builder, "GET", url).await
}

/// Send a prepared request builder, recording the egress regardless of outcome.
async fn send_traced(
    builder: reqwest::RequestBuilder,
    method: &str,
    url: &str,
) -> anyhow::Result<reqwest::Response> {
    // Gate BEFORE sending: a refusal never opens a socket and is recorded as `egress.denied`.
    egress::check_egress(url)?;

    let host = egress::extract_host(url);
    let tool = egress::current_tool();
    let session_id = egress::current_session_id();

    let start = std::time::Instant::now();
    let result = builder.send().await;
    let latency_ms = start.elapsed().as_millis() as u64;
    let status = result.as_ref().ok().map(|r| r.status().as_u16());

    match &result {
        Ok(resp) => tracing::info!(
            target: "giap::trace",
            kind = "mcp_http",
            session_id = %session_id,
            tool = %tool,
            host = %host,
            status = resp.status().as_u16(),
            latency_ms,
        ),
        Err(e) => tracing::warn!(
            target: "giap::trace",
            kind = "mcp_http_error",
            session_id = %session_id,
            tool = %tool,
            host = %host,
            error = %e,
            latency_ms,
        ),
    }

    // Durable, queryable egress record.
    egress::record_egress(url, method, status, latency_ms);

    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_builds_without_panic() {
        let _client = build_http_client();
    }
}
