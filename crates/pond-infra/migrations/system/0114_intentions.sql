-- The heartbeat's own jobs: do one thing once a condition holds, until a deadline.
-- `condition_json` is the kind's spec; `condition_label` is the sentence a member reads.
CREATE TABLE IF NOT EXISTS intentions (
    id              TEXT PRIMARY KEY,
    profile_id      TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    act             TEXT NOT NULL CHECK (act IN ('push','refresh','look_into','ask','propose_todo')),
    text            TEXT NOT NULL CHECK (length(trim(text)) > 0),
    text_key        TEXT NOT NULL,
    why             TEXT NOT NULL DEFAULT '',
    condition_kind  TEXT NOT NULL CHECK (condition_kind IN ('time','place','thing','chat')),
    condition_json  TEXT NOT NULL,
    condition_label TEXT NOT NULL,
    anchor_id       TEXT REFERENCES anchors(id) ON DELETE CASCADE,
    until           TEXT NOT NULL,
    state           TEXT NOT NULL DEFAULT 'waiting' CHECK (state IN ('waiting','done','expired','dropped')),
    outcome         TEXT,
    created_at      TEXT NOT NULL,
    settled_at      TEXT
);
CREATE INDEX IF NOT EXISTS idx_intentions_waiting ON intentions(state, condition_kind, until);
