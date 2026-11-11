-- A dated mention is offered to its member as a to-do: 'offered' while the phone waits for Add or
-- No, 'accepted' once it became one. `offered_at` is when the offer went out, which the daily cap
-- on what Goose puts in front of a member counts beside its proposals.
--
-- SQLite cannot widen a CHECK in place, so the table is rebuilt with every row kept. The member
-- trigger names the table in its body and is dropped first, or the rename is refused for a
-- trigger pointing at a table that is momentarily gone.

DROP TRIGGER IF EXISTS trg_profiles_delete_expires_reminders;

CREATE TABLE reminders_rebuilt (
    id           TEXT PRIMARY KEY,
    about        TEXT NOT NULL,
    when_said    TEXT NOT NULL,
    about_key    TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    window_id    TEXT NOT NULL,
    subject      TEXT NOT NULL,
    profile_id   TEXT,
    said_at      TEXT NOT NULL,
    captured_at  TEXT NOT NULL,
    disposition  TEXT NOT NULL DEFAULT 'pending',
    decided_at   TEXT,
    offered_at   TEXT,

    UNIQUE (window_id, about_key),

    CHECK (trim(about) <> ''),
    CHECK (trim(when_said) <> ''),
    CHECK (trim(about_key) <> ''),
    CHECK (disposition IN ('pending', 'proposed', 'offered', 'accepted', 'dismissed', 'expired'))
);

INSERT INTO reminders_rebuilt (
    id, about, when_said, about_key, session_id, window_id, subject, profile_id,
    said_at, captured_at, disposition, decided_at
)
SELECT id, about, when_said, about_key, session_id, window_id, subject, profile_id,
       said_at, captured_at, disposition, decided_at
  FROM reminders;

DROP TABLE reminders;
ALTER TABLE reminders_rebuilt RENAME TO reminders;

CREATE INDEX IF NOT EXISTS idx_reminders_disposition_said
    ON reminders(disposition, said_at DESC);
CREATE INDEX IF NOT EXISTS idx_reminders_session
    ON reminders(session_id);
CREATE INDEX IF NOT EXISTS idx_reminders_offered
    ON reminders(profile_id, offered_at);

-- As 0057: a departed member's live reminders, offered ones included, expire before the rows
-- are released.
CREATE TRIGGER IF NOT EXISTS trg_profiles_delete_expires_reminders
BEFORE DELETE ON profiles
BEGIN
    UPDATE reminders
       SET disposition = 'expired',
           decided_at  = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
     WHERE profile_id = OLD.id AND disposition IN ('pending', 'offered');
    UPDATE reminders
       SET profile_id = NULL
     WHERE profile_id = OLD.id;
END;
