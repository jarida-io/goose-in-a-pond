-- What a long task is for, which decides how its planner and workers are briefed. Every task
-- before this was general.
ALTER TABLE long_tasks ADD COLUMN kind TEXT NOT NULL DEFAULT 'general'
    CHECK (kind IN ('general', 'research'));
