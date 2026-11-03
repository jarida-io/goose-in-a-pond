-- The household's notification history (2026-10-28), so a screen that was not connected when a
-- notice went out (the dashboard after a reload) can list it. The `notifications` queue cannot
-- serve that: it holds per-phone copies only, none at all for a household without a paired
-- phone, and none for a member notice to a member with no phone.
--
-- One row per durable notice under its household id. `publish_live` status (warm-up) is never
-- written. Who may read a row is `audience`:
--   'household' -- every device but `except_device` (NULL: every device);
--   'devices'   -- only the devices in notification_history_recipients (a member's own, or the
--                  one device a notice was sent to), as the queue scoped it when it went out.
-- Retention follows the queue's age cap and per-device cap (`pond_infra::pruning`).
CREATE TABLE IF NOT EXISTS notification_history (
    id            TEXT PRIMARY KEY,
    audience      TEXT NOT NULL CHECK (audience IN ('household', 'devices')),
    except_device TEXT,
    category      TEXT NOT NULL,
    title         TEXT NOT NULL,
    body          TEXT NOT NULL,
    timestamp     TEXT NOT NULL,         -- the notice's own timestamp
    data          TEXT,                  -- optional JSON client payload, `open` hint included
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_notification_history_created
    ON notification_history (created_at);

-- The devices an audience = 'devices' notice is for. A removed device's rows go with it.
CREATE TABLE IF NOT EXISTS notification_history_recipients (
    notification_id TEXT NOT NULL,
    device_id       TEXT NOT NULL,
    PRIMARY KEY (notification_id, device_id),
    FOREIGN KEY (notification_id) REFERENCES notification_history(id) ON DELETE CASCADE,
    FOREIGN KEY (device_id) REFERENCES devices(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_notification_history_recipients_device
    ON notification_history_recipients (device_id);

-- Which device marked which notice read. No foreign key to `devices`: the reader is named by its
-- session token, and a row outlives nothing but its notice.
CREATE TABLE IF NOT EXISTS notification_history_reads (
    notification_id TEXT NOT NULL,
    device_id       TEXT NOT NULL,
    read_at         TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (notification_id, device_id),
    FOREIGN KEY (notification_id) REFERENCES notification_history(id) ON DELETE CASCADE
);
