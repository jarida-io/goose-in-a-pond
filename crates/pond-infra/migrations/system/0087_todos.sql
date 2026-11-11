-- A member's to-dos and reminders. A reminder tells them at its hook; a to-do also has Goose ask
-- afterwards whether it was done (`ask`), and the answer is kept on the row.
CREATE TABLE IF NOT EXISTS todos (
    id             TEXT PRIMARY KEY,
    profile_id     TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    kind           TEXT NOT NULL DEFAULT 'todo' CHECK (kind IN ('todo', 'reminder')),
    text           TEXT NOT NULL CHECK (length(trim(text)) > 0),
    notes          TEXT,
    state          TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'done', 'dropped')),
    due_at         TEXT,
    ask            INTEGER NOT NULL DEFAULT 1,
    source         TEXT NOT NULL DEFAULT 'you' CHECK (source IN ('you', 'chat', 'goose', 'idea')),
    source_ref     TEXT,
    last_asked_at  TEXT,
    asked          INTEGER NOT NULL DEFAULT 0,
    snoozed_until  TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    done_at        TEXT
);
CREATE INDEX IF NOT EXISTS idx_todos_profile ON todos(profile_id, state, due_at);

-- What fires a to-do: a time, a repeat, a place, something the Pond notices, or a connector's
-- event. `spec` is the kind's JSON; `label` is the sentence the phone shows for it.
CREATE TABLE IF NOT EXISTS todo_hooks (
    id             TEXT PRIMARY KEY,
    todo_id        TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
    kind           TEXT NOT NULL CHECK (kind IN ('time', 'repeat', 'place', 'event', 'connector')),
    spec           TEXT NOT NULL DEFAULT '{}',
    label          TEXT NOT NULL,
    next_fire_at   TEXT,
    last_fired_at  TEXT,
    created_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_todo_hooks_todo ON todo_hooks(todo_id);
CREATE INDEX IF NOT EXISTS idx_todo_hooks_due ON todo_hooks(next_fire_at);

-- Places a member named, which their phone fences and reports arriving at or leaving.
CREATE TABLE IF NOT EXISTS member_places (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    label       TEXT NOT NULL CHECK (length(trim(label)) > 0),
    lat         REAL NOT NULL,
    lng         REAL NOT NULL,
    radius_m    REAL NOT NULL DEFAULT 150,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_member_places_profile ON member_places(profile_id);
