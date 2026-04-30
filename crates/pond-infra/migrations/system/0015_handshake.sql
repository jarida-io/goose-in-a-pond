-- Handshake protocol tables for GOTG ↔ GIAP authenticated communication.
--
-- Pairing flow:
--   1. Server issues a 6-digit pairing code (hashed at rest, single-use, 10 min TTL)
--   2. Client requests a challenge (32 random bytes, 60 s TTL)
--   3. Client returns HMAC-SHA256(pairing_code, challenge || client_id)
--   4. On verify: a session_token + refresh_token pair is minted (hashed at rest)
--
-- All token material returned to clients is opaque base64; only sha256 hashes
-- live on disk. Constant-time compare is used in the application layer.

CREATE TABLE pairing_codes (
    code_hash         TEXT PRIMARY KEY,    -- sha256(code) hex
    created_at        TEXT NOT NULL,
    expires_at        TEXT NOT NULL,
    consumed_at       TEXT,                -- non-NULL once a verify uses this code
    failed_attempts   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE handshake_challenges (
    id          TEXT PRIMARY KEY,          -- uuid v4
    client_id   TEXT NOT NULL,
    challenge   BLOB NOT NULL,             -- 32 random bytes
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    consumed_at TEXT
);

CREATE INDEX idx_handshake_challenges_expires ON handshake_challenges(expires_at);

CREATE TABLE session_tokens (
    token_hash         TEXT PRIMARY KEY,   -- sha256(session_token) hex
    refresh_hash       TEXT UNIQUE NOT NULL,
    device_id          TEXT NOT NULL,
    client_id          TEXT NOT NULL,
    client_type        TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    expires_at         TEXT NOT NULL,
    refresh_expires_at TEXT NOT NULL,
    revoked_at         TEXT,
    last_seen_at       TEXT
);

CREATE INDEX idx_session_tokens_device       ON session_tokens(device_id);
CREATE INDEX idx_session_tokens_refresh      ON session_tokens(refresh_hash);
CREATE INDEX idx_session_tokens_expires      ON session_tokens(expires_at);
