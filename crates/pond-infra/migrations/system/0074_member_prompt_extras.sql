-- A prompt extra may be one member's own: NULL is the household's, read on every turn, and a
-- member's is read only on that member's turns. A key is unique per owner, so the table is rebuilt.
CREATE TABLE prompt_extras_by_owner (
    key         TEXT NOT NULL,
    instruction TEXT NOT NULL,
    active      INTEGER NOT NULL DEFAULT 1,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    profile_id  TEXT REFERENCES profiles(id) ON DELETE CASCADE
);
INSERT INTO prompt_extras_by_owner (key, instruction, active, sort_order, updated_at)
    SELECT key, instruction, active, sort_order, updated_at FROM prompt_extras;
DROP TABLE prompt_extras;
ALTER TABLE prompt_extras_by_owner RENAME TO prompt_extras;
CREATE UNIQUE INDEX idx_prompt_extras_owner_key ON prompt_extras (COALESCE(profile_id, ''), key);
