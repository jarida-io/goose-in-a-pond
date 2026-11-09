# My Goose: the hub on the phone, and what the Pond does for it

My Goose is the middle tab of Goose On The Go. It is where a member finds what the Pond has been
doing for them: news written from their interests, their to-dos and reminders, the links they
asked Goose to watch, ideas Goose researched for them, the services the Pond connects to, and what
else Goose can do. This document is the contract between the Pond and the phone.

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

`GET /me/hub` → `{"version":1,"interests":bool,"news":bool,"todos":bool,"cart":bool,
"connectors":bool,"ideas":bool,"catalog":bool}`. A Pond without the route answers 404 and the phone
keeps its older My Goose.

## 1. Interests, the interview that builds them, and the portrait

Tables: `interests`, `member_portraits` (migration 0086).

```
Interest = { "id", "topic": "Formula 1",
  "category": "sport|tech|science|business|money|politics|culture|film_tv|music|games|books|
               food|travel|health|home|cars|shopping|people|places|learning|other",
  "facets": ["Ferrari", "race-day strategy"],  // the granular part: teams, people, angles
  "depth": "curious|keen|devoted",
  "news": "off|tiles|digest|both",              // whether and how it feeds the news
  "avoid": false,                               // true: never raise it
  "origin": "interview|chat|you", "evidence": 3, "confirmed": true, "updated_at" }
```

- `GET /me/interests` → `{"interests":[Interest]}` (confirmed first, then guesses by evidence)
- `POST /me/interests` `{topic, category?, facets?, depth?, news?, avoid?}` → Interest (confirmed)
- `PATCH /me/interests/{id}` any writable field, plus `"confirmed": true` → Interest
- `DELETE /me/interests/{id}` → 204

Memory extraction stops turning a one-off question into a trait. What a conversation shows the
member caring about goes to `interests` as an unconfirmed row (or adds one to its `evidence`);
the phone offers guesses with `evidence >= 2` for the member to confirm. The news and ideas read
confirmed rows and guesses with `evidence >= 3`, never an `avoid` row.

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

The portrait is the background model's account of the member, rewritten on the lane once a day
or after the interests change. The news and ideas prompts read it.

- `GET /me/portrait` → `{"summary": "two or three sentences", "into": [topic], "working_on":
  [string], "likes": [string], "updated_at"}`, or 404 before the first is written

## 2. News

```
NewsSettings = { "digest": bool, "digest_time": "07:00", "spoken_on_speaker": bool,
                 "tiles": bool, "timezone": "Africa/Nairobi" }
Story = { "id", "kind": "news|long_read", "headline", "dek": "one line", "picture": path|null,
  "source": {"name","url"}, "interest": {"id","topic"}, "published_at"|null, "written_at" }
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

What to look up comes from the member's topic map (`member_topics`, rebuilt daily): about 40
scored web-search queries written from that member's interests, portrait, memories and scheduled
tasks, in batches by area, at most four per subject, weighted by what this member cares about,
with private matters the public web cannot answer dropped. Newsworthy topics feed the tiles and
the digest; the rest feed ideas and an occasional long read (a Story of kind `long_read`).

Tiles are refreshed a few times a day, a handful per interest, each a page the Pond read and
summarised itself. The digest is written once a day, ready by `digest_time`, from the day's
stories: a section per interest with its pictures and references. With `spoken_on_speaker`, the
Pond reads the intro and the headings on its speaker at `digest_time`, under the household's
quiet hours and unprompted-speech setting.

## 3. To-dos and reminders, with hooks

Tables: `todos`, `todo_hooks`, `member_places` (migration 0087).

```
Todo = { "id", "kind": "todo|reminder", "text", "notes"|null, "state": "open|done|dropped",
  "due_at"|null, "hooks": [Hook], "ask": bool, "source": "you|chat|goose|idea",
  "last_asked_at"|null, "asked": n, "snoozed_until"|null, "created_at", "done_at"|null }
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
- `POST /me/todos` `{kind, text, notes?, due_at?, ask?, hooks?: [{kind, spec}]}` → Todo
- `PATCH /me/todos/{id}` (text, notes, due_at, ask, state; `hooks` replaces all) → Todo
- `DELETE /me/todos/{id}` → 204
- `POST /me/todos/{id}/answer` `{"answer":"done|not_yet|drop", "snooze_minutes"?}` → Todo
- `GET /me/hooks/sources` → `{"places":[Place], "sources":[{"id", "kind":
  "presence|camera|device|sensor|web_watch|cart|connector", "label", "events":[{"id","label"}]}]}`
- `POST /me/places` `{label, lat, lng, radius_m?}` → Place; `DELETE /me/places/{id}` → 204
- `POST /me/places/{id}/event` `{"transition":"arrive|leave"}` → 202 (the phone's geofence)
- `POST /hooks/in/{token}` (public; the token is the credential) → 202: a webhook connector's
  event, any JSON body

When a hook fires, a reminder is pushed to the member's devices. An `ask` to-do is pushed as a
question, `category: "action_required"`, `data: {"action":"todo_checkin","todo_id","text"}`,
which the phone shows with Done and Not yet buttons and answers through `/answer`. Goose asks
again after `not_yet` (at the snooze, else the next morning), at most three times, then leaves
the to-do open and stops asking. The chat model gets `todo` tools (add, list, done) so "remind me
to ... when I get home" makes a row.

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

```
Idea = { "id", "title", "pitch": "two or three sentences",
  "why": "the specific thing about the member it builds on",
  "grounded_in": [{"kind":"interest|memory|conversation","label"}],
  "sources": [{"title","url","site"}], "picture": path|null,
  "goose_can": {"label", "kind": "research|watch|todo|schedule|chat"}|null,
  "you_can": ["step", ...],
  "state": "new|working|done|dismissed", "progress": {"task_id"}|null, "created_at" }
```

- `GET /me/ideas` → `{"ideas":[Idea], "state":"idle|thinking"}`
- `POST /me/ideas/{id}/goose` → Idea: Goose starts its part (a research task, a watch, a
  schedule, a to-do) and the idea moves to `working`
- `POST /me/ideas/{id}/mine` → `{"idea": Idea, "todos": [Todo]}`: the steps become to-dos
- `POST /me/ideas/{id}/dismiss` → 204; `POST /me/ideas/refresh` → 202

Ideas replace the one-question-per-memory suggestions. Once a day on the lane, the Pond picks one
or two interests or things the member is working on, searches for something new about them,
reads the pages, and writes an idea that is specific to the member, sourced, and doable.

## 7. What else Goose can do

```
CatalogItem = { "id", "title", "body", "icon", "state": "on|off|needs_setup",
  "try": "a prompt to try"|null,
  "opens": "news|todos|cart|connectors|ideas|interests|agents|web_watches|cameras|voice|
            models|settings"|null }
```

- `GET /me/catalog` → `{"items":[CatalogItem]}`, built from what this Pond has switched on
