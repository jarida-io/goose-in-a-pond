-- The index keyed by item (context-space.md 4.3), still without text or foreign keys: an orphan
-- resolves to nothing on the JOIN to `sys.items`. Runs after the item backfill (`Database::init`).

CREATE TABLE IF NOT EXISTS vectors_by_item (
    item_id      TEXT NOT NULL,
    -- 0 for a whole-text vector. Passages of one item are contiguous from 0.
    chunk_ix     INTEGER NOT NULL DEFAULT 0,
    -- Byte span into the item's embedded text; NULL = the whole of it.
    chunk_start  INTEGER,
    chunk_len    INTEGER,
    model_id     TEXT NOT NULL,
    modality     TEXT NOT NULL DEFAULT 'text',
    dims         INTEGER NOT NULL,
    vector       BLOB NOT NULL,
    -- The header's `updated_at` this vector was made from.
    source_rev   TEXT,
    embedded_at  TEXT NOT NULL,
    PRIMARY KEY (item_id, chunk_ix)
);

-- Memory and context vectors were made from text already redacted, as their headers hold, so
-- they carry across; summary vectors were made from the raw summary and are re-embedded.
INSERT OR IGNORE INTO vectors_by_item
    (item_id, chunk_ix, chunk_start, chunk_len, model_id, modality, dims, vector,
     source_rev, embedded_at)
SELECT i.id, v.chunk_ix, v.chunk_start, v.chunk_len, v.model_id, 'text', v.dims, v.vector,
       i.updated_at, v.embedded_at
FROM vectors v
JOIN sys.memory_fragments m ON v.corpus = 'memory' AND m.id = v.row_id
JOIN sys.items i ON i.id = m.item_id;

INSERT OR IGNORE INTO vectors_by_item
    (item_id, chunk_ix, chunk_start, chunk_len, model_id, modality, dims, vector,
     source_rev, embedded_at)
SELECT i.id, v.chunk_ix, v.chunk_start, v.chunk_len, v.model_id, 'text', v.dims, v.vector,
       i.updated_at, v.embedded_at
FROM vectors v
JOIN sys.context_items c ON v.corpus = 'context' AND c.id = v.row_id
JOIN sys.items i ON i.id = c.item_id;

DROP TABLE vectors;
ALTER TABLE vectors_by_item RENAME TO vectors;

-- Retrieval always filters by model first: mixing spaces is refused, not scored.
CREATE INDEX IF NOT EXISTS idx_vectors_model ON vectors (model_id);
