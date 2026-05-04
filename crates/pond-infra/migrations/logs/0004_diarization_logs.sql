-- Diarization log: one row per speaker identification event.
--
-- profile_id is NULL when the speaker is unknown (no match above threshold).
-- Unknown speaker events are NOT errors — the session proceeds without attribution.
-- TTL: 30 days (pruned by the background pruning job).

CREATE TABLE IF NOT EXISTS diarization_logs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT,               -- NULL outside of an active session
    profile_id  TEXT,               -- NULL if speaker is unknown
    confidence  REAL,               -- cosine similarity score, NULL if no match attempted
    model       TEXT    NOT NULL,   -- 'resemblyzer' | 'x-vector'
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_diarization_logs_session_id
    ON diarization_logs(session_id);

CREATE INDEX IF NOT EXISTS idx_diarization_logs_profile_id
    ON diarization_logs(profile_id);

CREATE INDEX IF NOT EXISTS idx_diarization_logs_created_at
    ON diarization_logs(created_at);
