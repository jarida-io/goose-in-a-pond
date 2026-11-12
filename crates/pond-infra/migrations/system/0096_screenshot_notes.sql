-- One line about a screenshot a member took, kept only for members who turned this on. The
-- picture itself is never stored: it is read once from memory and dropped.
CREATE TABLE IF NOT EXISTS screenshot_notes (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    note        TEXT NOT NULL,
    taken_at    TEXT,
    created_at  TEXT NOT NULL,
    used_at     TEXT
);
CREATE INDEX IF NOT EXISTS idx_screenshot_notes_member
    ON screenshot_notes (profile_id, created_at);
