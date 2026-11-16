-- What a member told Goose not to offer. Every maker and critic reads the `never` rules.
CREATE TABLE IF NOT EXISTS dont_list (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    rule         TEXT NOT NULL CHECK (length(trim(rule)) > 0),
    rule_key     TEXT NOT NULL,
    strength     TEXT NOT NULL CHECK (strength IN ('never','avoid')),
    reasons_json TEXT NOT NULL DEFAULT '[]',
    said         TEXT,
    origin       TEXT NOT NULL CHECK (origin IN ('reject','stop','fact','you')),
    origin_ref   TEXT,
    created_at   TEXT NOT NULL,
    UNIQUE (profile_id, rule_key)
);
