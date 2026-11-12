-- A change on a watch set up in its owner's words alone waits here until the model has said
-- whether it is what they asked for.
ALTER TABLE web_watch_changes ADD COLUMN held INTEGER NOT NULL DEFAULT 0;
