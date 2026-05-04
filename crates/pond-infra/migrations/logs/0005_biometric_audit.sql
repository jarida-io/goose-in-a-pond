-- Biometric audit log: records every enroll / identify / delete action.
--
-- action values:
--   'enroll_speaker'    — a new voice embedding was registered
--   'identify_speaker'  — a speaker identification was attempted
--   'delete_biometrics' — all biometric data for a profile was deleted
--
-- profile_id is NULL on identify_speaker when no match was found.
-- TTL: 30 days (pruned by the background pruning job).

CREATE TABLE IF NOT EXISTS biometric_audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id  TEXT,               -- NULL when identification found no match
    action      TEXT    NOT NULL,   -- see action values above
    modality    TEXT    NOT NULL DEFAULT 'voice',
    confidence  REAL,               -- NULL for enroll/delete actions
    model       TEXT,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_biometric_audit_profile_id
    ON biometric_audit_log(profile_id);

CREATE INDEX IF NOT EXISTS idx_biometric_audit_created_at
    ON biometric_audit_log(created_at);
