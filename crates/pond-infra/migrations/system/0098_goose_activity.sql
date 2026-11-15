-- What Goose did for a member without being asked: a reminder set, a note written, a failing
-- watch paused, something looked up. Written after the thing is done, so the member can read
-- it and take it back.
CREATE TABLE IF NOT EXISTS goose_activity (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    summary     TEXT NOT NULL,
    detail      TEXT,
    ref_id      TEXT,
    at          TEXT NOT NULL,
    undone_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_goose_activity_member ON goose_activity (profile_id, at);
