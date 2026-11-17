-- A to-do's list, priority, steps, what Goose can do for it and the anchor it serves.
ALTER TABLE todos ADD COLUMN list TEXT;
ALTER TABLE todos ADD COLUMN priority TEXT NOT NULL DEFAULT 'normal';
ALTER TABLE todos ADD COLUMN steps_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE todos ADD COLUMN goose_can_json TEXT;
ALTER TABLE todos ADD COLUMN anchor_id TEXT;
