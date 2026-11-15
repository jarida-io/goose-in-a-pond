-- The pairing lock, one row: who the main member is, whether a phone must be let in by them,
-- and the recovery PIN with its count of wrong tries. The PIN is kept as a slow hash.
CREATE TABLE IF NOT EXISTS pairing_lock (
    id               INTEGER PRIMARY KEY CHECK (id = 1),
    owner_profile_id TEXT REFERENCES profiles(id) ON DELETE SET NULL,
    locked           INTEGER NOT NULL DEFAULT 1,
    pin_verifier     TEXT,
    pin_failures     INTEGER NOT NULL DEFAULT 0,
    pin_locked_until TEXT
);
INSERT OR IGNORE INTO pairing_lock (id) VALUES (1);

-- A phone that proved it holds a pairing code while the lock was on, waiting to be let in.
-- `claim_hash` is of the secret only that phone was handed.
CREATE TABLE IF NOT EXISTS pairing_requests (
    id          TEXT PRIMARY KEY,
    client_id   TEXT NOT NULL,
    client_type TEXT NOT NULL,
    device_name TEXT NOT NULL,
    profile_id  TEXT REFERENCES profiles(id) ON DELETE SET NULL,
    claim_hash  TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'approved', 'declined', 'claimed')),
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    decided_at  TEXT,
    decided_by  TEXT
);
CREATE INDEX IF NOT EXISTS idx_pairing_requests_waiting ON pairing_requests (status, expires_at);

-- A code the host minted as already accepted: its phone is let in without a request.
ALTER TABLE pairing_codes ADD COLUMN approved INTEGER NOT NULL DEFAULT 0;
