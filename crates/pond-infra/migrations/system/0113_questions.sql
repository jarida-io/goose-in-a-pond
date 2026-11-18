-- Questions Goose asks a member, four to a batch. A question is asked of a member once: its key
-- stays after it is answered or skipped.
CREATE TABLE IF NOT EXISTS questions (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    batch_id     TEXT NOT NULL,
    purpose      TEXT NOT NULL CHECK (purpose IN ('anchor','interest','profile','ledger')),
    anchor_id    TEXT REFERENCES anchors(id) ON DELETE CASCADE,
    subject_ref  TEXT,
    text         TEXT NOT NULL CHECK (length(trim(text)) > 0),
    text_key     TEXT NOT NULL,
    options_json TEXT NOT NULL DEFAULT '[]',
    multi        INTEGER NOT NULL DEFAULT 0,
    parent_id    TEXT,
    state        TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open','answered','skipped','expired')),
    choices_json TEXT NOT NULL DEFAULT '[]',
    other_text   TEXT,
    asked_at     TEXT,
    answered_at  TEXT,
    expires_at   TEXT,
    created_at   TEXT NOT NULL,
    UNIQUE (profile_id, text_key)
);
CREATE INDEX IF NOT EXISTS idx_questions_member ON questions(profile_id, state, batch_id);
