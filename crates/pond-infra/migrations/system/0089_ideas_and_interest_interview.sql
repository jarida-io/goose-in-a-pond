-- The conversations a guessed interest rests on, so one conversation adds to its `evidence` once
-- however many of its windows mention it.
CREATE TABLE IF NOT EXISTS interest_sightings (
    interest_id  TEXT NOT NULL REFERENCES interests(id) ON DELETE CASCADE,
    session_id   TEXT NOT NULL,
    seen_at      TEXT NOT NULL,
    PRIMARY KEY (interest_id, session_id)
);

-- A member's interests interview, one row each: the turns and the draft it has built so far.
-- `thinking` is set while the next question is being written; `updated_at` lets a read recover
-- an interview whose writer never came back.
CREATE TABLE IF NOT EXISTS interest_interviews (
    profile_id  TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    status      TEXT NOT NULL DEFAULT 'not_started'
                CHECK (status IN ('not_started', 'asking', 'thinking', 'review', 'done')),
    turns_json  TEXT NOT NULL DEFAULT '[]',
    draft_json  TEXT NOT NULL DEFAULT '[]',
    asked       INTEGER NOT NULL DEFAULT 0,
    planned     INTEGER NOT NULL DEFAULT 10,
    round       INTEGER NOT NULL DEFAULT 1,
    updated_at  TEXT NOT NULL
);

-- Ideas Goose researched for a member. Only the model's own words and the pages' addresses are
-- kept, never a search snippet. `topic` is what the idea builds on, so the next one picks another.
-- `goose_plan` is what Goose's part needs and the phone never sees: a prompt, a listings address.
CREATE TABLE IF NOT EXISTS ideas (
    id              TEXT PRIMARY KEY,
    profile_id      TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    topic           TEXT NOT NULL,
    title           TEXT NOT NULL CHECK (length(trim(title)) > 0),
    pitch           TEXT NOT NULL,
    why             TEXT NOT NULL,
    grounded_json   TEXT NOT NULL DEFAULT '[]',
    sources_json    TEXT NOT NULL DEFAULT '[]',
    picture         TEXT,
    goose_can_json  TEXT,
    goose_plan_json TEXT,
    you_can_json    TEXT NOT NULL DEFAULT '[]',
    state           TEXT NOT NULL DEFAULT 'new'
                    CHECK (state IN ('new', 'working', 'done', 'dismissed')),
    task_id         TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ideas_profile ON ideas(profile_id, state, created_at);

-- When a member last asked for ideas and when the lane last finished thinking of them, so a
-- read can say `thinking` without asking the lane.
CREATE TABLE IF NOT EXISTS idea_runs (
    profile_id    TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    requested_at  TEXT,
    started_at    TEXT,
    finished_at   TEXT
);

-- The topic's own picture, a report-image key filled in the background from its Wikipedia
-- article; null until found, and for a topic with no article.
ALTER TABLE interests ADD COLUMN picture_key TEXT;
