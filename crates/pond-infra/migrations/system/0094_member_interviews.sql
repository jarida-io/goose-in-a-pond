-- The one structured interview, one row a member: its answers so far and the draft it wrote.
-- `updated_at` lets a read recover a write-up whose writer never came back.
CREATE TABLE IF NOT EXISTS member_interviews (
    profile_id  TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    status      TEXT NOT NULL DEFAULT 'not_started'
                CHECK (status IN ('not_started', 'asking', 'thinking', 'review', 'done')),
    body_json   TEXT NOT NULL DEFAULT '{}',
    updated_at  TEXT NOT NULL
);
