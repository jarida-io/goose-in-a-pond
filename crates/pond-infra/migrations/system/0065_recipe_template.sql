-- The shipped template a recipe was copied from, so the pond knows which members already have an
-- agent and an edited copy is never given more tools than its template. NULL for recipes written
-- by hand, which is every recipe saved before this column.
ALTER TABLE agent_recipes ADD COLUMN template TEXT;
