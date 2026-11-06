-- My Pond's per-phone keys: one per device, as a SHA-256 of a 32-byte random key the phone keeps
-- behind its biometrics. Removing or revoking a device takes its key with it.
CREATE TABLE IF NOT EXISTS admin_device_keys (
    device_id  TEXT PRIMARY KEY REFERENCES devices(id) ON DELETE CASCADE,
    key_hash   TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
