# The idea engine: anchors, deliverables and the fact sheet

What Goose offers a member unprompted, how it is made, and how it is checked before the member
sees it. This replaces the composed question chips, the ideas job, the member portrait and the
heartbeat's notes and reminders.

## 1. Why

A look at one pond's data on 2026-11-16 found:

| Generator | What it held | What was wrong |
|---|---|---|
| Question chips (`suggestion_queue`) | 281 rows, none ever expire | Generic "What are the latest X?" questions; the same topic up to five times |
| Ideas (`ideas`) | 30, 8 dismissed | A duck recipe built on a colour preference; three ideas on one physics term |
| Heartbeat reminders (`todos`, source `goose`) | One reminder added 7 times in 5 days | It was already a daily schedule; nothing checked for a duplicate |
| Heartbeat notes (`agent_notes`) | The same rain note 6 times | Never closed, never acted on |
| Interests | 93, none confirmed | Every question asked became an interest |
| Portrait, topic map | One each | Described what the member asked, not who they are |

Three causes sit under all of it: nothing checked output against what already existed, quality
was checked by format only, and the profile every generator read was built from one-off questions.

## 2. What it is

- **Done-for-you results.** Goose offers finished work (a dossier, a plan, a shortlist, a digest),
  not questions the member could ask.
- **Anchors.** An anchor is a real thing in the member's life: an event, a trip, a purchase, a
  project, an interest. It holds the facts. Deliverables chain off it and stay live until it ends.
- **Maker and critic.** Every background job that needs the model has a maker and an adversarial
  critic. The critic vetoes with objections; the maker revises. Past the time cap a deliverable is
  shown marked unsure with the objections; anything Goose would keep about the member is dropped.
- **One fact sheet per member.** At most 100 typed lines, each with a source and a tool hint. It
  replaces the portrait. Background jobs read all of it; a chat turn gets the relevant lines.
- **A don't-list.** The member rejects with a reason. Every maker and critic reads the list.
- **Structured questions.** When details are missing Goose asks up to four questions, each with
  options and a free-text "Other". Answers feed the work at once and are kept for later plans.
- **A strict heartbeat.** The heartbeat's notes are intentions: do X once condition C holds, until
  a deadline. Code checks the condition. A met intention is carried out and cleared; a late one is
  cleared. It never writes a to-do; it may propose one.
- **Autonomy.** Research, drafting, checklists and notes run unasked. Booking, sending, spending,
  scheduling and device changes still need the member's yes (proposals, unchanged).

## 3. Objects

| Object | Table | Owner | Notes |
|---|---|---|---|
| Fact line | `fact_sheet` | member | 100 active lines at most |
| Don't rule | `dont_list` | member | `never` rules are always read; `avoid` rules by relevance |
| Anchor | `anchors`, `anchor_shares` | member, shared per anchor | `candidate` until the member confirms |
| Deliverable | `deliverables` | its anchor | One per recipe slot of the anchor |
| Engine job | `engine_jobs` | member | One maker-critic run; most are also a long task |
| Question | `questions` | member | Grouped by `batch_id`, four to a batch |
| Intention | `intentions` | member | The heartbeat's own jobs |
| Capture | `captures` | member | A screenshot or photo from the phone |
| Ledger entry | `ledger_entries` | member | The Budget specialist's records |
| Specialist switch | `member_agents` | member | Which specialists are on |

## 4. Schema

Migrations `0109` to `0117` under `crates/pond-infra/migrations/system/`. Timestamps are RFC 3339
text written by the application. Keys (`fact_key`, `rule_key`, `title_key`, `text_key`) come from
`interest::topic_key`.

```sql
-- 0109_fact_sheet.sql
CREATE TABLE IF NOT EXISTS fact_sheet (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    category    TEXT NOT NULL CHECK (category IN ('identity','home','work','people','plan','hunt',
                    'interest','food','health','style','routine','avoid','other')),
    fact        TEXT NOT NULL CHECK (length(trim(fact)) > 0),
    fact_key    TEXT NOT NULL,
    confidence  TEXT NOT NULL DEFAULT 'likely' CHECK (confidence IN ('sure','likely','said')),
    since       TEXT,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('memory','summary','interest','anchor',
                    'answer','interview','you')),
    source_ref  TEXT,
    tools_json  TEXT NOT NULL DEFAULT '[]',
    pinned      INTEGER NOT NULL DEFAULT 0,
    score       REAL NOT NULL DEFAULT 0,
    used        INTEGER NOT NULL DEFAULT 0,
    state       TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','retired','removed')),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (profile_id, fact_key)
);
CREATE INDEX IF NOT EXISTS idx_fact_sheet_member ON fact_sheet(profile_id, state, score);

-- 0110_dont_list.sql
CREATE TABLE IF NOT EXISTS dont_list (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    rule         TEXT NOT NULL CHECK (length(trim(rule)) > 0),
    rule_key     TEXT NOT NULL,
    strength     TEXT NOT NULL CHECK (strength IN ('never','avoid')),
    reasons_json TEXT NOT NULL DEFAULT '[]',
    said         TEXT,
    origin       TEXT NOT NULL CHECK (origin IN ('reject','stop','fact','you')),
    origin_ref   TEXT,
    created_at   TEXT NOT NULL,
    UNIQUE (profile_id, rule_key)
);

-- 0111_anchors.sql
CREATE TABLE IF NOT EXISTS anchors (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('event','trip','purchase','project','interest','standing')),
    title        TEXT NOT NULL CHECK (length(trim(title)) > 0),
    title_key    TEXT NOT NULL,
    state        TEXT NOT NULL DEFAULT 'candidate'
                 CHECK (state IN ('candidate','active','ended','stopped','faded')),
    starts_at    TEXT,
    ends_at      TEXT,
    place        TEXT,
    facts_json   TEXT NOT NULL DEFAULT '[]',
    agent        TEXT,
    origin       TEXT NOT NULL CHECK (origin IN ('chat','capture','interest','you','goose')),
    origin_ref   TEXT,
    why          TEXT NOT NULL DEFAULT '',
    offered_at   TEXT,
    confirmed_at TEXT,
    ended_at     TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    UNIQUE (profile_id, title_key)
);
CREATE INDEX IF NOT EXISTS idx_anchors_member ON anchors(profile_id, state, starts_at);
CREATE TABLE IF NOT EXISTS anchor_shares (
    anchor_id  TEXT NOT NULL REFERENCES anchors(id) ON DELETE CASCADE,
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    PRIMARY KEY (anchor_id, profile_id)
);
CREATE TABLE IF NOT EXISTS deliverables (
    id              TEXT PRIMARY KEY,
    anchor_id       TEXT NOT NULL REFERENCES anchors(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('dossier','plan','shortlist','digest','checklist')),
    slot            TEXT NOT NULL,
    title           TEXT NOT NULL CHECK (length(trim(title)) > 0),
    state           TEXT NOT NULL DEFAULT 'queued'
                    CHECK (state IN ('queued','making','ready','unsure','rejected','archived')),
    hook            TEXT NOT NULL DEFAULT '',
    body            TEXT NOT NULL DEFAULT '',
    facts_json      TEXT NOT NULL DEFAULT '[]',
    sources_json    TEXT NOT NULL DEFAULT '[]',
    objections_json TEXT NOT NULL DEFAULT '[]',
    changed         TEXT NOT NULL DEFAULT '',
    version         INTEGER NOT NULL DEFAULT 0,
    task_id         TEXT,
    refresh_at      TEXT,
    seen_at         TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE (anchor_id, slot)
);

-- 0112_engine_jobs.sql
CREATE TABLE IF NOT EXISTS engine_jobs (
    id              TEXT PRIMARY KEY,
    task_id         TEXT,
    profile_id      TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('deliverable','fact_sheet','memory','consolidation',
                        'heartbeat','capture','ledger','rule','questions')),
    class           TEXT NOT NULL CHECK (class IN ('asked','deadline','upkeep','open')),
    anchor_id       TEXT,
    deliverable_id  TEXT,
    subject         TEXT NOT NULL DEFAULT '',
    due_at          TEXT,
    state           TEXT NOT NULL DEFAULT 'queued'
                    CHECK (state IN ('queued','running','passed','unsure','dropped','failed','cancelled')),
    rounds          INTEGER NOT NULL DEFAULT 0,
    objections_json TEXT NOT NULL DEFAULT '[]',
    outcome         TEXT,
    created_at      TEXT NOT NULL,
    started_at      TEXT,
    finished_at     TEXT
);
CREATE INDEX IF NOT EXISTS idx_engine_jobs_open ON engine_jobs(state, class, due_at);
CREATE INDEX IF NOT EXISTS idx_engine_jobs_task ON engine_jobs(task_id);

-- 0113_questions.sql
CREATE TABLE IF NOT EXISTS questions (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    batch_id     TEXT NOT NULL,
    purpose      TEXT NOT NULL CHECK (purpose IN ('anchor','interest','profile','ledger')),
    anchor_id    TEXT REFERENCES anchors(id) ON DELETE CASCADE,
    subject_ref  TEXT,
    text         TEXT NOT NULL CHECK (length(trim(text)) > 0),
    text_key     TEXT NOT NULL,
    options_json TEXT NOT NULL DEFAULT '[]',
    multi        INTEGER NOT NULL DEFAULT 0,
    parent_id    TEXT,
    state        TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open','answered','skipped','expired')),
    choices_json TEXT NOT NULL DEFAULT '[]',
    other_text   TEXT,
    asked_at     TEXT,
    answered_at  TEXT,
    expires_at   TEXT,
    created_at   TEXT NOT NULL,
    UNIQUE (profile_id, text_key)
);
CREATE INDEX IF NOT EXISTS idx_questions_member ON questions(profile_id, state, batch_id);

-- 0114_intentions.sql
CREATE TABLE IF NOT EXISTS intentions (
    id              TEXT PRIMARY KEY,
    profile_id      TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    act             TEXT NOT NULL CHECK (act IN ('push','refresh','look_into','ask','propose_todo')),
    text            TEXT NOT NULL CHECK (length(trim(text)) > 0),
    text_key        TEXT NOT NULL,
    why             TEXT NOT NULL DEFAULT '',
    condition_kind  TEXT NOT NULL CHECK (condition_kind IN ('time','place','thing','chat')),
    condition_json  TEXT NOT NULL,
    condition_label TEXT NOT NULL,
    anchor_id       TEXT REFERENCES anchors(id) ON DELETE CASCADE,
    until           TEXT NOT NULL,
    state           TEXT NOT NULL DEFAULT 'waiting' CHECK (state IN ('waiting','done','expired','dropped')),
    outcome         TEXT,
    created_at      TEXT NOT NULL,
    settled_at      TEXT
);
CREATE INDEX IF NOT EXISTS idx_intentions_waiting ON intentions(state, condition_kind, until);

-- 0115_todo_structure.sql
ALTER TABLE todos ADD COLUMN list TEXT;
ALTER TABLE todos ADD COLUMN priority TEXT NOT NULL DEFAULT 'normal';
ALTER TABLE todos ADD COLUMN steps_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE todos ADD COLUMN goose_can_json TEXT;
ALTER TABLE todos ADD COLUMN anchor_id TEXT;

-- 0116_captures.sql
CREATE TABLE IF NOT EXISTS captures (
    id            TEXT PRIMARY KEY,
    profile_id    TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('screenshot','photo')),
    album         TEXT,
    taken_at      TEXT,
    state         TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','read')),
    sensitive     TEXT NOT NULL DEFAULT 'none' CHECK (sensitive IN ('none','money')),
    hot           INTEGER NOT NULL DEFAULT 0,
    description   TEXT,
    intent        TEXT,
    details_json  TEXT NOT NULL DEFAULT '{}',
    picture_file  TEXT,
    picture_until TEXT,
    content_hash  TEXT,
    attempts      INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (profile_id, content_hash)
);
CREATE TABLE IF NOT EXISTS ledger_entries (
    id           TEXT PRIMARY KEY,
    profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    occurred_on  TEXT NOT NULL,
    payee        TEXT NOT NULL,
    amount_minor INTEGER NOT NULL,
    currency     TEXT NOT NULL,
    category     TEXT NOT NULL DEFAULT 'other',
    anchor_id    TEXT,
    note         TEXT,
    created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ledger_member ON ledger_entries(profile_id, occurred_on);
CREATE TABLE IF NOT EXISTS member_agents (
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    agent      TEXT NOT NULL CHECK (agent IN ('budget','work','trip','hunt')),
    state      TEXT NOT NULL CHECK (state IN ('on','off','offered','declined')),
    changed_at TEXT NOT NULL,
    PRIMARY KEY (profile_id, agent)
);

-- 0117_engine_purge.sql: section 12
```

## 5. Engine jobs

### 5.1 The format

An engine job is a long task whose id starts with `engine-`, plus one `engine_jobs` row. Its steps
are `long_task_steps`; what its steps produce are `long_task_artifacts`. The one long-task runner
advances it. Member task lists leave `engine-` tasks out: they are read through the engine's own
routes.

Two kinds of step run:

| Step | Role | How it runs |
|---|---|---|
| Helper | planner, worker, composer (the existing long-task roles) | A fresh agent session with tools, as today. Used to research and write a deliverable |
| Inline | `engine-make`, `engine-critic`, `engine-revise` | One direct model call whose prompt the code builds; the reply is saved as an artifact. No tools |

An inline step needs no orchestrator, so the runner starts without one and takes the lane for a
job of inline steps whatever `ext_orchestrator_enabled` and `long_tasks_enabled` say. A helper
step on a pond with no orchestrator fails with that reason.

Every model call of a job runs on the inference bus's Task lane. A chat turn takes the model at
the next token; the cut call is asked again when the turn ends.

Two jobs keep their own slot on the inference lane and are not long tasks, because they must not
stall on a pond where long tasks are off: memory extraction and consolidation. Extraction gains
the critic (section 6.3). Consolidation already has a proposer, an adversary and a judge. Each
pass writes an `engine_jobs` row with no `task_id`, so both show in the same log.

### 5.2 Queue order

`long_task_queue::run_order` takes the open tasks and the open engine jobs, and orders:

1. Tasks the member started (any task that is not an engine job or an activation) and engine jobs
   of class `asked`, oldest first.
2. Agent activations, oldest first.
3. Engine jobs of class `deadline`, nearest `due_at` first.
4. Engine jobs of class `upkeep`, oldest first.
5. Engine jobs of class `open`, oldest first.

The order is read before every step, so a higher job takes over between steps.

### 5.3 The critic loop

When a job's steps are all settled the runner asks the engine what comes next, before it would
mark the task complete:

| State of the job | Next |
|---|---|
| A draft with no verdict on it | Add an `engine-critic` step |
| Verdict `pass` | Settle as `passed` |
| Verdict `fail`, time left, fewer than `MAX_ROUNDS` (3) rounds | Add an `engine-revise` step; its draft gets a new critic step |
| Verdict `fail`, out of time or rounds | Settle as `unsure` (a deliverable) or `dropped` (anything else) |

The critic's prompt holds the brief, the draft, the source notes, the whole fact sheet, the
don't-list and the titles of what already exists for the anchor, filled to the window. It answers
JSON only:

```json
{"verdict": "pass" | "fail",
 "objections": [{"kind": "grounding" | "relevance" | "duplicate" | "dont_list" | "vague" | "stale",
                 "about": "the claim or line", "fix": "what would make it pass"}]}
```

A reply that does not parse counts as `fail` with one objection saying so. The critic never writes
the output.

Time caps: a deliverable job has 60 minutes; an inline job has 15. The existing downtime rule
moves a deadline by the time the pond was off.

### 5.4 Settling

| Kind | On `passed` | On `unsure` or `dropped` |
|---|---|---|
| `deliverable` | The deliverable takes the draft, its sources and hook; state `ready`; version + 1; the member is told (section 9) | State `unsure` with the objections shown; told the same way |
| `fact_sheet` | The sheet is replaced within its rules (section 6.2) | The sheet is left as it was |
| `heartbeat` | The surviving intentions are stored | Nothing is stored |
| `capture` | The capture's description, intent and details are saved | Retried once, then the picture is deleted |
| `ledger` | The entries are saved; a hot capture is deleted whole | Nothing saved; the hot capture is deleted whole |
| `rule` | The don't rule is saved | The member's own words are saved as the rule |
| `questions` | The batch is stored and pushed | No questions are asked |

Every settle writes the job's `state`, `rounds`, `objections_json` and a one-line `outcome`.

## 6. Job kinds

### 6.1 Deliverable

Made as a research long task (kind `research`, answer `report`), so the planner splits it, workers
read the web to their window and the composer writes it. A worker of an engine task may fill 60%
of the window with sources (`reading_room`: 0.6 for an `engine-` task, 0.45 otherwise).

The opening brief artifact holds, in this order: today's date and the member's place; the anchor
(kind, title, dates, place, facts); the recipe for this slot (what the deliverable must contain);
the specialist's playbook when one owns the anchor; the whole fact sheet; every `never` rule and
the ten most relevant `avoid` rules; the member's answers for this anchor and the most relevant
answers from other anchors; relevant captures; the titles and hooks of the anchor's other
deliverables; and, on a refresh, the last version with "say what changed".

Recipes, by anchor kind (`slot`: kind, what it must contain):

| Anchor | Slots, in order |
|---|---|
| `event` | `dossier`: dossier (what, when, where; weather on the day; agenda and the sessions that fit the member; people worth meeting; getting there; what to bring) |
| `trip` | `plan`: plan (days, transport, stays, budget table, essentials); `stays`: shortlist (options compared with prices and links); `packing`: checklist |
| `purchase` | `shortlist`: shortlist (listings worth a look: price, specs, why each fits, links); `costs`: plan (total cost: duty, fees, running costs) |
| `project` | `digest`: digest (what is new since the last version: releases, news, people, deadlines, and why each matters to the member) |
| `interest` | `digest`: digest |
| `standing` | Set by its owner: the everyday anchor has `out`: shortlist (named places and events near the member this week); the budget anchor has `month`: digest |

A later slot is queued when the one before it is `ready`. A refresh is due by how near the anchor
is: more than 14 days out, weekly; within 14 days, every 3 days; within 3 days, daily; no date,
weekly. `changed` holds what the refresh changed; an unchanged refresh tells nobody.

### 6.2 Fact sheet

Inline maker, inline critic, class `upkeep`. Due once a day, after five new memories, and after an
interview or an answered batch.

The maker sees numbered sources (the member's most important and most recent memories, their
conversation summaries, confirmed interests, active anchors, answers), the current sheet, the
names and one-line uses of the tools this pond has, and the `never` rules. It answers a JSON list
of at most 100 lines: `{category, fact, confidence, since, sources: [numbers], tools: [names]}`.

Code, not the model, then enforces:

- a line with no source number that exists is dropped;
- a tool name this pond does not have is removed from the line;
- a line whose key the member removed is never added back;
- pinned lines and lines the member wrote are always kept;
- the 100 best by score stay active (score: pinned, then confidence, evidence, recency and `used`).

The critic drops lines that restate a one-off question, describe the pond itself, duplicate
another line or lack support in their numbered sources.

`MemberPortrait` stays as a type, derived from the sheet (`fact_sheet::as_portrait`), for the news
writer, the interest interview and `GET /me/portrait`. Nothing writes `member_portraits`.

### 6.3 Memory extraction

The extraction prompt gains a `plans` list beside interests, memories and reminders: concrete
plans the member stated (`{kind, title, when, where, details}`). Before a window's results are
saved, a critic call sees the window and the candidates and names the ones to drop: not said by
the member, a one-off lookup, about the pond, already known. Kept plans become candidate anchors.

### 6.4 Heartbeat

Section 8.

### 6.5 Capture

Inline, with the picture attached to both the maker and the critic. The maker answers
`{description, intent, details: {names, dates, places, prices, links}, sensitive, plan}`.

| `sensitive` | What happens |
|---|---|
| `private` (a login, a private conversation, a document) | The picture and the row are deleted; nothing is kept |
| `money` | Marked hot, and nothing about it is indexed. With the Budget specialist on, a `ledger` job reads it and then the picture and the row are deleted. With it off, up to three are held until the member answers the offer to turn it on; a decline deletes them, and later ones are deleted at once |
| `none` | Description, intent and details are kept and indexed; the picture is kept 30 days |

A `plan` in the reply becomes a candidate anchor.

### 6.6 Questions

Inline, class as its anchor's. The maker sees the anchor, the fact sheet and the stored answers
and writes at most four questions, each with two to four options. The critic drops a question the
facts or stored answers already settle. "Other" is always offered by the surfaces, never listed as
an option. Research does not wait for answers: it states its assumptions, and an answer queues a
refresh of class `asked`. An answer that is free text may produce one follow-up batch
(`parent_id`).

Batches are also used to confirm an interest (seen on three separate days, or liked) and to fill a
gap in the fact sheet (purpose `profile`, one batch a day at most).

### 6.7 Rule from a rejection

"Never this" with the member's words runs an inline job that writes one general rule ("Never:
recipes built on a colour preference"). "Not this one" stores `Avoid: <title>` with the reason
chips and needs no model.

## 7. What starts a job

A tick every minute, with no model call:

| When | What is queued |
|---|---|
| An anchor is confirmed | A `questions` job, and its first slot |
| A slot becomes `ready` | The next slot |
| `refresh_at` has passed | A refresh of that deliverable (`deadline` when the anchor has a date, else `open`) |
| An answer arrives, or the member taps refresh | A refresh, class `asked` |
| An intention with act `refresh` fires | A refresh, class `deadline` |
| The fact sheet is due | A `fact_sheet` job |
| A capture is pending | A `capture` job |
| An anchor's end has passed by a day | The anchor ends: deliverables are archived, its engine tasks and their artifacts are deleted, its facts and deliverable bodies are kept |
| A candidate anchor is a week old | It fades |

One open job per deliverable, per member's fact sheet and per capture. Two open jobs of class
`open` per member at most.

Anchors come from: a plan in a chat (candidate), a plan in a capture (candidate), the
`plan_for` tool when the member asks Goose to plan or track something (active), and the member
adding one (active). A confirmed interest gets no anchor of its own: a standing topic with no
date, place or question gave only vague weekly digests. An interest marked avoided stops an
anchor made for it. Each member has one standing everyday anchor.

## 8. Intentions: the heartbeat

The hourly review keeps its place on the lane and its reviewer role. Its answer changes: each
element is an intention or, for something only the member can approve, a proposal as before.

```json
{"do": "intend", "act": "push" | "refresh" | "look_into" | "ask" | "propose_todo",
 "text": "what to do or say", "when": {"time": "3 December at 8am"}, "until": "the deadline",
 "about": "words from an anchor's title, or omit", "why": "one sentence"}
```

`when` is one of four shapes, chosen from what the brief lists, so the code never guesses what a
phrase means. It refuses what it cannot check:

| Kind | Shape | How it is checked |
|---|---|---|
| `time` | `{"time": "3 December at 8am"}` | `when_said::due_from_words`; fires when the time has come |
| `place` | `{"place": "office", "on": "arrive"}` | A saved place of the member, by its label, and a crossing reported by their phone |
| `thing` | `{"thing": "gate camera", "event": "package"}` | A camera, sensor or device from the hook catalogue, by its label, and an event it lists |
| `chat` | `{"chat": true}` | The member's next turn |

An intention with no checkable condition, no deadline, a deadline already past, or the same key as
a waiting one, an open to-do or a schedule is refused. The survivors go to a critic call, which
drops any that only remark on what happened, concern someone being at home, or serve no anchor,
event or stated wish.

A runner checks conditions with no model call: time on a 30-second tick, the rest as events
arrive. A met intention is carried out and marked `done`; one past `until` is marked `expired`.
Both are logged to the member's Goose activity. The review's brief lists waiting intentions only.

| Act | What happens |
|---|---|
| `push` | A notification now, and a line in the Goose thread |
| `refresh` | Queues the anchor's deliverable |
| `look_into` | The errand runner looks it up and tells the member |
| `ask` | Pushes the anchor's open questions |
| `propose_todo` | Offers a to-do the member adds or declines (the existing offer) |

The heartbeat no longer writes reminders, and no longer keeps or closes notes. A note the chat
model left with `note_to_self` is shown to the review once, to turn into an intention, and is then
deleted.

## 9. Telling the member

| Event | Push | Elsewhere |
|---|---|---|
| A deliverable is ready, or a refresh changed it | `info`, opens the deliverable; spoken at the pond when unread | Goose thread line; Home |
| A candidate anchor | `action_required` with Yes and No (`anchor_offer`) | Home |
| A question batch | `action_required`, opens the questions | Anchor page |
| An intention's `push` | `alert` | Goose thread line |

`OpenScreen` gains `Anchor`, `Deliverable` and `Questions`.

## 10. Retrieval

No new corpus. `PERSONAL_SOURCE` gains limbs, each with a row-id prefix: `fact:` (active fact
lines, with their tool hints in the text), `dont:` (rules), `answer:` (answered questions with
their answers), `anchor:` (title and facts of active anchors), `deliverable:` (title and hook of
ready ones) and `capture:` (description and details where `sensitive` is `none`). The `portrait:`
and `topic:` limbs are removed. A chat turn's personal context therefore carries the relevant fact
lines; makers and critics are handed the whole sheet.

## 11. Routes

All under `/api/v1/me`, resolved by `caller_member`.

| Method and path | Body | Answer |
|---|---|---|
| `GET /anchors` | | `{anchors: [Anchor]}`, candidates and active, each with its deliverables' summaries, open question count and running job |
| `POST /anchors` | `{kind, title, starts_at?, ends_at?, place?, notes?}` | 201 `Anchor` |
| `GET /anchors/{id}` | | `Anchor` with facts, deliverables, questions and the running job's steps |
| `PATCH /anchors/{id}` | `{title?, starts_at?, ends_at?, place?, facts?}` | `Anchor` |
| `POST /anchors/{id}/confirm` | | `Anchor` |
| `POST /anchors/{id}/dismiss` | | 204 |
| `POST /anchors/{id}/stop` | `{reasons: [], text?, never?}` | 204 |
| `PUT /anchors/{id}/shares` | `{profile_ids: []}` | `Anchor` |
| `GET /deliverables/{id}` | | `Deliverable` with body, facts, sources, objections, changed |
| `POST /deliverables/{id}/refresh` | | 202 |
| `POST /deliverables/{id}/reject` | `{verb: "not_this" \| "never", reasons: [], text?}` | 204 |
| `POST /deliverables/{id}/seen` | | 204 |
| `GET /deliverables/{id}/file` | | The body as a Markdown file |
| `GET /facts`, `POST /facts` | `{category, fact}` | `{facts: [Fact], limit: 100}` |
| `PATCH /facts/{id}`, `DELETE /facts/{id}` | `{fact?, category?, pinned?}` | `Fact`, 204 |
| `POST /facts/rebuild` | | 202 |
| `GET /dont`, `POST /dont`, `DELETE /dont/{id}` | `{rule, strength}` | `{rules: [Rule]}` |
| `GET /questions` | | `{batches: [{id, purpose, anchor_id, title, questions: [{id, text, options, multi}]}]}` |
| `POST /questions/{id}/answer` | `{choices: [], text?}` | 204 |
| `POST /questions/{id}/skip` | | 204 |
| `GET /captures` | | `{settings, available, pending, captures: [Capture]}` |
| `PUT /captures/settings` | `{screenshots?, albums?}` | settings |
| `POST /captures` | `{image, mime_type, kind, album?, taken_at?}` | 202 |
| `GET /captures/{id}/picture` | | The picture |
| `DELETE /captures/{id}`, `DELETE /captures` | | 204 |
| `GET /agents`, `PUT /agents/{id}` | `{on}` | `{agents: [{id, title, about, state, why}]}` |
| `GET /ledger?month=`, `PATCH /ledger/{id}`, `DELETE /ledger/{id}` | | `{entries, totals}` |
| `GET /engine/jobs?limit=` | | Recent jobs with kind, subject, state, rounds, objections, outcome |
| `GET /intentions?state=`, `DELETE /intentions/{id}` | | `{intentions: [Intention]}` |

Existing routes that change: the to-do view and its `POST` and `PATCH` gain `list`, `priority`,
`steps`, `goose_can` and `anchor_id`; `/me/screenshots` becomes an alias over captures; `GET
/me/hub` lists the new parts; `GET /suggestions` no longer returns composed questions and gains
cards derived from anchors, questions, deliverables and the fact sheet ([my-goose-hub.md,
Suggestions on Home](my-goose-hub.md#suggestions-on-home)).

Reason chips, the same on every surface: `too_generic`, `not_relevant`, `wrong_facts`,
`already_know`, `bad_timing`.

## 12. What is removed, and the purge

Removed: the composed-suggestion lane job and its setting, the ideas job, the portrait job, and
the heartbeat's `remind`, `note` and `done` acts. `/me/ideas` keeps answering for ideas already
acted on.

`0117_engine_purge.sql`, run once by the migration:

```sql
DELETE FROM suggestion_queue WHERE state = 'queued';
DELETE FROM todo_hooks
    WHERE todo_id IN (SELECT id FROM todos WHERE source = 'goose' AND state = 'open');
DELETE FROM todos WHERE source = 'goose' AND state = 'open';
DELETE FROM agent_notes WHERE kind = 'note';
DELETE FROM ideas WHERE state = 'new';
DELETE FROM member_topics;
DELETE FROM member_portraits;
INSERT INTO captures (id, profile_id, kind, taken_at, state, sensitive, description, created_at,
                      updated_at)
    SELECT s.id, s.profile_id, 'screenshot', s.taken_at, 'read', 'none', s.note, s.created_at,
           s.created_at
    FROM screenshot_notes s
    WHERE NOT EXISTS (SELECT 1 FROM captures c WHERE c.id = s.id)
      AND EXISTS (SELECT 1 FROM profiles p WHERE p.id = s.profile_id);
```

The `suggestion_queue`, `idea_runs` and `screenshot_notes` tables stay; a later change may drop
them.

Interests keep their rows: with `feeds_research` now true only for a confirmed interest, the
unconfirmed ones are candidates and feed nothing. The topic map rebuilds from the fact sheet on
its next pass. Back up the data directory before the first start on this version.

## 13. To-dos

A to-do gains a list, a priority (`low`, `normal`, `high`), steps (`[{text, done}]`), something
Goose can do for it (`{label, kind, prompt}`) and an anchor. `add_reminder` takes `list`,
`priority` and `steps`, and its description asks for a full action as the text. Every path that
adds a to-do refuses one whose key matches an open to-do of the same member.

## 14. Specialists

A specialist is a shipped constant: an id, a title, the anchor kinds it owns, a playbook added to
the briefs of those anchors, and what it reads.

| Id | Owns | Reads | Keeps | Default |
|---|---|---|---|---|
| `trip` | `trip`, `event` | | | on |
| `work` | `project` | | | on |
| `hunt` | `purchase` | | | on |
| `budget` | the standing budget anchor | Money captures | `ledger_entries` | off; offered after three money captures |

## 15. Surfaces

Built after the rest, with Jarida Ink as published (`@jarida/ink` 1.1.0) and no change to it.

**Desktop and kiosk** (`pond-desktop`, both shells, one shared screen with a `variant`): a My Goose
section with anchors and their deliverables, the running job's steps (`InkTimeline`), question
batches (`InkRadioGroup` or `InkChoice` plus a text field for Other), reject buttons with reason
chips (`InkChip`), the fact sheet (pin, edit, remove) and the don't-list. A pond with more than
one member and no known caller points to the phone.

**Phone** (`goose-on-the-go`, its own repository and pull request): Home leads with ready
deliverables, candidate anchors and open questions; anchor and deliverable screens; the fact sheet
and don't-list under My Goose; the new to-do fields; screenshots sent automatically and photos
from chosen albums or the share sheet.

## 16. Not in this change

- **A background step's cache saved to disk when a chat turn arrives.** A turn already takes the
  model at the next token and the step's call is asked again; what is lost is the step's prefill.
  Keeping it needs a second cache slot inside the Goose engine and a measurement on the Orin.
- **Reading local folders** into the work specialist's context. The pond has no folder ingestion
  to build on; the specialist reads what the index already holds, the member's connectors and the
  web.
- **Embedding the pictures themselves.** EmbeddingGemma 2 puts text, images, audio and video in
  one space, so a picture could be found by how it looks and a text query could match it
  directly. The pond's llama.cpp fork carries only the first EmbeddingGemma, the GGUF embedder's
  width check refuses the second (`n_embd` 512 against a pooled 768), and its vision projector is
  a 555 MB file beside the 310 MB text model, which needs a memory measurement on the Orin. Until
  then a capture is found by its description and details, which the text embedder indexes.

## 17. Build order

Each part ends with the checks for the crates it touched, and a commit.

| Part | What | Crates |
|---|---|---|
| 1 | Migrations 0109 to 0116, domain types, ports, mocks, SQLite stores | `pond-core`, `pond-infra` |
| 2 | Fact sheet (prompts, parse, rules), don't-list, interest gate, retrieval limbs, the derived portrait, routes for facts and the don't-list | `pond-core`, `pond-infra`, `pond-api` |
| 3 | Engine: jobs, queue order, inline steps, the critic loop, settling, deliverable recipes, the tick, anchor and deliverable routes, notices; the ideas, portrait and composed-suggestion jobs removed | `pond-core`, `pond-server`, `pond-api`, `pond-mcp-server` |
| 4 | Intentions and the new heartbeat answer; to-do structure and the duplicate check; the extraction critic and plans; the purge migration | `pond-core`, `pond-infra`, `pond-server`, `pond-api`, `pond-mcp-server` |
| 5 | Questions, captures, the ledger, specialists, the `plan_for` tool, the remaining routes, the hub contract | all of the above |
| 6 | Whole-tree checks, `scripts/live-test.sh`, documentation | |
| 7 | Desktop and kiosk surfaces | `pond-desktop` |
| 8 | Phone surfaces | `goose-on-the-go` |
