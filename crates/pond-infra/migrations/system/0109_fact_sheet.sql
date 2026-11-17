-- A member's fact sheet. A removed line keeps its row, so its key is never written again.
CREATE TABLE IF NOT EXISTS fact_sheet (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    category    TEXT NOT NULL CHECK (category IN ('identity','home','work','people','plan','hunt',
                    'interest','food','health','style','routine','avoid','other')),
    fact        TEXT NOT NULL CHECK (length(trim(fact)) > 0),
    fact_key    TEXT NOT NULL,
    confidence  TEXT NOT NULL DEFAULT 'likely' CHECK (confidence IN ('sure','likely','said')),
    since       TEXT,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('memory','summary','interest','anchor',
                    'answer','interview','you')),
    source_ref  TEXT,
    tools_json  TEXT NOT NULL DEFAULT '[]',
    pinned      INTEGER NOT NULL DEFAULT 0,
    score       REAL NOT NULL DEFAULT 0,
    used        INTEGER NOT NULL DEFAULT 0,
    state       TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','retired','removed')),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (profile_id, fact_key)
);
CREATE INDEX IF NOT EXISTS idx_fact_sheet_member ON fact_sheet(profile_id, state, score);
