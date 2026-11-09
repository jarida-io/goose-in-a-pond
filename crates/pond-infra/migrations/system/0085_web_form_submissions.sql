-- POST forms waiting for, or answered by, the member's yes on their phone. `fields` is a JSON list
-- of [name, value] pairs; the result columns are set once the form is sent.
CREATE TABLE IF NOT EXISTS web_form_submissions (
    id             TEXT PRIMARY KEY,
    profile_id     TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    page           TEXT NOT NULL,
    action         TEXT NOT NULL,
    fields         TEXT NOT NULL,
    depth          INTEGER NOT NULL DEFAULT 0,
    session_id     TEXT,
    state          TEXT NOT NULL,
    result_status  INTEGER,
    result_address TEXT,
    result_title   TEXT,
    failure        TEXT,
    asked_at       TEXT NOT NULL,
    decided_at     TEXT
);
CREATE INDEX IF NOT EXISTS idx_web_form_submissions_profile
    ON web_form_submissions(profile_id, asked_at);
