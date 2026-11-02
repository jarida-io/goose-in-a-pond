-- Shipped agents a member declined, so their cards stay hidden for that member. Dismissing again
-- moves the time; a departed member's rows go with them.
CREATE TABLE IF NOT EXISTS agent_suggestion_dismissals (
    template     TEXT NOT NULL CHECK (trim(template) <> ''),
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    dismissed_at TEXT NOT NULL,
    PRIMARY KEY (template, profile_id)
);
