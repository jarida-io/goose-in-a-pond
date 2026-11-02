-- The ledger of long-running tasks: a task, its plan steps and the artifacts steps write.
-- New tables, so an existing pond gets them empty; nothing came before them to backfill.
-- Timestamps are RFC 3339 seconds with `Z`, always bound from Rust, so text order is time
-- order and same-second rows fall back to rowid.

CREATE TABLE IF NOT EXISTS long_tasks (
    id                TEXT PRIMARY KEY,

    -- A departed member's tasks and their artifacts are personal data, so they go with them.
    owner_profile_id  TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    -- The starter's scope; for 'owner' the member is owner_profile_id, so the two cannot differ.
    scope             TEXT NOT NULL CHECK (scope IN ('owner', 'household', 'guest')),

    goal              TEXT NOT NULL CHECK (trim(goal) <> ''),

    -- Where the prime reports back. The task is the member's, not the conversation's, so
    -- deleting the conversation leaves the task running.
    origin_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,

    state             TEXT NOT NULL CHECK (state IN (
                          'planning', 'running', 'awaiting_approval', 'paused',
                          'completed', 'failed', 'cancelled')),
    max_steps         INTEGER NOT NULL CHECK (max_steps > 0),
    deadline          TEXT,

    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    finished_at       TEXT,

    CHECK (
        (scope = 'owner' AND owner_profile_id IS NOT NULL)
        OR scope = 'household'
        OR (scope = 'guest' AND owner_profile_id IS NULL)
    )
);

-- A member's panel and the runner's "what is still open" both read by owner and state.
CREATE INDEX IF NOT EXISTS idx_long_tasks_owner_state
    ON long_tasks (owner_profile_id, state);

CREATE TABLE IF NOT EXISTS long_task_steps (
    task_id      TEXT NOT NULL REFERENCES long_tasks(id) ON DELETE CASCADE,
    -- Planner-chosen and unique within its task; `key` alone is an SQL keyword.
    step_key     TEXT NOT NULL CHECK (trim(step_key) <> ''),
    ordinal      INTEGER NOT NULL CHECK (ordinal >= 0),

    role         TEXT NOT NULL CHECK (trim(role) <> ''),
    instructions TEXT NOT NULL CHECK (trim(instructions) <> ''),
    -- Artifact ids the step starts from, and the keys of the steps it waits for.
    inputs       TEXT NOT NULL DEFAULT '[]'
                 CHECK (json_valid(inputs) AND json_type(inputs) = 'array'),
    depends_on   TEXT NOT NULL DEFAULT '[]'
                 CHECK (json_valid(depends_on) AND json_type(depends_on) = 'array'),
    acceptance   TEXT,

    state        TEXT NOT NULL CHECK (state IN (
                     'pending', 'running', 'completed', 'failed', 'skipped',
                     'awaiting_approval')),
    attempts     INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    summary      TEXT,
    failure      TEXT,
    started_at   TEXT,
    finished_at  TEXT,

    UNIQUE (task_id, step_key)
);

CREATE INDEX IF NOT EXISTS idx_long_task_steps_task_ordinal
    ON long_task_steps (task_id, ordinal);

CREATE TABLE IF NOT EXISTS long_task_artifacts (
    id         TEXT PRIMARY KEY,
    -- No scope column: an artifact is read at its task's scope and deleted with it.
    task_id    TEXT NOT NULL REFERENCES long_tasks(id) ON DELETE CASCADE,
    -- No foreign key: a replan deletes the step that wrote it, and the findings outlive it.
    step_key   TEXT,
    kind       TEXT NOT NULL CHECK (kind IN ('note', 'findings', 'draft', 'data')),
    title      TEXT NOT NULL CHECK (trim(title) <> ''),
    summary    TEXT NOT NULL CHECK (trim(summary) <> ''),
    body       TEXT NOT NULL,
    -- Derived from body in Rust and stored so a listing never reads the bodies.
    tokens     INTEGER NOT NULL CHECK (tokens >= 0),
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_long_task_artifacts_task_created
    ON long_task_artifacts (task_id, created_at);
