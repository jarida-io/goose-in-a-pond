-- The idea cards Goose offers a member. A set is the cards one `cards` job made (`batch_id`),
-- `rank` their order within it. Faded and settled rows stay a week so the next set does not
-- repeat them.
CREATE TABLE IF NOT EXISTS cards (
    id            TEXT PRIMARY KEY,
    profile_id    TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    batch_id      TEXT NOT NULL,
    rank          INTEGER NOT NULL DEFAULT 0,
    kind          TEXT NOT NULL CHECK (kind IN ('do','next_step','reminder','ask')),
    title         TEXT NOT NULL CHECK (trim(title) <> ''),
    why_json      TEXT NOT NULL,
    when_text     TEXT,
    where_text    TEXT,
    cost          TEXT,
    effort        TEXT,
    action_json   TEXT,
    sources_json  TEXT NOT NULL DEFAULT '[]',
    notes_json    TEXT NOT NULL DEFAULT '{}',
    anchor_id     TEXT,
    anchor_title  TEXT,
    state         TEXT NOT NULL DEFAULT 'new' CHECK (state IN ('new','saved','done','dismissed')),
    created_at    TEXT NOT NULL,
    fades_at      TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_cards_member_state ON cards(profile_id, state, created_at);
