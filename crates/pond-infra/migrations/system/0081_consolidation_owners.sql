-- Consolidation wrote every merge and split with no owner, so a member's merged facts became the
-- whole household's. Give each such row the owner its sources had; one that two members' facts
-- went into is archived, since neither may see the other's half. Nothing is deleted.

-- What consolidation wrote: a `consolidated` event with no data (recategorising carries data).
CREATE TEMP TABLE repair_written AS
SELECT DISTINCT e.memory_id AS id
FROM memory_events e
JOIN memory_fragments m ON m.id = e.memory_id
WHERE e.event_kind = 'consolidated' AND e.data IS NULL AND m.profile_id IS NULL;

-- A merge links each source to its result. A split links only its first piece, so a piece written
-- in the same second as a one-source result is taken to come from that source too.
CREATE TEMP TABLE repair_parent AS
SELECT s.superseded_by AS child, s.id AS parent
FROM memory_fragments s
WHERE s.superseded_by IS NOT NULL
UNION
SELECT piece.id, src.id
FROM repair_written piece
JOIN memory_fragments pm ON pm.id = piece.id
JOIN memory_fragments first_piece
  ON first_piece.created_at = pm.created_at AND first_piece.id <> piece.id
JOIN memory_fragments src ON src.superseded_by = first_piece.id
WHERE NOT EXISTS (SELECT 1 FROM memory_fragments x WHERE x.superseded_by = piece.id)
  AND (SELECT count(*) FROM memory_fragments y WHERE y.superseded_by = first_piece.id) = 1;

-- Each written row's original sources, through any number of later merges and splits.
CREATE TEMP TABLE repair_owners AS
WITH RECURSIVE lineage(row_id, ancestor) AS (
    SELECT w.id, p.parent FROM repair_written w JOIN repair_parent p ON p.child = w.id
    UNION
    SELECT l.row_id, p.parent FROM lineage l JOIN repair_parent p ON p.child = l.ancestor
)
SELECT l.row_id AS id,
       COUNT(DISTINCT m.profile_id) AS members,
       MAX(m.profile_id) AS member
FROM lineage l
JOIN memory_fragments m ON m.id = l.ancestor
WHERE NOT EXISTS (SELECT 1 FROM repair_parent p WHERE p.child = l.ancestor)
GROUP BY l.row_id;

-- One member's facts, alone or with the household's: the member's. Narrowing never discloses.
UPDATE memory_fragments
SET profile_id = (SELECT member FROM repair_owners o WHERE o.id = memory_fragments.id)
WHERE id IN (SELECT id FROM repair_owners WHERE members = 1);

-- Two members' facts in one row: nobody's to read.
UPDATE memory_fragments
SET lifecycle = 'archived'
WHERE lifecycle = 'active'
  AND id IN (SELECT id FROM repair_owners WHERE members > 1);

-- No source found: the only member's on a pond of one, else archived.
UPDATE memory_fragments
SET profile_id = (SELECT id FROM profiles)
WHERE id IN (SELECT id FROM repair_written)
  AND id NOT IN (SELECT id FROM repair_owners)
  AND (SELECT count(*) FROM profiles) = 1;

UPDATE memory_fragments
SET lifecycle = 'archived'
WHERE lifecycle = 'active'
  AND profile_id IS NULL
  AND id IN (SELECT id FROM repair_written)
  AND id NOT IN (SELECT id FROM repair_owners)
  AND (SELECT count(*) FROM profiles) > 1;

DROP TABLE repair_owners;
DROP TABLE repair_parent;
DROP TABLE repair_written;
