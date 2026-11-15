-- Household roles: owner, member, guest, developer. Permission checks (can this caller
-- remove a device, change the model, enroll remote access, reset the Pond, or control a
-- device outside its guest allow-list) need to know a profile's role; nothing has one today.

ALTER TABLE profiles ADD COLUMN role TEXT NOT NULL DEFAULT 'member';

-- Only meaningful when role = 'guest'; NULL means the access never expires on its own.
ALTER TABLE profiles ADD COLUMN guest_until TEXT;

-- The owner is the pairing lock's main member; a pond whose seat is still empty falls back to
-- `settings.primary_profile_id`, which is what the server names the seat from at startup. A
-- cleared primary is stored as '', not NULL, so it counts as unset.
UPDATE profiles SET role = 'owner'
WHERE id = COALESCE(
    (SELECT owner_profile_id FROM pairing_lock WHERE id = 1),
    (SELECT NULLIF(value, '') FROM settings WHERE key = 'primary_profile_id')
);

-- A pond with exactly one profile and no owner named above: that profile is unambiguously it.
UPDATE profiles SET role = 'owner'
WHERE (SELECT COUNT(*) FROM profiles) = 1
  AND NOT EXISTS (SELECT 1 FROM profiles WHERE role = 'owner');

-- Per-guest device allow-list: a profile with role = 'guest' starts with no devices.
CREATE TABLE IF NOT EXISTS guest_device_access (
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    device_id  TEXT NOT NULL,
    PRIMARY KEY (profile_id, device_id)
);
