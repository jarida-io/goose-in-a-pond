-- Suggestions the pond composed, rather than picked from a list.
--
-- ── What was on Home before this ───────────────────────────────────────────
--
-- Two tiers answer `GET /api/v1/suggestions`. The model-backed one is the
-- proactive reviewer, which writes `drafts` rows -- and on a real household
-- pond it had never written one, because `audience_for_review` needed a
-- conversation carrying a `profile_id` and 0 of 961 sessions carried one. So
-- the column always fell through to the other tier: `suggestion.rs`, a pure
-- function that picks among SEVEN FIXED PROMPT STRINGS and attaches a measured
-- count to the one it picks.
--
-- That tier is correct and it is not going away -- it needs no inference, so it
-- answers on a pond with no model at all. But its questions never vary. A
-- household with mail, memories and devices reads the same three sentences
-- every day forever, which is what a household calls a placeholder, and they
-- are right.
--
-- This table holds the other kind: a question composed from one of THEIR
-- memories, generated on the inference lane while nobody is talking, and
-- queued so that rendering Home costs no inference at all.
--
-- ── The model writes the question and NOTHING else ─────────────────────────
--
-- `reason` is computed by the pond from the source memory, never by the model.
-- That is the whole difference between this and a generated blurb: the sentence
-- under the question is a fact about the store, so the offer stays falsifiable
-- (DESIGN.md section 3). A model that writes its own justification can write a
-- true-sounding one about a memory that does not exist.
--
-- `source_memory_id` is what makes that checkable, and it is load-bearing at
-- READ time as well as write time: a queued row whose memory has since been
-- deleted, archived or superseded is not offered. No FK, because
-- `memory_fragments` rows are deleted by the household and by decay and a
-- CASCADE would silently empty this table without anyone being able to see it
-- had happened; the read-side check is visible and testable.
--
-- ── One live row per memory ────────────────────────────────────────────────
--
-- The partial unique index is what stops a pass that runs every idle period
-- from queueing the same question about the same memory a hundred times. It is
-- partial on `state = 'queued'` deliberately: once a household has taken or
-- dismissed a suggestion, a LATER pass may compose a different question about
-- the same memory, and the dismissal of the old one is a fact about the old
-- one. What it must not do is offer two at once.
--
-- ── Against a database that already has rows ───────────────────────────────
--
-- A new table; every existing pond gets an empty one. No backfill: nothing
-- composed a suggestion before this migration, so there is nothing to recover,
-- and the honest starting state is empty. The template tier keeps answering
-- until the first generation pass lands.

CREATE TABLE IF NOT EXISTS suggestion_queue (
    id                TEXT PRIMARY KEY,

    -- Who it is for. NULL means the memory it came from is not attributed, so
    -- the suggestion belongs to the household. The READ path is what enforces
    -- audience -- a personal memory's question is never offered to a shared
    -- audience -- and this column is what it reads.
    profile_id        TEXT,

    -- The sentence the household reads AND the prompt sent when they tap it.
    -- One column, because two would let the card promise something other than
    -- what it does.
    prompt            TEXT NOT NULL CHECK (length(trim(prompt)) > 0),

    -- The fact underneath, written by the pond from `source_memory_id`.
    reason            TEXT NOT NULL CHECK (length(trim(reason)) > 0),

    -- The memory this was composed from. Checked at read time; see above.
    source_memory_id  TEXT NOT NULL,

    -- The tool group whose tools would answer it, so a suggestion is never
    -- offered on a pond whose extension manager does not carry that group.
    answered_by       TEXT NOT NULL,

    created_at        TEXT NOT NULL DEFAULT (datetime('now')),

    -- queued  : offerable
    -- taken   : the household tapped it
    -- dismissed: the household said no, and the ledger remembers
    state             TEXT NOT NULL DEFAULT 'queued'
                      CHECK (state IN ('queued', 'taken', 'dismissed'))
);

CREATE UNIQUE INDEX IF NOT EXISTS suggestion_queue_one_live_per_memory
    ON suggestion_queue (source_memory_id)
    WHERE state = 'queued';

-- The read path is "the newest queued rows for this audience", so the index
-- carries state first and time second.
CREATE INDEX IF NOT EXISTS suggestion_queue_offerable
    ON suggestion_queue (state, created_at DESC);
