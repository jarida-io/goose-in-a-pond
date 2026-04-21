-- Speaker (voice) embeddings for household member identification.
--
-- Each row is one enrolment sample linked to profiles(id).
-- The embedding column is a raw little-endian float32 BLOB — opaque and not
-- reconstructable to the original audio.
--
-- ON DELETE CASCADE ensures all embeddings are wiped when a profile is deleted,
-- satisfying the "forget all biometric data" requirement.

CREATE TABLE IF NOT EXISTS speaker_embeddings (
    id          TEXT    PRIMARY KEY,
    profile_id  TEXT    NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    model       TEXT    NOT NULL,               -- 'resemblyzer' | 'x-vector'
    dims        INTEGER NOT NULL,               -- 256 or 512
    threshold   REAL    NOT NULL DEFAULT 0.50,  -- cosine-similarity threshold
    embedding   BLOB    NOT NULL,               -- Vec<f32> little-endian bytes
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_speaker_embeddings_profile_id
    ON speaker_embeddings(profile_id);

CREATE INDEX IF NOT EXISTS idx_speaker_embeddings_created_at
    ON speaker_embeddings(created_at);
