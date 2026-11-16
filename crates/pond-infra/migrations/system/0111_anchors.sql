-- A real thing in a member's life, and the finished work that hangs off it.
CREATE TABLE IF NOT EXISTS anchors (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('event','trip','purchase','project','interest','standing')),
    title        TEXT NOT NULL CHECK (length(trim(title)) > 0),
    title_key    TEXT NOT NULL,
    state        TEXT NOT NULL DEFAULT 'candidate'
                 CHECK (state IN ('candidate','active','ended','stopped','faded')),
    starts_at    TEXT,
    ends_at      TEXT,
    place        TEXT,
    facts_json   TEXT NOT NULL DEFAULT '[]',
    agent        TEXT,
    origin       TEXT NOT NULL CHECK (origin IN ('chat','capture','interest','you','goose')),
    origin_ref   TEXT,
    why          TEXT NOT NULL DEFAULT '',
    offered_at   TEXT,
    confirmed_at TEXT,
    ended_at     TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    UNIQUE (profile_id, title_key)
);
CREATE INDEX IF NOT EXISTS idx_anchors_member ON anchors(profile_id, state, starts_at);
-- The other members an anchor is shared with; its owner is `anchors.profile_id`.
CREATE TABLE IF NOT EXISTS anchor_shares (
    anchor_id  TEXT NOT NULL REFERENCES anchors(id) ON DELETE CASCADE,
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    PRIMARY KEY (anchor_id, profile_id)
);
-- One row per recipe slot of an anchor; a refresh rewrites the row and counts a version.
CREATE TABLE IF NOT EXISTS deliverables (
    id              TEXT PRIMARY KEY,
    anchor_id       TEXT NOT NULL REFERENCES anchors(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('dossier','plan','shortlist','digest','checklist')),
    slot            TEXT NOT NULL,
    title           TEXT NOT NULL CHECK (length(trim(title)) > 0),
    state           TEXT NOT NULL DEFAULT 'queued'
                    CHECK (state IN ('queued','making','ready','unsure','rejected','archived')),
    hook            TEXT NOT NULL DEFAULT '',
    body            TEXT NOT NULL DEFAULT '',
    facts_json      TEXT NOT NULL DEFAULT '[]',
    sources_json    TEXT NOT NULL DEFAULT '[]',
    objections_json TEXT NOT NULL DEFAULT '[]',
    changed         TEXT NOT NULL DEFAULT '',
    version         INTEGER NOT NULL DEFAULT 0,
    task_id         TEXT,
    refresh_at      TEXT,
    seen_at         TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE (anchor_id, slot)
);
