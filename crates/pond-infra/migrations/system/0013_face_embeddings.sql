-- Face embeddings for household-member identification.
--
-- Each row stores one enrolled face per profile.  Multiple enrollments per
-- profile are encouraged — cosine-similarity matching picks the highest score
-- across all rows.
--
-- Privacy:
--   - No raw image bytes are stored — only the opaque float vector.
--   - `embedding` is a packed little-endian f32 BLOB whose length is
--     `model_dims * 4` bytes.
--   - `ON DELETE CASCADE` ensures deleting a profile also deletes its
--     biometric data (supports the "forget all biometric data" contract).

CREATE TABLE IF NOT EXISTS face_embeddings (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    embedding    BLOB NOT NULL,
    -- Dimensionality of the embedding (e.g. 512 for ArcFace, 128 for MobileFaceNet).
    model_dims   INTEGER NOT NULL,
    -- Free-form identifier of the producing model (e.g. "arcface-r100",
    -- "mobilefacenet").  Lets us migrate to newer models without dropping data.
    model_name   TEXT NOT NULL DEFAULT 'arcface',
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_face_embeddings_profile_id
    ON face_embeddings(profile_id);

CREATE INDEX IF NOT EXISTS idx_face_embeddings_created_at
    ON face_embeddings(created_at);
