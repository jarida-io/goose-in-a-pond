-- What became of each queued notice. `delivered_at` stays the phone's acknowledgement; these say
-- whether a push wake was sent, when the phone first got the copy, whether it showed it, whether
-- the person opened it, and whether the pond said it aloud because no phone came for it.
ALTER TABLE notifications ADD COLUMN wakes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE notifications ADD COLUMN woken_at TEXT;
ALTER TABLE notifications ADD COLUMN fetched_at TEXT;
ALTER TABLE notifications ADD COLUMN shown_at TEXT;
ALTER TABLE notifications ADD COLUMN opened_at TEXT;
ALTER TABLE notifications ADD COLUMN spoken_at TEXT;

-- A notice acknowledged before this existed did reach its phone.
UPDATE notifications SET fetched_at = delivered_at WHERE delivered_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_notifications_unfetched
    ON notifications (fetched_at, created_at);
