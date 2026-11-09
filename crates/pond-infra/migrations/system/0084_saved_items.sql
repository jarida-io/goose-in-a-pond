-- Cards a member kept from the assistant's work. `fields` and `sources` are JSON; `kind` repeats
-- the fields' kind so a list can filter on it.
CREATE TABLE IF NOT EXISTS saved_items (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    title       TEXT NOT NULL,
    fields      TEXT NOT NULL,
    body        TEXT NOT NULL DEFAULT '',
    collection  TEXT,
    shared      INTEGER NOT NULL DEFAULT 0,
    sources     TEXT NOT NULL DEFAULT '[]',
    session_id  TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_saved_items_profile ON saved_items(profile_id, updated_at);
CREATE INDEX IF NOT EXISTS idx_saved_items_shared ON saved_items(shared);
