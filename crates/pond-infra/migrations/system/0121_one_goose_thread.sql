-- One Goose thread per person. A household routine's answer used to go to a separate
-- `goose-household` thread, so a household of two read two "Goose" conversations, and the first
-- member to answer in that thread was bound to it and became the only one who could read it.
-- Results that are nobody's in particular now go to every member's own thread
-- (goose_thread.rs :: post_to_household); this folds the old thread in once.
--
-- Bound to a member: it was already theirs alone, so its messages move to their thread as they
-- are. Unbound: everyone could read it, so each member, guests aside, gets a copy. The
-- household thread then goes. With no member to give it to, it stays as it is.

INSERT OR IGNORE INTO sessions
    (id, created_at, updated_at, title, profile_id, identification_source, surface)
SELECT 'goose-' || p.id, h.created_at, h.updated_at, 'Goose', p.id, 'explicit', 'goose'
  FROM sessions h
  JOIN profiles p
    ON p.id = h.profile_id
    OR (h.profile_id IS NULL AND p.role <> 'guest')
 WHERE h.id = 'goose-household';

INSERT INTO session_messages
    (id, session_id, role, content, tool_call_id, tool_calls_json, created_at, liked, note_json)
SELECT lower(hex(randomblob(16))), 'goose-' || p.id, m.role, m.content, m.tool_call_id,
       m.tool_calls_json, m.created_at, m.liked, m.note_json
  FROM session_messages m
  JOIN sessions h ON h.id = m.session_id AND h.profile_id IS NULL
  JOIN profiles p ON p.role <> 'guest'
 WHERE m.session_id = 'goose-household';

UPDATE session_messages
   SET session_id = 'goose-' || (SELECT profile_id FROM sessions WHERE id = 'goose-household')
 WHERE session_id = 'goose-household'
   AND (SELECT profile_id FROM sessions WHERE id = 'goose-household') IS NOT NULL;

-- A thread that just received the household's latest answer sorts by it.
UPDATE sessions
   SET updated_at = max(updated_at, (SELECT updated_at FROM sessions WHERE id = 'goose-household'))
 WHERE id IN (
       SELECT 'goose-' || p.id
         FROM sessions h
         JOIN profiles p
           ON p.id = h.profile_id
           OR (h.profile_id IS NULL AND p.role <> 'guest')
        WHERE h.id = 'goose-household');

DELETE FROM sessions
 WHERE id = 'goose-household'
   AND EXISTS (
       SELECT 1
         FROM sessions h
         JOIN profiles p
           ON p.id = h.profile_id
           OR (h.profile_id IS NULL AND p.role <> 'guest')
        WHERE h.id = 'goose-household');
