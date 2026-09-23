//! Compile-time port assignments for all GIAP services.
//!
//! **All port numbers live here and nowhere else.**  Edit these constants and
//! recompile to reassign ports across the entire server — no flags, no config
//! files at runtime, no user-facing port management.
//!
//! If a service cannot bind its configured port, GIAP automatically tries the
//! next port in arithmetic sequence (`base`, `base+1`, `base+2`, …) up to
//! [`MAX_TRIES`] attempts.  The process that eventually binds tells callers
//! what port was actually used so dependent services can connect to the right
//! address.

/// Local REST API and dashboard (loopback only).
pub const API_SERVER: u16 = 4000;

/// Pinned HTTPS companion API on LAN and tailnet.
pub const HTTPS_SERVER: u16 = 4443;

/// llamafile LLM subprocess (loopback only).
pub const LLAMAFILE: u16 = 8080;

/// The effective llamafile / llama-server base port.
///
/// `GIAP_LLAMAFILE_PORT` overrides the [`LLAMAFILE`] default. This matters when
/// pond-server itself is bound to 8080 (its second-choice serve port): the
/// llamafile fast-path probe ("something is answering on the port") would then
/// hit pond-server's own dashboard and wire the LLM provider back at itself.
/// The override also lets an externally managed OpenAI-compatible server
/// (e.g. llama.cpp's `llama-server` on the Jetson) own the port — the provider
/// path simply uses whatever answers there.
pub fn llamafile_port() -> u16 {
    std::env::var("GIAP_LLAMAFILE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(LLAMAFILE)
}

/// How many sequential port numbers to try before giving up.
pub const MAX_TRIES: u16 = 10;

// ─────────────────────────────────────────────────────────────────────────────
// Utilities
// ─────────────────────────────────────────────────────────────────────────────

/// Bind a TCP listener on `host`, trying ports `start`, `start+1`, … up to
/// [`MAX_TRIES`] attempts.  Returns the bound listener and the actual port.
///
/// Use this for services **owned by GIAP** (API server, piper-http bridge)
/// where we hold the socket ourselves.
pub async fn bind_with_fallback(
    host: &str,
    start: u16,
) -> anyhow::Result<(tokio::net::TcpListener, u16)> {
    for offset in 0..MAX_TRIES {
        if let Some(port) = start.checked_add(offset) {
            let addr = format!("{host}:{port}");
            if let Ok(listener) = tokio::net::TcpListener::bind(&addr).await {
                if offset > 0 {
                    tracing::info!("port {} in use — bound on port {}", start, port);
                }
                return Ok((listener, port));
            }
        }
    }
    anyhow::bail!(
        "no available port in {}..{} on {}",
        start,
        start + MAX_TRIES - 1,
        host
    )
}

/// Find a free port for an **external child process** without holding the
/// socket (the child will bind it moments later).
///
/// Always probes `127.0.0.1`.  There is a small TOCTOU window, but on a
/// single-user embedded device this is acceptable.  Returns `None` if no port
/// is free within [`MAX_TRIES`] of `start`.
pub async fn find_free_port(start: u16) -> Option<u16> {
    for offset in 0..MAX_TRIES {
        if let Some(port) = start.checked_add(offset) {
            if tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
                .await
                .is_ok()
            {
                return Some(port);
            }
        }
    }
    None
}
