-- One maker-critic run. `task_id` is its long task; extraction and consolidation have none.
-- `anchor_id` and `deliverable_id` are not foreign keys: the log outlives what it was about.
CREATE TABLE IF NOT EXISTS engine_jobs (
    id              TEXT PRIMARY KEY,
    task_id         TEXT,
    profile_id      TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('deliverable','fact_sheet','memory','consolidation',
                        'heartbeat','capture','ledger','rule','questions')),
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
CREATE INDEX IF NOT EXISTS idx_engine_jobs_open ON engine_jobs(state, class, due_at);
CREATE INDEX IF NOT EXISTS idx_engine_jobs_task ON engine_jobs(task_id);
