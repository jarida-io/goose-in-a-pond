-- Adds the `cards` job kind. SQLite cannot widen a CHECK in place, so the table is rebuilt with
-- every row and index kept.
CREATE TABLE engine_jobs_rebuilt (
    id              TEXT PRIMARY KEY,
    task_id         TEXT,
    profile_id      TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('deliverable','fact_sheet','memory','consolidation',
                        'heartbeat','capture','ledger','rule','questions','cards')),
    class           TEXT NOT NULL CHECK (class IN ('asked','deadline','upkeep','open')),
    anchor_id       TEXT,
    deliverable_id  TEXT,
    subject         TEXT NOT NULL DEFAULT '',
    due_at          TEXT,
    state           TEXT NOT NULL DEFAULT 'queued'
                    CHECK (state IN ('queued','running','passed','unsure','dropped','failed','cancelled')),
    rounds          INTEGER NOT NULL DEFAULT 0,
    objections_json TEXT NOT NULL DEFAULT '[]',
    outcome         TEXT,
    created_at      TEXT NOT NULL,
    started_at      TEXT,
    finished_at     TEXT
);

INSERT INTO engine_jobs_rebuilt (id, task_id, profile_id, kind, class, anchor_id, deliverable_id,
                                 subject, due_at, state, rounds, objections_json, outcome,
                                 created_at, started_at, finished_at)
SELECT id, task_id, profile_id, kind, class, anchor_id, deliverable_id, subject, due_at, state,
       rounds, objections_json, outcome, created_at, started_at, finished_at
  FROM engine_jobs;

DROP TABLE engine_jobs;
ALTER TABLE engine_jobs_rebuilt RENAME TO engine_jobs;

CREATE INDEX IF NOT EXISTS idx_engine_jobs_open ON engine_jobs(state, class, due_at);
CREATE INDEX IF NOT EXISTS idx_engine_jobs_task ON engine_jobs(task_id);
