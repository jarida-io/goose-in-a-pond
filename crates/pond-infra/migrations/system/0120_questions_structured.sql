-- Structured questions: a header chip, options with descriptions, a recommended pick, the plan
-- gap a question settles, the chat that asked it and its round in a chain. Adds the `chat`
-- purpose and the `waiting` state. SQLite cannot widen a CHECK in place, so the table is rebuilt
-- with every row kept; `options_json` stays, and a plain string in it reads as `{label}`.
CREATE TABLE questions_rebuilt (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    batch_id     TEXT NOT NULL,
    purpose      TEXT NOT NULL CHECK (purpose IN ('anchor','interest','profile','ledger','chat')),
    anchor_id    TEXT REFERENCES anchors(id) ON DELETE CASCADE,
    subject_ref  TEXT,
    header       TEXT NOT NULL DEFAULT '',
    text         TEXT NOT NULL CHECK (length(trim(text)) > 0),
    text_key     TEXT NOT NULL,
    options_json TEXT NOT NULL DEFAULT '[]',
    recommended  INTEGER,
    multi        INTEGER NOT NULL DEFAULT 0,
    gap          TEXT,
    session_id   TEXT,
    round        INTEGER NOT NULL DEFAULT 1,
    parent_id    TEXT,
    state        TEXT NOT NULL DEFAULT 'open'
                 CHECK (state IN ('open','waiting','answered','skipped','expired')),
    choices_json TEXT NOT NULL DEFAULT '[]',
    other_text   TEXT,
    asked_at     TEXT,
    answered_at  TEXT,
    expires_at   TEXT,
    created_at   TEXT NOT NULL,
    UNIQUE (profile_id, text_key)
);

INSERT INTO questions_rebuilt (id, profile_id, batch_id, purpose, anchor_id, subject_ref, text,
                               text_key, options_json, multi, parent_id, state, choices_json,
                               other_text, asked_at, answered_at, expires_at, created_at)
SELECT id, profile_id, batch_id, purpose, anchor_id, subject_ref, text, text_key, options_json,
       multi, parent_id, state, choices_json, other_text, asked_at, answered_at, expires_at,
       created_at
  FROM questions;

DROP TABLE questions;
ALTER TABLE questions_rebuilt RENAME TO questions;

CREATE INDEX IF NOT EXISTS idx_questions_member ON questions(profile_id, state, batch_id);
CREATE INDEX IF NOT EXISTS idx_questions_session ON questions(session_id) WHERE session_id IS NOT NULL;
