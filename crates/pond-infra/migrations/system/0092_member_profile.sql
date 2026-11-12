-- What each memory says about its member (a closed list), and when a passing fact stops being
-- shown. Rows filed before this have no `about`; the re-sort job reads them.
ALTER TABLE memory_fragments ADD COLUMN about TEXT;
ALTER TABLE memory_fragments ADD COLUMN valid_until TEXT;
-- Times the re-sort asked what an older row is about and got no usable answer.
ALTER TABLE memory_fragments ADD COLUMN resort_attempts INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_memory_fragments_about
    ON memory_fragments(profile_id, about);

-- The two segments that can only be about one thing need no model to file them.
UPDATE memory_fragments SET about = 'routine' WHERE about IS NULL AND segment = 'routine';
UPDATE memory_fragments SET about = 'goal' WHERE about IS NULL AND segment = 'project';
