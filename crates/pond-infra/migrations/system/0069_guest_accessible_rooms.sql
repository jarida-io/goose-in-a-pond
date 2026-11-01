-- Rooms a guest (an unidentified speaker in a multi-member household) may use devices in.
--
-- `ProfileScope::Guest` carries no per-guest identity, so this is a household-wide list rather
-- than per-guest grants: presence of a row means "guests may control devices placed in this
-- room." A room with no row here is off-limits to guests, including devices with no room at all.

CREATE TABLE IF NOT EXISTS guest_accessible_rooms (
    room_id TEXT PRIMARY KEY REFERENCES rooms(id) ON DELETE CASCADE
);
