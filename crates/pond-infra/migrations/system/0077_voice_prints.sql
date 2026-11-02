-- Voice Match: one speaker embedding per enrolment take, mirroring face_embeddings (0013).
--
-- No audio is ever stored, only the float vector, packed little-endian f32 with
-- `model_dims * 4` bytes. ON DELETE CASCADE makes forgetting a member forget
-- their voice. Sessions a voice match names carry identification_source
-- 'voice', ranked below 'face' (the values 0037 lists, plus this one).

CREATE TABLE IF NOT EXISTS voice_prints (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    embedding    BLOB NOT NULL,
    model_dims   INTEGER NOT NULL,
    -- Prints from another model are never compared with this one's.
    model_name   TEXT NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_voice_prints_profile_id ON voice_prints(profile_id);

-- Whether the "hey goose" CNN fired on each enrolment take, for the detector
-- choice. A take taught with Voice Match off may name nobody.
CREATE TABLE IF NOT EXISTS voice_wake_takes (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    phrase       TEXT NOT NULL,
    cnn_heard    INTEGER NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_voice_wake_takes_phrase ON voice_wake_takes(phrase);
