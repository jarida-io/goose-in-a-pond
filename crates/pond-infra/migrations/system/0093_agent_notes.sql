-- What Goose writes down for itself: lessons about how to work and follow-ups to come back to.
-- `profile_id` NULL is the household's; a member's go with the member.
CREATE TABLE IF NOT EXISTS agent_notes (
    id         TEXT PRIMARY KEY,
    profile_id TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('lesson', 'note')),
    text       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_notes_owner
    ON agent_notes(profile_id, kind, created_at);

-- The scratchpad was seeded under the name `memory`, which is what members' memories are
-- called. An edited one keeps its words under the new name; an unedited one is reseeded.
UPDATE prompt_templates SET name = 'scratchpad'
    WHERE name = 'memory' AND is_customized = 1
      AND NOT EXISTS (SELECT 1 FROM prompt_templates WHERE name = 'scratchpad');
DELETE FROM prompt_templates WHERE name = 'memory';
