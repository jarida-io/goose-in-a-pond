-- How far batch memory extraction has read into each conversation.
--
-- The batch engine walks one message window of one chat per lane slot, so it
-- needs a per-session watermark that survives a restart. This copies the shape
-- of 0030's rolling summary exactly: an id naming the newest message the walk
-- has covered, a stamp saying when it last looked, and -- new here -- a count
-- of consecutive attempts that produced nothing parseable.
--
-- `extracted_through_id` is deliberately NOT a foreign key. A message can be
-- deleted (history truncation, an edited turn), and the engine has a defined
-- answer for an anchor that is gone: clear the cursor and re-walk the chat from
-- the beginning. A constraint here would turn that recoverable state into a
-- write error on a path that has no user to report it to.
--
-- `extraction_attempts` counts attempts against the CURRENT watermark, not
-- against the session. It exists so a model that never emits parseable JSON
-- cannot wedge one conversation forever: three strikes and the walk moves on,
-- recording that it did. Advancing the cursor resets it to zero.
--
-- NULL on every existing row, and that is the correct starting state: NULL
-- means "never examined", which is exactly true of every conversation in every
-- pond that upgrades into this release, and it sorts to the front of the
-- backlog so the history actually gets read.

ALTER TABLE sessions ADD COLUMN extracted_through_id TEXT;
ALTER TABLE sessions ADD COLUMN extracted_at         TEXT;
ALTER TABLE sessions ADD COLUMN extraction_attempts  INTEGER NOT NULL DEFAULT 0;
