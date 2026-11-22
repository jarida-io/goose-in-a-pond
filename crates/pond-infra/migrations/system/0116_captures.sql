-- A screenshot or photo from a member's phone. `content_hash` stops the same picture twice.
CREATE TABLE IF NOT EXISTS captures (
    id            TEXT PRIMARY KEY,
    profile_id    TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('screenshot','photo')),
    album         TEXT,
    taken_at      TEXT,
    state         TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','read')),
    sensitive     TEXT NOT NULL DEFAULT 'none' CHECK (sensitive IN ('none','money')),
    hot           INTEGER NOT NULL DEFAULT 0,
    description   TEXT,
    intent        TEXT,
    details_json  TEXT NOT NULL DEFAULT '{}',
    picture_file  TEXT,
    picture_until TEXT,
    content_hash  TEXT,
    attempts      INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (profile_id, content_hash)
);
-- The Budget specialist's records; amounts are in the currency's minor unit.
CREATE TABLE IF NOT EXISTS ledger_entries (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    occurred_on  TEXT NOT NULL,
    payee        TEXT NOT NULL,
    amount_minor INTEGER NOT NULL,
    currency     TEXT NOT NULL,
    category     TEXT NOT NULL DEFAULT 'other',
    anchor_id    TEXT,
    note         TEXT,
    created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ledger_member ON ledger_entries(profile_id, occurred_on);
-- Which specialists a member has on; no row means the specialist's shipped default.
CREATE TABLE IF NOT EXISTS member_agents (
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    agent      TEXT NOT NULL CHECK (agent IN ('budget','work','trip','hunt')),
    state      TEXT NOT NULL CHECK (state IN ('on','off','offered','declined')),
    changed_at TEXT NOT NULL,
    PRIMARY KEY (profile_id, agent)
);
