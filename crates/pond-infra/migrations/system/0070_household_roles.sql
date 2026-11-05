-- Household roles: owner, member, guest, developer. Permission checks (can this caller
-- remove a device, change the model, enroll remote access, reset the Pond, or control a
-- device outside its guest allow-list) need to know a profile's role; nothing has one today.

ALTER TABLE profiles ADD COLUMN role TEXT NOT NULL DEFAULT 'member';

-- Only meaningful when role = 'guest'; NULL means the access never expires on its own.
ALTER TABLE profiles ADD COLUMN guest_until TEXT;

-- `settings.primary_profile_id` is already the one trusted "whose Pond is this" signal from
-- onboarding (used to attribute the claiming device to its owner in `attribute_claimant_to_owner`).
-- Unlike attributing a *device* to a member from this key (deliberately refused in
-- 0043_device_profile.sql, since one key can't safely stand in for which of several phones is
-- whose), naming the *household's owner* from it is exactly what it already means: there is only
-- ever one primary profile per pond.
UPDATE profiles SET role = 'owner'
WHERE id = (SELECT value FROM settings WHERE key = 'primary_profile_id');

-- Defends a pond with no `primary_profile_id` row (shouldn't happen post-onboarding) and
-- exactly one profile: that profile is unambiguously the owner.
UPDATE profiles SET role = 'owner'
WHERE role != 'owner'
  AND (SELECT COUNT(*) FROM profiles) = 1
  AND (SELECT value FROM settings WHERE key = 'primary_profile_id') IS NULL;

-- Per-guest device allow-list: a profile with role = 'guest' starts with no devices.
CREATE TABLE IF NOT EXISTS guest_device_access (
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    device_id  TEXT NOT NULL,
    PRIMARY KEY (profile_id, device_id)
);
