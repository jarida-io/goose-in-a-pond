-- turn_metrics: per-turn telemetry, persisted so it survives a restart.

CREATE TABLE IF NOT EXISTS turn_metrics (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id               TEXT    NOT NULL,
    turn_number               INTEGER NOT NULL,
    prompt_tokens             INTEGER NOT NULL,
    completion_tokens         INTEGER NOT NULL,
    ttft_ms                   INTEGER NOT NULL,
    total_latency_ms          INTEGER NOT NULL,
    tool_name                 TEXT,
    tool_latency_ms           INTEGER,
    tool_cache_hit            INTEGER,
    context_utilization_pct   REAL    NOT NULL,
    model_name                TEXT    NOT NULL,
    timestamp                 TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_turn_metrics_session
    ON turn_metrics(session_id, turn_number);
