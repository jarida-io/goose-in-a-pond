# Idea cards: structured ideas and suggestions

What Goose offers a member unprompted is a set of **cards**. Each card is short and structured, and has a source behind it. A card is readable in five seconds and carries one action. Long research reports from the engine in [idea-engine.md](idea-engine.md) are no longer made unprompted. They are made only when the member taps **Go deeper**.

## 1. A card

| Field | Required | What it holds |
|---|---|---|
| `kind` | yes | `do` (a thing to do), `next_step` (the next step on something the member is doing), `reminder` (a reminder or to-do to add), `ask` (a question the member could send Goose) |
| `title` | yes | At most 70 characters, in plain words |
| `why` | **yes** | `{text, source}`. `text` is one line to the member, starting "You". `source` is `{kind, label, url?}`. `kind` is one of `memory`, `fact`, `plan`, `interest`, `todo`, `web`. `label` names the thing it builds on |
| `when`, `where` | one of these four groups | When and where it applies, in words ("Saturday 2 pm", "Karura Forest") |
| `cost`, `effort` | one of these four groups | The price, the time it takes, how hard it is ("KSh 600 entry", "2 hours") |
| `action` | one of these four groups | `{kind, label, prompt?, text?, due?, url?}`. `kind` is one of `todo`, `reminder`, `chat`, `watch`, `go_deeper` |
| `sources` | yes, at least one | `[{kind, title, url?}]`. Every factual detail on the card comes from these |
| `notes` | yes | `{found, next, caveats}`: what Goose found, what it would do next, and what it is unsure of. Shown when the card is opened, and given to the chat model as context |
| `anchor` | no | `{id, title}`: the plan the card belongs to |
| `state` | | `new`, `saved`, `done` or `dismissed` |
| `batch_id`, `created_at`, `fades_at` | | A card fades 36 hours after its set is made, unless it is saved |

"One of these four groups" means a card must also carry at least one of the following: when or where, cost or effort, or an action.

The code refuses a card that has:

- no `why`;
- no source;
- a source number the maker was never shown;
- none of when/where, cost/effort or an action;
- an action of an unknown kind.

Nothing on a card may come from the model's own knowledge alone.

## 2. Making a set

A set is one engine job of kind `cards`. It runs on the inference bus's Task lane, as an inline long task (see idea-engine.md section 5). The job makes these model calls:

1. **Queries.** One call reads the member's brief and writes up to 3 web searches that would ground "things to do" and next steps. Examples: an event this week near their place, or something one of their plans needs. Code runs them with the pond's web search and keeps the top passages.
2. **Maker.** One call sees the numbered sources and writes up to 12 candidate cards as JSON, each citing source numbers. The sources are:
   - fact-sheet lines;
   - active plans with their dates and places;
   - open to-dos;
   - confirmed interests;
   - recent memories;
   - the web passages from step 1;
   - today's date, the member's place and the weather.

   The maker is also given the don't-list and the titles of the member's cards from the last 7 days, so it does not repeat them.
3. **Critic.** One call over the whole set. It names cards to drop: vague, repeated, unsupported by their sources, against the don't-list, or not about this member.

Code then:

- validates each card against section 1;
- ranks the cards, best first;
- keeps 5 to 8 of them, with at most 3 of one kind and at least one `next_step` when the member has an active plan.

It then stores the set. A new set replaces the member's unsaved `new` cards.

**When a set is made:**

- Each morning at 06:00 in the member's time zone.
- When the member taps refresh (`POST /me/cards/refresh`, class `asked`).
- Never twice at once for one member.

**Push:** a card whose `when` falls today, and that has an action, is pushed once, with category `info`, opening `cards`.

## 3. Actions

| Kind | Tap "Do it" |
|---|---|
| `todo` | Adds the to-do through `todo_add::add_once`, with the card's text and steps |
| `reminder` | Adds a reminder at `due` through `add_once` |
| `chat` | Returns the prompt for the surface to send as the member's message, with the card's notes as context |
| `watch` | Starts a web watch on `url` |
| `go_deeper` | Finds or makes the plan (`anchor_add::add_asked`, active), then queues its first deliverable (class `asked`). The report pipeline runs only here |

"Do it" marks the card `done`. "Not this" marks it `dismissed` and writes an `avoid` rule with the reason chips and the member's words, using the same chips as everywhere else. "Save" keeps the card past its fade time.

## 4. Routes (under `/api/v1/me`)

| Method and path | Body | Answer |
|---|---|---|
| `GET /cards` | | `{cards: [Card], set: {state: "none"\|"queued"\|"making"\|"ready", made_at}}`, with new and saved cards, newest set first |
| `POST /cards/refresh` | | 202 `{state}` |
| `POST /cards/{id}/do` | | `{card, result: {kind, todo?, prompt?, anchor_id?, watch_id?}}` |
| `POST /cards/{id}/save` and `POST /cards/{id}/unsave` | | `Card` |
| `POST /cards/{id}/dismiss` | `{reasons: [], text?}` | 204 |

`GET /me/hub` gains `"cards": true`.

`GET /suggestions` leads with the member's `ask` cards as chips. Each chip's prompt is the card's `action.prompt` or `title`, and its `because` is the card's `why.text`.

## 5. Surfaces

The same card everywhere:

- **Collapsed:** kind, title, why, and the one or two structured fields it has. Buttons: **Do it** (labelled with the action), **Save**, **Not this**, **Go deeper**.
- **Opened:** the notes and the sources.

Where cards appear:

- phone Home (top, with refresh);
- My Goose (phone and desktop);
- the desktop and kiosk;
- chat chips (`ask` cards).
