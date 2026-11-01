-- An agent the planner made for one step when no saved role fits: its name, instructions and the
-- tool groups it asked for, as JSON. NULL for a step that names a saved role.
ALTER TABLE long_task_steps ADD COLUMN agent TEXT;
