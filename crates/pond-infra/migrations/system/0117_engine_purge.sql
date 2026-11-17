-- What the retired generators left that no member asked for or acted on. A member's own to-dos,
-- done ones, lessons, ideas they acted on and interests stay.
DELETE FROM suggestion_queue WHERE state = 'queued';

-- Hooks first, so a connection without foreign keys does not leave them behind.
DELETE FROM todo_hooks
    WHERE todo_id IN (SELECT id FROM todos WHERE source = 'goose' AND state = 'open');
DELETE FROM todos WHERE source = 'goose' AND state = 'open';

DELETE FROM agent_notes WHERE kind = 'note';
DELETE FROM ideas WHERE state = 'new';
DELETE FROM member_topics;
DELETE FROM member_portraits;

-- A screenshot's line becomes a read capture with no picture. A note whose member is gone would
-- fail the captures foreign key and stop the start.
INSERT INTO captures (id, profile_id, kind, taken_at, state, sensitive, description, created_at,
                      updated_at)
    SELECT s.id, s.profile_id, 'screenshot', s.taken_at, 'read', 'none', s.note, s.created_at,
           s.created_at
    FROM screenshot_notes s
    WHERE NOT EXISTS (SELECT 1 FROM captures c WHERE c.id = s.id)
      AND EXISTS (SELECT 1 FROM profiles p WHERE p.id = s.profile_id);
