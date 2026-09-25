//! Every GIAP port number lives here. Binding falls back to `base+1`, … up to [`MAX_TRIES`].

/// GIAP REST API + web dashboard (all interfaces, 0.0.0.0).
pub const API_SERVER: u16 = 4000;

/// llamafile LLM subprocess (loopback only).
pub const LLAMAFILE: u16 = 8080;

/// `GIAP_LLAMAFILE_PORT` or [`LLAMAFILE`]. Override when pond-server holds 8080 (the probe
/// would wire the LLM back at itself) or an external llama-server owns the port.
pub fn llamafile_port() -> u16 {
    std::env::var("GIAP_LLAMAFILE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(LLAMAFILE)
}

/// How many sequential port numbers to try before giving up.
pub const MAX_TRIES: u16 = 10;

// ── Utilities ────────────────────────────────────────────────────────────────

/// Bind the first free port from `start` (up to [`MAX_TRIES`]); for sockets GIAP holds itself.
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

/// Free loopback port for a child to bind; not held, so a small TOCTOU race is accepted.
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
