# My Goose: the hub on the phone, and what the Pond does for it

My Goose is the middle tab of Goose On The Go. It is where a member finds what the Pond has been
doing for them: news written from their interests, their to-dos and reminders, the links they
asked Goose to watch, the work Goose made for the things in their life, the services the Pond
connects to, and what else Goose can do. This document is the contract between the Pond and the
phone.

## Ground rules

- Every route below is under `/api/v1`, protected, and acts for the **calling member**: the paired
  device's member, else the household's only member, else `409` with `who_are_you` in the error
  (`crate::me::caller_member`). One member never reads or changes another's rows.
- A list read never calls a model. Model work runs on the inference lane (one job at a time,
  never during a turn) and its result is read later; a route that starts work answers `202`.
- Times are RFC 3339 UTC strings. Money is `{"amount": number, "currency": string}`.
- A picture is a Pond path (`/api/v1/report-images/{key}`) the phone fetches with its token, or
  `null`. Pictures come from `ReportImages::for_pages` (a page's `og:image` or Wikipedia's lead
  image), never from a search result.
- Brave's search results are transient: a stored digest or story keeps the model's own words, the
  page addresses and the pictures, never a snippet.
- Every background prompt names the member, says what is already known about them, and is told
  that one question asked once is curiosity, not an interest. Prompts name their sources, give an
  exact JSON schema with each property defined, ask for granular, technical, scored items with
  priorities tailored to the member, and ask for JSON only.

## 0. What this Pond serves

`GET /me/hub` → `{"version":1,"about":bool,"interests":bool,"interview":bool,"news":bool,
"todos":bool,"cart":bool,"connectors":bool,"ideas":bool,"screenshots":bool,"anchors":bool,
"facts":bool,"questions":bool,"captures":bool,"agents":bool,"ledger":bool,"intentions":bool,
"engine_log":bool,"activity":bool,"catalog":bool,"cards":[kind]}`. A Pond without the route
answers 404 and the phone keeps its older My Goose. A key a Pond does not send is a part it
does not serve: `anchors` covers `/me/anchors*` and `/me/deliverables*`, `facts` covers
`/me/facts*` and `/me/dont*`, `agents` is `/me/agents*`, and `engine_log` is `/me/engine/jobs`.

## 0a. About you: the member's profile

What the Pond remembers about the member, under headings, with the member's own hand on it. The
headings, the closed `about` list and the write policy are in
[member-memory.md](member-memory.md).

```
Fact = { "id", "content": "Jerry is allergic to peanuts.", "about": "allergy"|null,
  "sensitive": bool,      // allergy, health, partner: never put in a notice's words
  "shared": bool,         // a household row: shown, not the member's to change
  "created_at", "expires_at"|null }   // expires_at: a passing ("now") fact
```

- `GET /me/about` → `{"headings":[{"segment","heading","facts":[Fact]}], "to_file": n,
  "words":[{"about","heading","one_at_a_time":bool}]}`. Headings come in profile order and only
  when they hold a fact; `to_file` counts older rows the re-sort has yet to file (shown under
  "Other"); `words` is the list a picker offers.
- `POST /me/about` `{content, about, shared?}` → Fact, `201`. The same sentence again answers `200`
  with the row already there. A one-at-a-time `about` closes the member's old row. A word
  outside the list is `400`.
- `PATCH /me/about/{id}` `{content?, about?}` → Fact; `DELETE /me/about/{id}` → `204`. Only the
  member's own rows; a shared row or another member's is `404`.
- `GET /me/about/archived` → `{"facts":[Fact + "why"]}`: rows the re-sort set aside, each with
  its reason. `POST /me/about/{id}/restore` → Fact (`409` when the row is not archived).

## 0b. Goose's own notes

`GET /me/notes` → `{"lessons":[Note], "notes":[Note]}`, `Note = {"id","text","created_at",
"shared": bool}`: what Goose wrote down for itself about working for this member, and its open
follow-ups. `DELETE /me/notes/{id}` → `204`. A member does not write them; Goose does
([agent-documents.md](agent-documents.md)). The soul, the heartbeat's checklist and the
scratchpad's rules are read and saved with `GET/PUT /prompts/{soul|heartbeat|scratchpad}` and
`POST /prompts/{name}/reset`.

## 0b-1. What Goose did by itself

`GET /me/goose/activity` → `{"activity":[Entry], "unseen": n, "seen_at": time|null}`, newest
first, at most 60. `Entry = {"id","kind","summary","detail": text|null,"at","can_undo": bool,
"undone": bool}`. `kind` is `intention` (a job it left itself, carried out or let go),
`errand` (one that looked something up; `detail` is what it found, null when that was nothing)
or `suggestion` (something it asked them to approve). Entries from before intentions keep
their kinds until they age out: `reminder`, `note`, `note_closed` and `watch_paused`.
`summary` is one line written for the member. For an intention it says what became of it:
"Told you: Leave by 7:15. Sent to your phone.", "Offered you a to-do: Renew the insurance.
Asked whether to add it to your to-dos.", "Let go, its time ran out: Ask about the trip".

- `POST /me/goose/activity/seen` → `204`: the member has read the list down to now.
- `POST /me/goose/activity/{id}/undo` → `{"undone":true}`: the reminder is removed, the note
  deleted or the watch resumed. `409` for an entry that cannot be taken back or already was,
  `404` for one that is not theirs.

The heartbeat and the runner of its intentions write entries, after the thing is done. A
look-up that found something sends one notification, category `update`, with
`{"open":"goose_activity"}`; every other entry has a notification of its own or needs none.
Entries are kept 30 days. None of the kinds written now can be taken back; `undo` still
serves the older ones.

What the heartbeat does alone is leave itself [intentions](#intentions): tell the member
something, make again what it made for a plan, look one thing up with reading tools only
(four a day), put a plan's open questions to them, or offer a to-do. It never writes a to-do.
Anything that changes a device, spends, sends or schedules is still a suggestion they approve.

## 0c. The interview

One interview a member, about three minutes: eight fixed sections (you, your people, food and
health, your days, for fun, news, off limits, how Goose talks to you), each one question with
tap answers. A section asks at most one follow-up, chosen in code from the answer ("What are
their names?", "What are you allergic to?"). Every section can be skipped and the interview
resumes where it stopped. Table: `member_interviews` (migration 0094).

No model writes a question, so an answer returns the next one at once. Taps become facts from
sentences written in code. Only what the member typed goes to one model call after the last
answer; an interview of taps alone never calls a model, and a call that fails still leaves
every tap written up. Every drafted fact passes the profile's write policy
([member-memory.md](member-memory.md)); the draft repeats nothing the profile holds, never
offers "has a partner" where the Pond already knows who, and holds one fact for each
one-at-a-time word. Nothing is kept until the member approves.

```
Interview = { "status": "not_started|asking|thinking|review|done",
              "turns": [InterviewTurn],            // as in section 1; the last is the open question
              "heading": "Food and health"|null,   // the section being asked
              "progress": {"done": 2, "total": 8},
              "sections": [{"id","heading"}],
              "facts": [{"id":"f1","about","content"}],   // the draft, once in review
              "interests": [Interest],                    // the draft; ids d1, d2, ...
              "news": "digest|tiles|both|none"|null }
```

- `GET /me/interview` → Interview. While `thinking` the phone polls it every 1.5 s.
- `POST /me/interview` `{}` → starts, resumes, or begins again once done
- `POST /me/interview/answer` `{"choices"?: [string], "text"?, "skip"?: true}` → Interview with
  the next question, `thinking` when the last answer leaves typed words to write up, or
  `review` at once. `422` for an answer with none of the three, `409` when nothing is asked.
- `POST /me/interview/finish` `{}` → stops asking and writes up what there is
- `POST /me/interview/approve` `{"facts":[ids], "interests":[ids], "edits"?: {factId: sentence},
  "news"?: bool}` → `{"facts": n, "interests": n, "news": bool}`, status `done`. Facts go to
  the profile (source `interview`), interests are confirmed with origin `interview`, and unless
  `"news": false` the news answer sets the member's `tiles` and `digest`.

The interests interview below and the setup interview in chat remain for older phones.

## 1. Interests, the interview that builds them, and the portrait

Tables: `interests` (migration 0086), `interest_sightings` (0089). The portrait has no table
of its own: it is read from the member's fact sheet.

```
Interest = { "id", "topic": "Formula 1",
  "category": "sport|tech|science|business|money|politics|culture|film_tv|music|games|books|
               food|travel|health|home|cars|shopping|people|places|learning|other",
  "facets": ["Ferrari", "race-day strategy"],  // the granular part: teams, people, angles
  "depth": "curious|keen|devoted",
  "news": "off|tiles|digest|both",              // whether and how it feeds the news
  "avoid": false,                               // true: never raise it
  "origin": "interview|chat|you", "evidence": 3, "confirmed": true,
  "picture": path|null,   // the topic's own picture (Wikipedia's lead image), when one is found
  "updated_at" }
```

- `GET /me/interests` → `{"interests":[Interest]}` (confirmed first, then guesses by evidence)
- `POST /me/interests` `{topic, category?, facets?, depth?, news?, avoid?}` → Interest (confirmed)
- `PATCH /me/interests/{id}` any writable field, plus `"confirmed": true` → Interest
- `DELETE /me/interests/{id}` → 204

Memory extraction stops turning a one-off question into a trait. What a conversation shows the
member caring about goes to `interests` as an unconfirmed row (or adds one to its `evidence`).
An unconfirmed row is a candidate: it stays listed for the member and feeds nothing. The news,
prompts and the personal-context index read confirmed rows only, never an `avoid` row
(`Interest::feeds_research`). A candidate is ready to be offered for confirming once
conversations on three separate days, in the household's time zone, showed it, or the member
voted a story of it up (`interest::ready_to_offer`, `InterestRepository::ready_to_offer`).

The interview is a chat with tap answers, one question at a time, and digs into each answer
(which team, which writers, how closely they follow it, whether they want news of it).

```
InterviewTurn = { "id", "role": "goose|you", "text", "choices": ["..."], "multi": bool,
                  "allow_free": bool, "at" }
Interview = { "status": "not_started|asking|thinking|review|done",
              "turns": [InterviewTurn], "draft": [Interest], "asked": n, "planned": n }
```

- `GET /me/interests/interview` → Interview
- `POST /me/interests/interview` `{}` → starts, resumes, or begins a new round once done
- `POST /me/interests/interview/answer` `{"text"?, "choices"?: [string]}` → Interview with
  status `thinking` until the next question is written; the phone polls GET every 1.5 s
- `POST /me/interests/interview/finish` `{}` → status `review`, the draft filled
- `POST /me/interests/interview/approve` `{"keep":[draft ids], "edits":{id: partial Interest}}`
  → `{"saved": n}`, status `done`

The portrait is read from the member's fact sheet (`fact_sheet::as_portrait`); no job writes
it. Identity, work and home lines make the summary, interest lines `into`, plans and things
they are looking for `working_on`, and tastes and food `likes`. The news and interview prompts
read it.

- `GET /me/portrait` → `{"summary": "a few sentences", "into": [string], "working_on":
  [string], "likes": [string], "updated_at"}`, or 404 while the fact sheet is empty

### The fact sheet and the don't-list

Tables: `fact_sheet` (migration 0109), `dont_list` (0110). The design is in
[idea-engine.md](idea-engine.md), sections 6.2 and 6.7.

```
Fact = { "id", "category": "identity|home|work|people|plan|hunt|interest|food|health|style|
                            routine|avoid|other",
  "fact": "Lives in Kilimani", "confidence": "sure|likely|said", "since": string|null,
  "source": {"kind": "memory|summary|interest|anchor|answer|interview|you", "ref": id|null},
  "tools": ["get_weather"], "pinned": bool, "score": n, "used": n, "state": "active",
  "created_at", "updated_at" }
Rule = { "id", "rule": "recipes built on a colour preference", "strength": "never|avoid",
  "reasons": ["too_generic|not_relevant|wrong_facts|already_know|bad_timing"],
  "said": string|null, "origin": "reject|stop|fact|you", "origin_ref": id|null, "created_at" }
```

- `GET /me/facts` → `{"facts":[Fact], "limit": 100}`: the active lines, pinned first, then the
  member's own, then by score
- `POST /me/facts` `{category, fact}` → the same list. The line is the member's own: sure, and
  kept whatever a rebuild says. Writing a line they once removed brings it back
- `PATCH /me/facts/{id}` `{fact?, category?, pinned?}` → Fact. Rewording a line makes it the
  member's own; 409 when another line already says that
- `DELETE /me/facts/{id}` → 204. The line is marked removed, so no rebuild learns it again, and
  an `avoid` rule with origin `fact` is added so nothing is built on it
- `POST /me/facts/rebuild` → 202 `{"state":"queued|running"}`. It adds a queued `engine_jobs`
  row of kind `fact_sheet`, class `asked`, with no task, unless the member already has one
  open. Asking for any engine job works this way; the engine's runner picks queued rows up
- `GET /me/dont` → `{"rules":[Rule]}`, `never` rules first, then newest
- `POST /me/dont` `{rule, strength?}` → the same list; `strength` is `never` when left out. A
  rule said twice is one rule, and a `never` is not weakened by a later `avoid`
- `DELETE /me/dont/{id}` → the same list

## 1a. Anchors, what Goose makes for them, and its questions

An anchor is a real thing in the member's life: an event, a trip, a purchase, a project, an
interest. Goose makes deliverables for it and asks questions when details are missing. Tables:
`anchors`, `anchor_shares`, `deliverables` (migration 0111), `engine_jobs` (0112), `questions`
(0113). The design is in [idea-engine.md](idea-engine.md).

```
Anchor = { "id", "kind": "event|trip|purchase|project|interest|standing", "title",
  "state": "candidate|active|ended|stopped|faded",   // candidate: waiting for the member's yes
  "starts_at"|null, "ends_at"|null, "place"|null,
  "facts": [{"label","value","source"}],            // source "you": the member wrote it
  "agent": "trip|work|hunt|budget"|null,             // the specialist that owns it
  "origin": "chat|capture|interest|you|goose", "why": "why Goose offered it",
  "confirmed_at"|null, "ended_at"|null, "created_at", "updated_at",
  "mine": bool, "shared_by": "Jerry"|null,           // the owner's name, on one shared with you
  "shares": [profile_id], "deliverables": [DeliverableSummary], "open_questions": n,
  "job": Job|null }                                  // the job running for it, else the next queued
DeliverableSummary = { "id", "kind": "dossier|plan|shortlist|digest|checklist", "slot", "title",
  "state": "queued|making|ready|unsure|rejected|archived", "hook": "one line", "version": n,
  "changed": "what the last refresh changed", "unseen": bool }
Deliverable = DeliverableSummary + { "anchor_id", "anchor_title", "body": "markdown",
  "facts": [{"label","value","source"}], "sources": [{"title","url"}],
  "objections": [{"kind": "grounding|relevance|duplicate|dont_list|vague|stale", "about", "fix"}],
  "task_id"|null, "refresh_at"|null, "mine": bool, "created_at", "updated_at", "job": Job|null }
Job = { "kind": "deliverable|fact_sheet|memory|consolidation|heartbeat|capture|ledger|rule|
                 questions", "subject": "in words, never an id", "state": "queued|running",
  "rounds": n, "steps"?: [{"key","role","label": "Researching","state","summary"|null}] }
Question = { "id", "text", "options": ["..."], "multi": bool }   // the screen adds "Other"
```

- `GET /me/anchors` → `{"anchors":[Anchor]}`: the candidates and the active ones the member owns
  or is shared with
- `POST /me/anchors` `{kind, title, starts_at?, ends_at?, place?, notes?}` → 201 Anchor, active.
  `kind` is not `standing`. A title the member already has answers 200 with that anchor; one
  Goose offered is confirmed by it; one that faded or was stopped comes back (201); one that
  ended is 409
- In a chat, the model's `plan_for` tool (`giap-schedule`) adds one the same way when the
  member asks Goose to plan, prepare for or keep an eye on something: `{kind, title, when?,
  until?, place?, details?}`, the dates read from the member's words. It answers in one line
  ("I'll work on a plan for your Diani trip and tell you when it's ready."); a guest or an
  unidentified speaker is refused
- `GET /me/anchors/{id}` → Anchor with `"questions":[Question]` and the job's `steps`
- `PATCH /me/anchors/{id}` `{title?, starts_at?, ends_at?, place?, facts?}` → Anchor. A date or
  place sent as `null` is cleared; `facts` replaces the whole list. 409 for a title the member
  has or had, 422 when it would end before it starts
- `POST /me/anchors/{id}/confirm` → Anchor; `POST /me/anchors/{id}/dismiss` → 204 (a
  candidate fades; 409 for an active one)
- `POST /me/anchors/{id}/stop` `{reasons?: [reason], text?, never?: bool}` → 204. An active
  anchor stops, its open jobs are called off, and a reason, words or `never` leave a rule on
  the don't-list. `reason` is one of `too_generic|not_relevant|wrong_facts|already_know|bad_timing`
- `PUT /me/anchors/{id}/shares` `{profile_ids: []}` → Anchor. A member it is shared with reads
  it and answers its questions; every change to it answers them 403
- `GET /me/deliverables/{id}` → Deliverable
- `POST /me/deliverables/{id}/refresh` → 202 `{"state":"queued|running"}`: a job of class
  `asked`, or the open one moved ahead. 409 once it was rejected or its anchor is over
- `POST /me/deliverables/{id}/reject` `{verb: "not_this"|"never", reasons?: [reason], text?}` →
  204. `not_this` leaves an `avoid` rule on its title; `never` with the member's words leaves a
  `never` rule that an engine job then rewrites as one general rule
- `POST /me/deliverables/{id}/seen` → 204; `GET /me/deliverables/{id}/file` → the body as a
  Markdown attachment (404 before anything is written)
- `GET /me/questions` → `{"batches":[{"id","purpose":"anchor|interest|profile|ledger",
  "anchor_id"|null,"title","questions":[Question]}]}`: the open ones, at most four a batch
- `POST /me/questions/{id}/answer` `{choices: [], text?}` → 204. A choice must be one of the
  options and `text` is the member's "Other"; 422 for neither, or two choices where `multi` is
  false; 409 once it is no longer open. An answer for an anchor queues a refresh of what was
  made for it, and one of purpose `profile` a rebuild of the fact sheet
- `POST /me/questions/{id}/skip` → 204

### The engine log

`GET /me/engine/jobs?limit=` (30 when left out, 100 at most) → `{"jobs":[{"id","kind","subject",
"state":"queued|running|passed|unsure|dropped|failed|cancelled","rounds": n,
"objections":[{"kind","about","fix"}],"outcome": "one line"|null,"anchor_id"|null,
"deliverable_id"|null,"created_at","started_at"|null,"finished_at"|null}]}`, newest first: every
maker-and-critic run made for the member, so they can see what Goose worked on and what its
critic held against it.

### Intentions

What the heartbeat means to do for the member: one thing, once a condition holds, until a
deadline (table `intentions`, migration 0114; [idea-engine.md](idea-engine.md) section 8).

```
Intention = { "id", "act": "push|refresh|look_into|ask|propose_todo", "text": "what to do or say",
  "why": "one sentence", "when": "tomorrow at 7am",   // the condition, as the member reads it
  "until", "state": "waiting|done|expired|dropped", "outcome": "what came of it"|null,
  "anchor_id"|null, "created_at", "settled_at"|null }
```

- `GET /me/intentions?state=waiting|settled` → `{"intentions":[Intention]}`, newest first.
  `waiting` when left out; `settled` is the done, expired and dropped ones, 100 at most
- `DELETE /me/intentions/{id}` → 204: a waiting one is withdrawn (`dropped`). 409 for one that
  was carried out or expired

## 1b. Specialists and the Budget ledger

A specialist is shipped with the Pond: it owns some kinds of anchor and adds its playbook to
their briefs. Trips and events, Projects and Purchases are on as shipped; Budget is off, and
Goose offers it once the member has sent three pictures that show money. Tables:
`member_agents`, `ledger_entries` (migration 0116).

```
Agent = { "id": "trip|work|hunt|budget", "title": "Budget", "about": "one sentence",
  "state": "on|off|offered|declined",
  "why": "You have sent three pictures of receipts or payments" }   // offered only, else ""
Entry = { "id", "occurred_on": "2026-11-03", "payee", "amount": "4200.50", "currency": "KES",
  "category": "groceries|eating_out|transport|bills|rent|health|shopping|travel|entertainment|
               transfers|other", "anchor_id"|null, "note"|null }
```

- `GET /me/agents` → `{"agents":[Agent]}`
- `PUT /me/agents/{id}` `{"on": bool}` → the same list. Off answers an offer with `declined`,
  which is never offered again, and otherwise leaves `off`. Turning Budget off, or declining
  it, deletes the money pictures held for it; the ledger is kept
- `GET /me/ledger?month=YYYY-MM` → `{"month","entries":[Entry],"totals":[{"currency","total",
  "by_category":[{"category","total"}]}]}`: the month's entries, newest day first, and one total
  for each currency with its categories largest first. The month it is now where the Pond is
  when left out
- `PATCH /me/ledger/{id}` `{payee?, amount?, category?, occurred_on?, note?, anchor_id?}` →
  Entry. `note` and `anchor_id` sent as `null` are cleared; 422 for an amount that is not a
  plain number, a category outside the list, a date after today or an anchor that is not theirs
- `DELETE /me/ledger/{id}` → 204

An amount is a decimal string in the currency's major unit, with as many places as the
currency has (`"4200.50"` KES, `"50000"` UGX); a refund is negative, and amounts in different
currencies are never added together. `PATCH` takes the same string, or a number. Every ledger
route answers 409 while Budget is not `on`.

## 2. News

```
NewsSettings = { "digest": bool, "digest_time": "07:00", "spoken_on_speaker": bool,
                 "tiles": bool, "timezone": "Africa/Nairobi" }
Story = { "id", "kind": "news|long_read", "headline", "dek": "one line", "picture": path|null,
  "source": {"name","url"}, "interest": {"id","topic","category"}, "published_at"|null,
  "written_at", "read": bool }
StoryDetail = Story + { "summary": "markdown, 120-250 words, in Goose's words",
  "why_you": "one line tying it to the member", "sources": [{"title","url","site"}] }
Digest = { "id", "title", "written_at", "intro": "two sentences",
  "sections": [{ "interest": {"id","topic"}, "heading", "body": "markdown",
                 "pictures": [{"path","caption","source_url"}],
                 "references": [{"title","url","site"}] }],
  "listen": "/api/v1/me/news/digests/{id}/listen" }
```

- `GET /me/news/settings` → NewsSettings; `PUT /me/news/settings` (partial) → NewsSettings
- `GET /me/news` → `{"stories":[Story], "digest": Digest|null, "state": "idle|writing",
  "next_digest_at", "last_written_at"}`
- `GET /me/news/stories/{id}` → StoryDetail
- `GET /me/news/digests` → `{"digests":[{id,title,written_at}]}`; `GET /me/news/digests/{id}`
- `GET /me/news/digests/{id}/listen` → `audio/wav` in the Pond's voice
- `POST /me/news/digests/{id}/speak` → 202, read on the house speaker now
- `POST /me/news/refresh` → 202 `{"state":"writing"}`
- `GET /me/news?interest={id}` → the same shape, only that interest's stories, newest first, up to
  30: the page for one interest
- `POST /me/news/stories/{id}/read` → 204; `GET /me/news/history` → `{"stories":[Story +
  "read_at"]}`, the last 30 days
- `POST /me/news/stories/{id}/feedback` `{"vote":"up|down|wrong", "note"?}` → 204. Up and down
  move that interest's topics at the next topic-map rebuild; `wrong` hides the story and keeps
  the note for the writer's log.

What to look up comes from the member's topic map (`member_topics`, rebuilt daily): about 40
scored web-search queries written from that member's interests, portrait, memories and scheduled
tasks, in batches by area, at most four per subject, weighted by what this member cares about,
with private matters the public web cannot answer dropped. Newsworthy topics feed the tiles and
the digest; the rest feed an occasional long read (a Story of kind `long_read`).

Tiles are refreshed a few times a day, a handful per interest, each a page the Pond read and
summarised itself. The digest is written once a day, ready by `digest_time`, from the day's
stories: a section per interest with its pictures and references. With `spoken_on_speaker`, the
Pond reads the intro and the headings on its speaker at `digest_time`, under the household's
quiet hours and unprompted-speech setting.

## 3. To-dos and reminders, with hooks

Tables: `todos`, `todo_hooks`, `member_places` (migrations 0087 and 0115).

```
Todo = { "id", "kind": "todo|reminder", "text", "notes"|null, "list"|null,
  "priority": "low|normal|high", "steps": [{"text", "done": bool}],
  "goose_can": {"label", "kind": "research|chat|watch", "prompt"}|null, "anchor_id"|null,
  "state": "open|done|dropped", "due_at"|null, "hooks": [Hook], "ask": bool,
  "source": "you|chat|goose|idea", "last_asked_at"|null, "asked": n, "snoozed_until"|null,
  "created_at", "done_at"|null }
Hook = { "id", "kind": "time|repeat|place|event|connector", "label": "plain sentence",
  "spec": {...}, "next_fire_at"|null, "last_fired_at"|null }
  time:      {"at": time}
  repeat:    {"every": "day|weekday|week|month", "at": "18:00", "days": [1..7]?}
  place:     {"place_id", "transition": "arrive|leave"}
  event:     {"source_id", "event"}            // from /me/hooks/sources
  connector: {"connector_id", "event", "filter"|null}
Place = { "id", "label", "lat", "lng", "radius_m" }
```

- `GET /me/todos?state=open|done|all` → `{"todos":[Todo]}`
- `POST /me/todos` `{kind, text, notes?, due_at?, ask?, hooks?: [{kind, spec}], list?,
  priority?, steps?: [{text, done?}], goose_can?: {label, kind, prompt}, anchor_id?}` → 201 Todo,
  or 200 with the Todo already open on the list under the same words, plus `"duplicate_of"`: its id
- `PATCH /me/todos/{id}` (text, notes, due_at, ask, state, list, priority, anchor_id; `hooks` and
  `steps` each replace all; `goose_can: null` clears it) → Todo
- `DELETE /me/todos/{id}` → 204
- `POST /me/todos/{id}/answer` `{"answer":"done|not_yet|drop", "snooze_minutes"?}` → Todo
- `GET /me/hooks/sources` → `{"places":[Place], "sources":[{"id", "kind":
  "presence|camera|device|sensor|web_watch|cart|connector", "label", "events":[{"id","label"}]}]}`
- `POST /me/places` `{label, lat, lng, radius_m?}` → Place; `DELETE /me/places/{id}` → 204
- `POST /me/places/{id}/event` `{"transition":"arrive|leave"}` → 202 (the phone's geofence)
- `POST /hooks/in/{token}` (public; the token is the credential) → 202: a webhook connector's
  event, any JSON body

A list name is at most 40 characters and an empty one is none. A to-do has at most 12 steps of
at most 120 characters each; a step is ticked by sending the whole list again, and ticking the
last one leaves the to-do open for the member to close. `goose_can` is something Goose offers
to do for the to-do: `label` is the button's words, at most 30 characters.

A member keeps one open to-do under a key, its words in lower case without punctuation, so a
reminder and a to-do in the same words are one, and a done or dropped to-do does not count.
Every path that adds one (`POST /me/todos`, `add_reminder`, an accepted offer, an idea's steps)
answers with the one already there instead of a second. When that one has nothing left to fire
(no hook, or only times that have passed) and the new ask names a time or place, it takes them.

When a hook fires, a reminder is pushed to the member's devices. An `ask` to-do is pushed as a
question, `category: "action_required"`, `data: {"action":"todo_checkin","todo_id","text"}`,
which the phone shows with Done and Not yet buttons and answers through `/answer`. Goose asks
again after `not_yet` (at the snooze, else the next morning), at most three times, then leaves
the to-do open and stops asking. The chat model gets `add_reminder`, `list_todos` and `complete_todo` so "remind me
to ... when I get home" makes a row; `add_reminder` also takes a `list`, a `priority` and `steps`.

A dated mention the Pond lifts from a chat ("call the garage on Friday", the `reminders` table) is
offered as a to-do instead of a bare suggestion: a push, `category: "action_required"`,
`data: {"action":"todo_offer","reminder_id","text","when_said"}`, answered with Add or No.

- `GET /me/todos/offers` → `{"offers":[{"reminder_id","text","when_said","due_at"|null,"said_at"}]}`
  — every reminder open to the member: those offered, and those the shared daily cap held back.
  The cap limits the pushes, not what the member can find and answer: Home counts every pending
  reminder, and the Reminders screen lists the same ones.
- `POST /me/todos/offers/{reminder_id}/accept` `{"due_at"?}` → 201 Todo (a time hook at the due
  time the Pond worked out from `when_said`, or the one the phone sends), or 200 with the Todo
  already on the list and `"duplicate_of"`; the offer is settled either way
- `POST /me/todos/offers/{reminder_id}/decline` → 204 (the reminder is dismissed)

Accept and decline settle a held-back reminder as well, and the offer job then never pushes it.

## 4. Cart: watched links and kept listings

```
CartItem = { "id", "url", "title", "picture": path|null, "site",
  "price": Money|null, "previous": Money|null, "lowest": Money|null, "highest": Money|null,
  "target": Money|null, "in_stock": bool|null, "history": [{"at","amount"}],
  "status": "ok|checking|failing", "checked_at"|null, "next_check_at"|null, "every_hours": n,
  "collection"|null, "notify": "drop|target|stock|any" }
```

- `GET /me/cart` → `{"items":[CartItem]}`
- `POST /me/cart` `{url, target?, collection?, every_hours?, notify?}` → CartItem (checking)
- `GET /me/cart/{id}`; `PATCH /me/cart/{id}`; `DELETE /me/cart/{id}` → 204
- `POST /me/cart/{id}/check` → CartItem, checked now

A link is read for its product, price, currency, stock and picture: JSON-LD first, then the
page's price meta tags and microdata, then (only when those find nothing) the model reading the
page. Its price history is kept. A change that matches `notify` pushes to the member
(`data: {"open":"cart","cart_id"}`) and is an event a to-do hook can attach to. Kept listings
(saved items of kind `listing`) stay where they are; the phone shows them beside the links.

## 5. Connectors

```
Connector = { "id", "name", "summary", "icon",
  "category": "mail|calendar|home|web|travel|weather|music|money|custom",
  "status": "connected|available|needs_setup|error", "status_detail"|null,
  "kind": "built_in|account|mcp|webhook",
  "provides": { "tools": n, "events": [{"id","label"}], "feeds": ["news","todos","ideas"] },
  "setup": { "fields": [{"key","label","kind":"text|secret|url|toggle|select","options"?,
             "required","help"?}], "oauth": string|null } }
```

- `GET /me/connectors` → `{"connectors":[Connector]}`: one list over the built-in tool groups,
  mail and calendar accounts, installed and marketplace MCP extensions, web search, Uber, Spotify
  and custom webhooks
- `POST /me/connectors/{id}/connect` `{"fields":{...}}` → Connector
- `POST /me/connectors/{id}/disconnect` → Connector
- `POST /me/connectors/webhook` `{"name"}` → Connector + `{"webhook_url"}`

A connector is a manifest (`docs/CONNECTORS.md`): its id, name, setup fields, the events it can
raise and the tools it brings. A new connector is a manifest plus, where it needs one, an adapter
or MCP server; the phone draws its setup from the fields with no app release.

## 6. Ideas

An idea is a short card that opens to its own page.

```
Idea = { "id",
  "kind": "outing|activity|date|solo_date|trip|recipe|interest|project",
  "kind_label": "Recipe",                      // the card's small label
  "title": "at most eight words",
  "hook": "the card's one line",
  "pitch": "two or three short sentences, for the page",
  "why": "the fact or interest of the member it fits",
  "facts": [{"label": "Serves", "value": "4"}],   // two to four, seen at a glance on the page
  "notes": "what Goose kept to work from",        // the page may show it under "Goose's notes"
  "grounded_in": [{"kind":"interest|memory|conversation|screenshot","label"}],
  "sources": [{"title","url","site"}], "picture": path|null,
  "goose_can": {"label", "kind": "research|watch|todo|schedule|chat"}|null,
  "you_can": ["step", ...],
  "state": "new|working|done|dismissed", "progress": {"task_id"}|null, "created_at" }
```

- `GET /me/ideas` → `{"ideas":[Idea], "state":"idle"}`; `GET /me/ideas/{id}` → Idea
- `POST /me/ideas/{id}/goose` → Idea: Goose starts its part (a research task, a watch, a
  schedule, a to-do) and the idea moves to `working`
- `POST /me/ideas/{id}/mine` → `{"idea": Idea, "todos": [Todo]}`: the steps become to-dos,
  and a new idea moves to `done`, so the phone lists them where its buttons were
- `POST /me/ideas/{id}/dismiss` → 204; `POST /me/ideas/refresh` → 202 `{"state":"idle"}`

No new ideas are written: anchors and their deliverables ([idea-engine.md](idea-engine.md))
replace them. These routes serve the ideas a pond already holds, and the engine purge
(migration 0117) deleted the ones nobody acted on. `state` is always `idle` and `refresh` starts
nothing, so a phone that still asks for ideas shows no error.

### Suggestions on Home

`GET /suggestions` answers with questions derived from what the pond holds, never stored and
never written by a model. Six read what the idea engine holds for the member asking, and only
from anchors they own, but for `questions_waiting`, which also asks the questions of an anchor
shared with them:

| Id | When | Prompt, for example | `because` | `context` |
|---|---|---|---|---|
| `anchor_soon` | An active anchor starts within three days, on the household's clock | "What do I need for MCP Dev Summit on Thursday?" | "MCP Dev Summit starts on Thursday, 19 November." | `temporal`: its title, dates, place and facts |
| `questions_waiting` | A live anchor has open questions | "Answer 3 questions about my Diani trip" | "3 questions about Diani trip are waiting for an answer." | `personal_memory`, `anchor:<id>`: the questions and their options, which the reply puts one at a time |
| `plan_to_confirm` | A candidate anchor waits for the member's yes | "Plan my Lamu trip" | Its `why`: "You mentioned it on 12 November." | `personal_memory`, `anchor:<id>`: the anchor, and to call `plan_for` with its kind and title, which confirms it |
| `changed_since` | A refresh changed a deliverable they have not opened | "What changed in my plan for Diani trip?" | "Updated: Plan for Diani trip. The SGR fare rose to KSh 3,000." | `personal_memory`, `deliverable:<id>`: what changed and the new opening |
| `ready_deliverable` | A first version, or an unsure one, they have not opened | "Tell me about my plan for Diani trip" | "Ready: Plan for Diani trip." | `personal_memory`, `deliverable:<id>`: its title, hook and opening |
| `fact_hint` | An active fact line hints a tool the pond can use now | "What's the weather in Ndenderu this weekend?" | "On your fact sheet: Lives in Ndenderu with family." | `personal_memory`, `fact:<id>`: the line |

```
{"id": "ready_deliverable", "prompt": "Tell me about my plan for Diani trip",
 "because": "Ready: Plan for Diani trip.", "answered_by": "giap-memory", "composed": false,
 "context": {"type": "personal_memory", "source_memory_id": "deliverable:<id>",
             "fact": "its title, hook and opening"}}
```

A card about an anchor with a date comes first, the nearest date first; then, in this order,
`questions_waiting`, `plan_to_confirm`, `changed_since`, `ready_deliverable`, `fact_hint` and the
templates (`calendar_today`, `upcoming_schedule`, `inbox_recent`, `routine_recall`,
`memory_recall`, `devices_online`, `weather_today`). A refresh that changed something is offered
only as `changed_since`, so one deliverable never takes two cards. `fact_hint` turns one line
into a fixed ask by its category and the tool it hints: `home` with weather, `routine` with
weather, `hunt` with `web_watch`, `work` and `food` with `search_web`. The tool must be on (search
also needs its key), the best three such lines take a day each, and the place named is the one
the household set. A prompt is at most 90 characters; a long title is cut to fit, and the
`context` keeps it whole. A pond with no engine data is offered what it was before.

Each counts against the four shown and is muted like the others (`suggestions_muted`). A
deliverable's card goes once the member opens it (`POST /me/deliverables/{id}/seen`), a
candidate's once it is confirmed, and a question's once it is answered or skipped in My Goose:
an answer given in the chat is not written to the question. Sent back as `suggestion_context`
with the tap, the `context` grounds the chat model's reply. `composed` is always `false`, and
`POST /suggestions/{id}/taken` answers 200 with `"settled": false` for any id.

## 6a. Cards sent to chat

Anything the member reads as a card can be sent to chat, where Goose works from what they read
and from its own notes on it. The phone names the card; the Pond reads it as the speaker and
puts both in the turn, so no client writes what the model is told and a card id never reaches
another member's card.

```
Card = { "kind": "Recipe", "title", "content": "what the member read",
         "notes": "what Goose kept: sources, details that did not fit, why it was shown" }
```

- `GET /me/cards/{kind}/{id}` → Card, for `kind` one of `idea`, `news` (a story), `digest`,
  `todo`, `cart`, `watch` (a web watch). `GET /me/hub` lists the kinds this Pond serves under `"cards"`.
- `POST /chat/stream` takes `"card": {"kind","id"}` beside the message. The turn's
  `<system-context>` then carries the card and the notes; later turns of the conversation see
  it in the history. A card that is not the speaker's is left out and the turn runs without it.

An idea's notes are written with the idea, from the pages it was read from. A story's and a
digest's are its sources, why it was shown and when it was written; a to-do's and a watched
link's are their state. Chat suggestions carry their grounding the same way
(`suggestion_context`).

## 6b. Captures: screenshots and photos

The phone sends the member's screenshots, and photos from the albums they chose or shared by
hand. The Pond reads each one once for what it shows and what the member seems to want with
it. Table `captures` (migration 0116); the design is in [idea-engine.md](idea-engine.md),
section 6.5.

- Screenshots are on until the member turns them off; photos are sent only from the albums
  they name, or one at a time by hand. Turning screenshots off deletes the ones still waiting.
- A picture is kept as a file under `<data_dir>/captures/`, in a directory and a file only the
  Pond's own user can open, named by the capture's id and the type its own bytes show. The
  same picture sent twice is kept once. The file is deleted 30 days after it is read; what it
  showed stays.
- A picture that is private (a login, a private conversation, a document) is deleted with
  nothing kept of it: no row, no file. One that shows money is never listed, described or served;
  the Budget agent reads it into the ledger and it is deleted, or it is deleted when the member
  says no to Budget.
- It needs a model that reads pictures; `available` says whether this Pond's does.

```
Capture = { "id", "kind": "screenshot|photo", "album"|null, "taken_at"|null,
  "description": "what it shows"|null, "intent": "what the member seems to want"|null,
  "details": {"names":[],"dates":[],"places":[],"prices":[],"links":[]},
  "has_picture": bool, "created_at" }
Settings = { "screenshots": bool, "albums": ["Receipts"] }
```

- `GET /me/captures` → `{"settings": Settings, "available": bool, "pending": n,
  "captures":[Capture]}`: the ones read, newest first, 200 at most; `pending` counts the ones
  waiting to be read
- `PUT /me/captures/settings` `{screenshots?, albums?}` → Settings. At most 20 albums, each
  name at most 60 characters (422 past either)
- `POST /me/captures` `{"image": base64, "mime_type": "image/jpeg|image/png|image/webp",
  "kind": "screenshot|photo", "album"?, "taken_at"?}` → `202 {"pending": n}`, or `200
  {"pending": n, "capture": Capture|null}` when the member already sent that picture (`null`
  for one that shows money). `409` for a screenshot while screenshots are off, a photo from
  an album the member did not choose, or a model that reads no pictures; `413` over about
  1.5 MB; `415` when the type, or the bytes, are not one of the three; `422` when the picture
  is not base64; `429` when 30 are waiting
- `GET /me/captures/{id}/picture` → the picture, to its member only; 404 once it is gone
- `DELETE /me/captures/{id}` → 204; `DELETE /me/captures` → 204, every capture the member has.
  The picture is deleted first, then the row, and a job reading it is called off

The settings are the member preferences `captures_screenshots` and `captures_albums`. A member
who chose under the older `ideas_from_screenshots` preference keeps that choice until they
choose again.

### The older screenshot routes

A phone build without captures calls `/me/screenshots`. The routes keep their shapes and are
served from the member's captures of kind `screenshot`: a note is a read screenshot's
description, `enabled` is the screenshots setting, and at most forty notes are listed.

```
Screenshots = { "enabled": bool, "available": bool, "pending": n, "max_pending": 30,
                "notes": [{"id","note","taken_at"|null,"created_at","used_at": null}] }
```

- `GET /me/screenshots` → Screenshots; `PUT /me/screenshots` `{"enabled": bool}` → Screenshots
- `POST /me/screenshots` `{"image": base64, "mime_type": "image/jpeg|image/png|image/webp",
  "taken_at"?}` → `202 {"pending": n}`, for a screenshot already sent as well. The refusals are
  those of `POST /me/captures`
- `DELETE /me/screenshots/{id}` → 204; `DELETE /me/screenshots` → `{"deleted": n, "dropped": n}`:
  every screenshot the member sent, read or waiting, and no photo

## 7. What else Goose can do

```
CatalogItem = { "id", "title", "body", "icon", "state": "on|off|needs_setup",
  "try": "a prompt to try"|null,
  "opens": "news|todos|cart|connectors|ideas|interests|agents|web_watches|cameras|voice|
            models|settings"|null }
```

- `GET /me/catalog` → `{"items":[CatalogItem]}`, built from what this Pond has switched on
