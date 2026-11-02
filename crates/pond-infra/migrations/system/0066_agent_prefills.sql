-- The agents whose prefills the pond keeps: the definition each was compiled from, and when it
-- was last compiled and last used. Keyed by agent name, with no foreign key, since the shipped
-- roles are not recipes; the runner forgets the rows of agents that no longer exist.
-- Timestamps are RFC 3339 seconds with `Z`, always bound from Rust.
CREATE TABLE IF NOT EXISTS agent_prefills (
    agent           TEXT PRIMARY KEY CHECK (trim(agent) <> ''),
    definition_hash TEXT NOT NULL,
    compiled_at     TEXT,
    last_used_at    TEXT
);
