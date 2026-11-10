-- Pages a member asked the pond to keep checking. `filters` and `items` are JSON; `items` is NULL
-- before the first good check. 0082 is left to the context space's items table.
CREATE TABLE IF NOT EXISTS web_watches (
    id              TEXT PRIMARY KEY,
    profile_id      TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    address         TEXT NOT NULL,
    title           TEXT NOT NULL,
    looking_for     TEXT NOT NULL DEFAULT '',
    filters         TEXT NOT NULL DEFAULT '{}',
    interval_hours  INTEGER NOT NULL DEFAULT 6,
    paused          INTEGER NOT NULL DEFAULT 0,
    items           TEXT,
    failures        INTEGER NOT NULL DEFAULT 0,
    last_checked_at TEXT,
    next_check_at   TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    session_id      TEXT
);
CREATE INDEX IF NOT EXISTS idx_web_watches_profile ON web_watches(profile_id);
CREATE INDEX IF NOT EXISTS idx_web_watches_due ON web_watches(paused, next_check_at);

-- What each check found. A matched change was pushed when found; the rest wait for the digest.
CREATE TABLE IF NOT EXISTS web_watch_changes (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    watch_id  TEXT NOT NULL REFERENCES web_watches(id) ON DELETE CASCADE,
    found_at  TEXT NOT NULL,
    change    TEXT NOT NULL,
    matched   INTEGER NOT NULL,
    digested  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_web_watch_changes_watch ON web_watch_changes(watch_id, found_at);
