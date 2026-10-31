-- Each member's setup interview: the chat session it runs in, how far it got, and the notes it
-- drafted for approval. Topic answers are cleared when it ends. Timestamps are RFC 3339 from Rust.
CREATE TABLE IF NOT EXISTS profile_interviews (
    profile_id      TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    session_id      TEXT NOT NULL,
    status          TEXT NOT NULL
        CHECK (status IN ('in_progress', 'awaiting_approval', 'done', 'stopped')),
    topics          TEXT NOT NULL,
    draft           TEXT NOT NULL DEFAULT '[]',
    pending_opening TEXT,
    drafted_at      TEXT,
    last_reply_at   TEXT,
    started_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_profile_interviews_session ON profile_interviews (session_id);
