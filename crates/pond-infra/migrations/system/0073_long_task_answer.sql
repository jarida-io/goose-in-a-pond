-- How long a task's answer should be: chosen by the member, or decided by its planner from the ask.
ALTER TABLE long_tasks ADD COLUMN answer TEXT NOT NULL DEFAULT 'auto'
    CHECK (answer IN ('auto', 'short', 'report'));
