-- The way back after a model switch. Per role, the model that last gave a real answer, and
-- the one a rollback would return to: the last working model at the moment of the switch.
--
-- Kept beside `model_role_assignments` rather than in it, so a role's current model and its
-- history can change independently and a list of assignments never shows history rows.

CREATE TABLE IF NOT EXISTS model_role_history (
    role                  TEXT PRIMARY KEY,
    last_working_model_id TEXT,              -- references models(id)
    rollback_model_id     TEXT,              -- references models(id)
    updated_at            TEXT NOT NULL DEFAULT (datetime('now'))
);
