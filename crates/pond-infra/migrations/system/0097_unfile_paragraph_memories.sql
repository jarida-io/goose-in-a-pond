-- A memory is one fact. Notes that consolidation ran together into a paragraph go back to the
-- re-sort, which splits each into its sentences, files those and archives the paragraph.
UPDATE memory_fragments
SET about = NULL, resort_attempts = 0
WHERE (lifecycle IS NULL OR lifecycle = 'active')
  AND source = 'extraction'
  AND length(trim(content)) > 300;
