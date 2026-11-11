-- Links a member asked Goose to watch the price of. Prices are in the page's own currency, which
-- a change of currency on the page does not mix into the history.
CREATE TABLE IF NOT EXISTS cart_items (
    id              TEXT PRIMARY KEY,
    profile_id      TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    url             TEXT NOT NULL,
    title           TEXT NOT NULL DEFAULT '',
    site            TEXT NOT NULL DEFAULT '',
    picture_key     TEXT,
    currency        TEXT,
    price           REAL,
    previous_price  REAL,
    lowest_price    REAL,
    highest_price   REAL,
    target_price    REAL,
    in_stock        INTEGER,
    status          TEXT NOT NULL DEFAULT 'checking' CHECK (status IN ('ok', 'checking', 'failing')),
    failures        INTEGER NOT NULL DEFAULT 0,
    every_hours     INTEGER NOT NULL DEFAULT 6,
    notify          TEXT NOT NULL DEFAULT 'drop' CHECK (notify IN ('drop', 'target', 'stock', 'any')),
    collection      TEXT,
    checked_at      TEXT,
    next_check_at   TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE (profile_id, url)
);
CREATE INDEX IF NOT EXISTS idx_cart_items_due ON cart_items(next_check_at);

CREATE TABLE IF NOT EXISTS cart_prices (
    cart_id   TEXT NOT NULL REFERENCES cart_items(id) ON DELETE CASCADE,
    at        TEXT NOT NULL,
    price     REAL NOT NULL,
    in_stock  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_cart_prices_item ON cart_prices(cart_id, at);

-- A member's custom webhook connectors. Only the token's hash is kept, so its address is shown
-- once, when it is made.
CREATE TABLE IF NOT EXISTS connector_webhooks (
    id             TEXT PRIMARY KEY,
    profile_id     TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    name           TEXT NOT NULL CHECK (length(trim(name)) > 0),
    token_hash     TEXT NOT NULL UNIQUE,
    created_at     TEXT NOT NULL,
    last_event_at  TEXT
);

-- What any part of the Pond raises for a member's to-do hooks to match: a price change, a watch
-- match, a webhook call, a new mail or calendar item, a place, presence, a camera or a device.
-- A null `profile_id` is a household event that any member's hook may match.
CREATE TABLE IF NOT EXISTS hook_events (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT,
    source_kind  TEXT NOT NULL,
    source_id    TEXT NOT NULL,
    event        TEXT NOT NULL,
    payload      TEXT NOT NULL DEFAULT '{}',
    at           TEXT NOT NULL,
    handled_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_hook_events_unhandled ON hook_events(handled_at, at);
