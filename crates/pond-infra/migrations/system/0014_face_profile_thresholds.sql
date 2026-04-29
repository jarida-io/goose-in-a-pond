-- Per-profile face-match thresholds.
--
-- The global threshold (chosen at server boot from the model + alignment
-- combination) is right for *most* users.  Some users — beards, glasses,
-- twins, identical-hair siblings — sit closer in the embedding space than
-- the global threshold can safely separate.  The fix is to push their bar
-- a little higher so they're only matched when the model is very sure.
--
-- This migration adds an optional per-profile override.  When NULL, the
-- global threshold applies (current behaviour).  When set, the matcher
-- requires `score >= profile_threshold` for that profile specifically.
--
-- Operators populate it via `/api/v1/faces/profile/{id}/threshold`
-- (forthcoming) or directly from `/faces/debug/eval` recommendations.
--
-- Backwards-compat: existing rows get NULL automatically; matching
-- behaviour is unchanged unless an override is explicitly set.

CREATE TABLE IF NOT EXISTS face_profile_thresholds (
    profile_id        TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    -- Cosine threshold in [0.0, 1.0].  NULL means "use the global threshold".
    match_threshold   REAL,
    -- Free-form note from the operator: "tightened after sibling false-match
    -- on 2026-04-12", "lowered for low-light enrollment", etc.
    note              TEXT,
    updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
