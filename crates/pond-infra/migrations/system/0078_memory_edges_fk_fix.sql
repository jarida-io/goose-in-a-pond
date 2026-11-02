-- `memory_edges` (0017) declared its foreign keys against a table called `memories`, which has
-- never existed -- `0005_memory.sql` named the real table `memory_fragments` twelve migrations
-- earlier. With `PRAGMA foreign_keys = ON` (every connection, see `Database::connect`), SQLite
-- resolves that FK's parent table eagerly, so every write to `memory_edges` -- insert, delete,
-- even an empty `DELETE FROM memory_edges` -- has always failed with "no such table: main.memories".
--
-- `MemoryRepository::add_edge`/`get_edges_from`/`get_edges_to` are still their default no-op
-- trait impls (`memory_repository.rs`); nothing has ever called through to a row here. DROP +
-- recreate is safe on every existing Pond because there is no data to lose.
DROP TABLE IF EXISTS memory_edges;

CREATE TABLE memory_edges (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    from_id   TEXT    NOT NULL,
    to_id     TEXT    NOT NULL,
    relation  TEXT    NOT NULL,       -- 'caused', 'referenced', 'superseded'
    created_at TEXT   NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (from_id) REFERENCES memory_fragments(id),
    FOREIGN KEY (to_id)   REFERENCES memory_fragments(id)
);

CREATE INDEX idx_memory_edges_from ON memory_edges(from_id);
CREATE INDEX idx_memory_edges_to   ON memory_edges(to_id);
