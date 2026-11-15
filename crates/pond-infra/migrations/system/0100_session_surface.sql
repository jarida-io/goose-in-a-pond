-- Where a conversation was started (phone, desktop, cli, pond, room, schedule, goose) and the
-- paired device that began it, so each screen can list its own. NULL: begun before this existed.
ALTER TABLE sessions ADD COLUMN surface TEXT;
ALTER TABLE sessions ADD COLUMN surface_device_id TEXT;
UPDATE sessions SET surface = 'schedule' WHERE id LIKE 'sched-%';
