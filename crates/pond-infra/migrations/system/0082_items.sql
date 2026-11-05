-- The item header (docs/architecture/context-space.md 4.1). Today's tables stay the authority:
-- deleting a header unlinks its row, deleting a row deletes its header. Headers are written in
-- Rust after the migrations (`Database::init`), since their text is redacted.
-- Timestamps are RFC 3339 UTC with milliseconds (`pond_core::items::timestamp`).

CREATE TABLE IF NOT EXISTS items (
    -- UUIDv7, so ids sort by when the header was written.
    id               TEXT PRIMARY KEY
                     CHECK (length(id) = 36 AND substr(id, 15, 1) = '7'),
    kind             TEXT NOT NULL CHECK (kind IN (
                         'message', 'conversation', 'fact', 'entity', 'event', 'media',
                         'notification', 'mail', 'home_event', 'visit', 'report',
                         'suggestion', 'trait')),
    -- A context source's id, or `pond:memory` / `pond:session` / `pond:suggestion` for the
    -- pond's own tables, whose row key is then `external_id`.
    source_id        TEXT NOT NULL CHECK (trim(source_id) <> ''),
    external_id      TEXT NOT NULL CHECK (trim(external_id) <> ''),
    owner_profile_id TEXT REFERENCES profiles(id) ON DELETE CASCADE
                     CHECK (owner_profile_id IS NULL OR trim(owner_profile_id) <> ''),
    audience         TEXT NOT NULL DEFAULT 'private'
                     CHECK (audience IN ('private', 'household')),
    sensitivity      TEXT NOT NULL DEFAULT 'sensitive'
                     CHECK (sensitivity IN ('public', 'internal', 'sensitive', 'secret')),
    modality         TEXT NOT NULL DEFAULT 'text'
                     CHECK (modality IN ('text', 'image', 'audio', 'video')),
    observed_at      TEXT NOT NULL CHECK (observed_at GLOB
                         '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'),
    valid_until      TEXT CHECK (valid_until IS NULL OR valid_until GLOB
                         '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'),
    -- Redacted, unlike some detail tables' own text (a conversation replays its raw summary).
    title            TEXT NOT NULL DEFAULT '',
    body             TEXT NOT NULL DEFAULT '',
    context          TEXT NOT NULL DEFAULT '',
    state            TEXT NOT NULL DEFAULT 'active' CHECK (state IN (
                         'active', 'archived', 'superseded', 'dismissed', 'accepted')),
    produced_by      TEXT NOT NULL CHECK (produced_by = 'ingest'
                         OR produced_by GLOB 'pond:?*'
                         OR produced_by GLOB 'recipe:?*@?*'),
    confidence       REAL CHECK (confidence IS NULL OR (confidence >= 0 AND confidence <= 1)),
    created_at       TEXT NOT NULL CHECK (created_at GLOB
                         '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'),
    -- Moves whenever the header does; the index compares it to tell a stale vector.
    updated_at       TEXT NOT NULL CHECK (updated_at GLOB
                         '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'),

    UNIQUE (source_id, external_id),
    CHECK (owner_profile_id IS NOT NULL OR audience = 'household')
);

CREATE INDEX IF NOT EXISTS idx_items_owner_kind ON items (owner_profile_id, kind, state);
CREATE INDEX IF NOT EXISTS idx_items_kind_observed ON items (kind, state, observed_at);

-- Reads `from_id <relation> to_id`: a conversation is evidence_for the fact it taught.
CREATE TABLE IF NOT EXISTS item_links (
    from_id    TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    relation   TEXT NOT NULL CHECK (relation IN (
                   'derived_from', 'evidence_for', 'mentions', 'same_as', 'supersedes')),
    to_id      TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL CHECK (created_at GLOB
                   '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'),
    PRIMARY KEY (from_id, relation, to_id),
    CHECK (from_id <> to_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_item_links_to ON item_links (to_id, relation);

-- A private item shown to named members besides its owner.
CREATE TABLE IF NOT EXISTS item_shares (
    item_id    TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL CHECK (created_at GLOB
                   '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'),
    PRIMARY KEY (item_id, profile_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_item_shares_profile ON item_shares (profile_id);

-- New kinds' details: the header is their authority, so deleting it deletes them.
CREATE TABLE IF NOT EXISTS media_details (
    item_id      TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
    blob_id      TEXT NOT NULL CHECK (trim(blob_id) <> ''),
    mime_type    TEXT NOT NULL CHECK (trim(mime_type) <> ''),
    size_bytes   INTEGER NOT NULL CHECK (size_bytes >= 0),
    content_hash TEXT NOT NULL CHECK (trim(content_hash) <> '')
);

-- Ingest deduplicates media on the content hash as well as on (source_id, external_id).
CREATE INDEX IF NOT EXISTS idx_media_details_hash ON media_details (content_hash);

CREATE TABLE IF NOT EXISTS visit_details (
    item_id       TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
    place_cluster TEXT NOT NULL CHECK (trim(place_cluster) <> ''),
    arrived_at    TEXT NOT NULL,
    departed_at   TEXT
);

ALTER TABLE memory_fragments    ADD COLUMN item_id TEXT REFERENCES items(id) ON DELETE SET NULL;
ALTER TABLE sessions            ADD COLUMN item_id TEXT REFERENCES items(id) ON DELETE SET NULL;
ALTER TABLE session_messages    ADD COLUMN item_id TEXT REFERENCES items(id) ON DELETE SET NULL;
ALTER TABLE context_items       ADD COLUMN item_id TEXT REFERENCES items(id) ON DELETE SET NULL;
ALTER TABLE suggestion_queue    ADD COLUMN item_id TEXT REFERENCES items(id) ON DELETE SET NULL;
ALTER TABLE long_task_artifacts ADD COLUMN item_id TEXT REFERENCES items(id) ON DELETE SET NULL;

-- Unique, and an index: SET NULL would otherwise scan the detail table for each header deleted.
CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_fragments_item ON memory_fragments (item_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_item ON sessions (item_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_session_messages_item ON session_messages (item_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_context_items_item ON context_items (item_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_suggestion_queue_item ON suggestion_queue (item_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_long_task_artifacts_item ON long_task_artifacts (item_id);

-- A detail row's header goes with it, however the row went: a writer, a cascade from its
-- session or source, or a member's deletion.
CREATE TRIGGER IF NOT EXISTS trg_memory_fragments_delete_item
AFTER DELETE ON memory_fragments FOR EACH ROW WHEN OLD.item_id IS NOT NULL
BEGIN
    DELETE FROM items WHERE id = OLD.item_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_sessions_delete_item
AFTER DELETE ON sessions FOR EACH ROW WHEN OLD.item_id IS NOT NULL
BEGIN
    DELETE FROM items WHERE id = OLD.item_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_session_messages_delete_item
AFTER DELETE ON session_messages FOR EACH ROW WHEN OLD.item_id IS NOT NULL
BEGIN
    DELETE FROM items WHERE id = OLD.item_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_context_items_delete_item
AFTER DELETE ON context_items FOR EACH ROW WHEN OLD.item_id IS NOT NULL
BEGIN
    DELETE FROM items WHERE id = OLD.item_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_suggestion_queue_delete_item
AFTER DELETE ON suggestion_queue FOR EACH ROW WHEN OLD.item_id IS NOT NULL
BEGIN
    DELETE FROM items WHERE id = OLD.item_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_long_task_artifacts_delete_item
AFTER DELETE ON long_task_artifacts FOR EACH ROW WHEN OLD.item_id IS NOT NULL
BEGIN
    DELETE FROM items WHERE id = OLD.item_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_media_details_delete_item
AFTER DELETE ON media_details FOR EACH ROW
BEGIN
    DELETE FROM items WHERE id = OLD.item_id;
END;

CREATE TRIGGER IF NOT EXISTS trg_visit_details_delete_item
AFTER DELETE ON visit_details FOR EACH ROW
BEGIN
    DELETE FROM items WHERE id = OLD.item_id;
END;
