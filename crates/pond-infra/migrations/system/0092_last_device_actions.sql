-- One row per member: the device command "undo"/"rudisha" would reverse. `before`/`after` are
-- JSON-encoded DeviceStatePatch, holding only the fields that command changed.
CREATE TABLE IF NOT EXISTS last_device_actions (
    profile_id  TEXT PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,
    device_id   TEXT NOT NULL,
    before      TEXT NOT NULL,
    after       TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
