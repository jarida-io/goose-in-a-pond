-- A notice queued in quiet hours that is neither an alert nor timed for then. A phone is not
-- woken for it and does not see it until quiet hours end, when the held notices of each phone
-- become one.
ALTER TABLE notifications ADD COLUMN held INTEGER NOT NULL DEFAULT 0;
