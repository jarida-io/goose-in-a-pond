-- A device's private notices belong to the member it had when they went out. Re-pairing it to
-- someone else, or losing its member, must not hand that member's history to the new holder.
CREATE TRIGGER IF NOT EXISTS notification_history_recipients_follow_member
AFTER UPDATE OF profile_id ON devices
WHEN OLD.profile_id IS NOT NEW.profile_id
BEGIN
    DELETE FROM notification_history_recipients WHERE device_id = NEW.id;
END;

-- Deleting a member sets its devices' profile_id to NULL by foreign-key action; drop their rows
-- here first rather than rely on that action firing the trigger above.
CREATE TRIGGER IF NOT EXISTS notification_history_recipients_leave_with_member
BEFORE DELETE ON profiles
BEGIN
    DELETE FROM notification_history_recipients
    WHERE device_id IN (SELECT id FROM devices WHERE profile_id = OLD.id);
END;
