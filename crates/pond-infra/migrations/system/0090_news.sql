-- A member's news choices and the bookkeeping between passes. A member with no row has the
-- defaults; a NULL timezone follows the household's.
CREATE TABLE IF NOT EXISTS news_settings (
    profile_id            TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    tiles                 INTEGER NOT NULL DEFAULT 1,
    digest                INTEGER NOT NULL DEFAULT 1,
    digest_time           TEXT NOT NULL DEFAULT '07:00',
    spoken_on_speaker     INTEGER NOT NULL DEFAULT 0,
    timezone              TEXT,
    refresh_requested_at  TEXT,
    writing_since         TEXT,
    stories_written_at    TEXT,
    spoken_on             TEXT,
    updated_at            TEXT NOT NULL
);

-- Stories the pond wrote from pages it read. Only the model's words, the pages' addresses and
-- names, and a kept picture: a search result's own words are never stored.
CREATE TABLE IF NOT EXISTS news_stories (
    id               TEXT PRIMARY KEY,
    profile_id       TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    kind             TEXT NOT NULL DEFAULT 'news' CHECK (kind IN ('news', 'long_read')),
    interest_id      TEXT NOT NULL,
    interest_topic   TEXT NOT NULL,
    headline         TEXT NOT NULL,
    headline_key     TEXT NOT NULL,
    dek              TEXT NOT NULL,
    summary          TEXT NOT NULL,
    why_you          TEXT NOT NULL,
    url              TEXT NOT NULL,
    url_key          TEXT NOT NULL,
    site             TEXT NOT NULL,
    source_title     TEXT NOT NULL,
    further_sources  TEXT NOT NULL DEFAULT '[]',
    picture_key      TEXT,
    found_by         TEXT,
    published_at     TEXT,
    written_at       TEXT NOT NULL,
    read_at          TEXT,
    vote             TEXT CHECK (vote IN ('up', 'down', 'wrong')),
    vote_note        TEXT,
    voted_at         TEXT,
    UNIQUE (profile_id, url_key)
);
CREATE INDEX IF NOT EXISTS idx_news_stories_profile ON news_stories(profile_id, written_at);
CREATE INDEX IF NOT EXISTS idx_news_stories_interest ON news_stories(profile_id, interest_id, written_at);

-- Every digest is kept; a section carries its own pictures and references.
CREATE TABLE IF NOT EXISTS news_digests (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    day         TEXT NOT NULL,
    title       TEXT NOT NULL,
    intro       TEXT NOT NULL,
    sections    TEXT NOT NULL,
    written_at  TEXT NOT NULL,
    UNIQUE (profile_id, day)
);
CREATE INDEX IF NOT EXISTS idx_news_digests_profile ON news_digests(profile_id, written_at);

-- A member's topic map: scored web-search queries written from their interests, portrait,
-- memories and scheduled tasks. The news searches the newsworthy ones; ideas take the rest and
-- stamp used_at.
CREATE TABLE IF NOT EXISTS member_topics (id TEXT PRIMARY KEY, profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE, area TEXT NOT NULL, subject TEXT NOT NULL, query TEXT NOT NULL, newsworthy INTEGER NOT NULL, score REAL NOT NULL, why TEXT NOT NULL DEFAULT '', built_at TEXT NOT NULL, used_at TEXT);
CREATE INDEX IF NOT EXISTS idx_member_topics_profile ON member_topics(profile_id, newsworthy, score);
