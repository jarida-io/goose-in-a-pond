-- What a member is into, kept apart from memories so the news, ideas and the interview can read
-- and change one row per interest. A row the member has not confirmed is only a guess from chat,
-- and `evidence` counts the conversations that guess rests on.
CREATE TABLE IF NOT EXISTS interests (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    topic       TEXT NOT NULL CHECK (length(trim(topic)) > 0),
    topic_key   TEXT NOT NULL,
    category    TEXT NOT NULL DEFAULT 'other',
    facets      TEXT NOT NULL DEFAULT '[]',
    depth       TEXT NOT NULL DEFAULT 'curious' CHECK (depth IN ('curious', 'keen', 'devoted')),
    news        TEXT NOT NULL DEFAULT 'both' CHECK (news IN ('off', 'tiles', 'digest', 'both')),
    avoid       INTEGER NOT NULL DEFAULT 0,
    origin      TEXT NOT NULL DEFAULT 'you' CHECK (origin IN ('interview', 'chat', 'you')),
    evidence    INTEGER NOT NULL DEFAULT 1,
    confirmed   INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (profile_id, topic_key)
);
CREATE INDEX IF NOT EXISTS idx_interests_profile ON interests(profile_id, confirmed, updated_at);

-- The background model's short account of a member, rewritten on the inference lane from their
-- interests, memories and recent conversations; the news and ideas read it instead of every memory.
CREATE TABLE IF NOT EXISTS member_portraits (
    profile_id          TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    summary             TEXT NOT NULL,
    into_json           TEXT NOT NULL DEFAULT '[]',
    working_on_json     TEXT NOT NULL DEFAULT '[]',
    likes_json          TEXT NOT NULL DEFAULT '[]',
    updated_at          TEXT NOT NULL
);
