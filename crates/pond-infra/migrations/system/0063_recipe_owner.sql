-- Whose agent a recipe is: the member whose paired device saved it. An agent that runs on its
-- own, on a schedule or an event that names nobody, runs at this member's scope.
-- Existing recipes stay ownerless: nothing recorded who wrote them.
ALTER TABLE agent_recipes ADD COLUMN owner_profile_id TEXT REFERENCES profiles(id) ON DELETE SET NULL;
