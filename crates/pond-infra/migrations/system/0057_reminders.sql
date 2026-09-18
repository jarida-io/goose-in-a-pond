-- Where a dated utterance actually lands.
--
-- The extraction gate refuses any memory whose note carries a one-off calendar
-- date, and that refusal was justified on the grounds that the date "goes
-- somewhere" instead. It did not. `capture_reminder` incremented a counter and
-- emitted a DEBUG line; `ReminderCandidate` was constructed, counted and
-- dropped, so on a pond whose model files a reminder for every dated note the
-- date was still gone every single time. This table is that somewhere.
--
-- ── Why a table rather than a proposal ─────────────────────────────────────
--
-- The proposal queue is the DESTINATION, and it is not reachable today:
-- `ProposalAudience::from_scope` correctly refuses Household and Guest (PAI-7
-- invariant 4), and `profile_id` is NULL on every live memory row, so a
-- reminder on a real pond has no audience to be addressed to. A design that
-- only made proposals would therefore keep losing the date on exactly the pond
-- it was written to fix. The row lands unconditionally; the proposal is what a
-- later phase makes out of it, and `disposition` is where it records having
-- done so.
--
-- ── Against a database that already has rows ───────────────────────────────
--
-- A new table. Every existing pond gets an empty one. No backfill is possible:
-- the dates this table exists to keep were discarded before they reached any
-- store, so there is nothing anywhere to recover them from, and the honest
-- starting state is empty.
--
-- ── `when_said` is words, never a date ─────────────────────────────────────
--
-- TEXT holding the subject's own words about the timing -- "next Tuesday",
-- "after the rains" -- copied out of the conversation and never parsed. Nothing
-- in this pipeline knows which Tuesday was meant, and a wrong date in a
-- reminder is worse than no reminder: it fires on the wrong day, about
-- something already done, and teaches the household that the pond's reminders
-- cannot be trusted. Whoever reads the row decides what it means. There is
-- deliberately no `due_at` column, because a column like that would be filled
-- by a guess.

CREATE TABLE IF NOT EXISTS reminders (
    id           TEXT PRIMARY KEY,

    -- What it is about, with the date taken out of it: "the dentist".
    about        TEXT NOT NULL,
    -- The subject's own words about the timing. Never parsed. See above.
    when_said    TEXT NOT NULL,

    -- The dedup key's second half: `about`, lowercased and stripped to its
    -- words. Stored as a column rather than computed in the query because it is
    -- half of a UNIQUE constraint, and SQLite can only enforce a constraint over
    -- something it can see. `reminder_dedup_key` in pond-core is the one
    -- producer; a row written by anything else still cannot collide with itself.
    about_key    TEXT NOT NULL,

    -- ── Provenance: where did this come from ──────────────────────────────
    --
    -- The household must be able to ask that and get an answer, so both halves
    -- are NOT NULL: the conversation, and the exact stretch of it the walk had
    -- read when this was lifted out. `window_id` is the same value the
    -- extraction cursor advances to, so a reminder can always be traced back to
    -- the messages that produced it.
    --
    -- No foreign key on `session_id`, and that is a decision rather than an
    -- omission. A reminder outlives the conversation that produced it -- "the
    -- dentist, next Tuesday" is still true after the chat is deleted -- so
    -- CASCADE would throw away the thing this table exists to keep, and SET NULL
    -- would strip the provenance to keep the row, leaving one that can never be
    -- explained. The id may therefore name a conversation that is gone, which is
    -- the readable failure of the three.
    session_id   TEXT NOT NULL,
    window_id    TEXT NOT NULL,

    -- Whose reminder this is, by name. On a multi-member pond this is the
    -- difference between a useful suggestion and one shown to the wrong person,
    -- and it is available even when `profile_id` is not.
    subject      TEXT NOT NULL,
    -- The member it belongs to, when the window's subject was one. NULL on every
    -- pond with no profile rows, which is every pond today -- and the reason the
    -- proposal step cannot be the only destination.
    profile_id   TEXT,

    -- When the CONVERSATION happened -- the window's last message, not the wall
    -- clock. The staleness question is asked about the conversation: a first run
    -- over a year of history reads chats from last March at three in the morning
    -- tonight, and a row stamped `now` would look fresh.
    said_at      TEXT NOT NULL,
    -- When this pond lifted it out. The two differ by the whole length of a
    -- backlog walk, and both are needed to say why a row is here.
    captured_at  TEXT NOT NULL,

    -- pending | proposed | dismissed | expired.
    --
    -- What lets a reminder be acted on rather than merely accumulate. 'pending'
    -- is a row nothing has done anything with yet; 'proposed' is the phase that
    -- builds the queue path recording that it made a proposal out of this row,
    -- so it cannot make a second one; 'dismissed' and 'expired' are the two ways
    -- a row stops being live -- one because somebody said so, one because time
    -- passed.
    disposition  TEXT NOT NULL DEFAULT 'pending',
    -- When the disposition last moved off 'pending'. NULL while it has not.
    decided_at   TEXT,

    -- ── The dedup key ─────────────────────────────────────────────────────
    --
    -- The engine re-walks: a watermark naming a deleted message clears the
    -- cursor and the conversation is read again from its first message. The
    -- window-level guard (`already_mined`) does not cover this, and cannot --
    -- it only fires for a window that WROTE a memory, and the windows that
    -- matter most here are precisely the ones whose only yield was a reminder
    -- the date rule refused to store as a memory.
    --
    -- So the guard is here, at the layer a second pass cannot walk past.
    -- (window_id, about_key): the window because it is the unit the model
    -- answered in and the same unit `already_mined` keys on, `about` because one
    -- window can legitimately produce two different reminders and a window-only
    -- key would silently keep the first and drop the second.
    --
    -- `when_said` is deliberately NOT in the key. The same appointment described
    -- twice with different timing words -- "Tuesday", "next Tuesday" -- is one
    -- reminder, and keying on the timing would let a re-walk file both.
    UNIQUE (window_id, about_key),

    CHECK (trim(about) <> ''),
    CHECK (trim(when_said) <> ''),
    CHECK (trim(about_key) <> ''),
    CHECK (disposition IN ('pending', 'proposed', 'dismissed', 'expired'))
);

-- The read every consumer makes: what is still live, most recently said first.
-- The proposal phase asks it, and so does any surface that shows the household
-- what the pond is holding.
CREATE INDEX IF NOT EXISTS idx_reminders_disposition_said
    ON reminders(disposition, said_at DESC);

-- "Where did this come from", asked the other way round: every reminder this
-- conversation produced.
CREATE INDEX IF NOT EXISTS idx_reminders_session
    ON reminders(session_id);

-- Deleting a household member must not leave their reminders live.
--
-- The same shape and the same reasoning as 0038's trigger on `drafts`, down to
-- the ordering: dispose first, then release, or the second UPDATE hides the
-- rows from the first. `profile_id` carries no foreign key here either, so this
-- is the semantics and not a missing ON DELETE action -- a departed member's
-- pending reminder is precisely the thing that must not outlive them, while the
-- row itself is kept, unowned, because the conversation it came from is still
-- part of the household's history.
CREATE TRIGGER IF NOT EXISTS trg_profiles_delete_expires_reminders
BEFORE DELETE ON profiles
BEGIN
    UPDATE reminders
       SET disposition = 'expired',
           decided_at  = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
     WHERE profile_id = OLD.id AND disposition = 'pending';
    UPDATE reminders
       SET profile_id = NULL
     WHERE profile_id = OLD.id;
END;
