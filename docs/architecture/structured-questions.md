# Structured questions

When Goose needs a detail from a member, it asks a **structured question** instead of writing a question in prose. Two things ask:

- the chat model, mid-turn, through the `ask` tool;
- background jobs (plans, interests, the fact sheet), as batches.

Both use the same shape and the same store, and are shown with the same card. This replaces the question parts of [idea-engine.md section 6.6](idea-engine.md).

## 1. A question

| Field | Required | What it holds |
|---|---|---|
| `header` | yes | A chip of at most 12 characters naming what is asked ("Dates", "Budget") |
| `text` | yes | One question to the member, as "you", at most 200 characters |
| `options` | yes, 2 to 4 | `[{label, description, preview?}]`. `label` is at most 60 characters; `description` is one line on what choosing it means |
| `recommended` | no | The index of Goose's pick. The surfaces put it first and mark it "Recommended" |
| `multi` | | True when several options can be chosen together |
| `gap` | background only | The plan detail the question settles (section 3) |

A batch holds 1 to 4 questions. Every surface offers "Other" with a text box beside the options, so no option may mean "Other".

`preview` is one of the following, and appears only when options are hard to tell apart from their labels:

| `preview.kind` | Holds |
|---|---|
| `draft` | `text`: at most 600 characters of what the option leads to, such as the first lines of an itinerary |
| `place` | `name`, `price?`, `url?`, `image_url?`, taken from a web source |
| `table` | `columns` and `rows`: option against option on cost, time and distance; at most 4 columns and 6 rows |

## 2. Rules that code enforces

Code refuses a question that:

- has fewer than 2 or more than 4 options;
- has an option meaning "Other", or two options with the same key;
- has no header, or has a text or label over its limit;
- repeats a question already asked in the chain, or already stored for the member. "Repeats" means the same `text_key`, or the embedder scores the two texts as alike (cosine at least 0.86);
- is settled by a stored answer. The embedder matches the question against answered questions; the refusal hands back the stored answer.

A batch over 4 questions is cut to 4.

## 3. Background: code picks the gap, the model words it

The model no longer decides what to ask. Each plan kind lists the details its work needs:

| Plan kind | Gaps, in order |
|---|---|
| `trip` | `dates`, `who`, `budget`, `stay` (where to stay or the base), `getting_there` |
| `event` | `when`, `who`, `budget`, `place` |
| `purchase` | `budget`, `must_haves`, `by_when`, `where_to_buy` |
| `project` | `done_looks_like`, `deadline`, `budget`, `help` |
| `interest`, `standing` | none |

A gap is settled by any of these:

- a field on the plan (a trip's start date settles `dates`; its place settles `stay` only when the place names a lodging);
- a stored answer with that plan and gap;
- an "enough" from the member (section 4).

**The questions job:**

1. Code computes the open gaps. When there are none, the job ends without a model call.
2. The maker sees the plan, the fact-sheet lines in the gap's categories (not the whole sheet), the stored answers, any web passages the plan already has, and the open gaps, at most 4. For each gap it writes one question with a header, options, descriptions and a recommendation. Options come from the sources or are plain ranges ("Under KSh 20,000"). A question with no gap is ignored.
3. The critic drops a question that the shown facts already answer, or whose options do not fit its gap. When its reply cannot be read, the batch is kept and the code checks still run.

Research does not wait for the answers: it states its assumptions. An answer queues a refresh of class `asked`. Plans of kind `interest` and `standing` are never asked about.

## 4. Chains and when they stop

When a batch is answered, code recomputes the gaps. If any are open, it queues the next round. A chain stops when:

- **no gap is left** (background); in chat, the model stops calling `ask`;
- **every candidate repeats** an earlier question (section 2);
- **the member says enough**: every card has a **Done** button. In the background, the remaining gaps become assumptions; in chat, the tool returns "The member wants you to go ahead with what you have";
- **a soft limit** is reached. After 5 rounds in one chain, the next round is a single question: "Goose has enough to start. Keep answering questions?" (`Start now` / `Ask more`). In chat, the 6th call is refused with the same instruction.

## 5. Chat: the `ask` tool

`giap-questions__ask` takes `{questions: [Question]}` as in section 1, without `gap`. The steps:

1. Code checks the batch against section 2. A refused question comes back to the model with the reason, or with the stored answer.
2. The batch is stored with purpose `chat` and the session id. The stream's `tool_call` frame carries the questions, and the surfaces draw the card at once.
3. The tool waits for `POST /me/questions/batches/{id}/answer`. The answers return as the tool result in plain lines (`Dates: 12-15 Dec`, `Budget: Other: "about 30k"`), and the turn carries on.
4. **Unanswered after 10 minutes:** the tool returns "Not answered yet; the questions wait in My Goose. Say you'll carry on when they answer." The batch is marked `waiting`, so a turn never holds the inference slot past 10 minutes. A later answer is sent into the same session as the member's next message ("My answers: ..."), and the turn carries on from the cached prompt.
5. A message typed while a batch is open counts as "Other" for its first open question. The tool returns it as the answer.

**Voice:** the `ask` tool is not offered in voice turns. If it is called in one, it is refused with "Ask in plain speech."

## 6. Who asks

| Flow | How |
|---|---|
| Chat | `ask`, sections 2 and 5 |
| Plans (Go deeper, a confirmed plan) | The questions job, section 3, queued with the plan's first deliverable |
| Interest confirmation | A one-question batch: header "Interest", options `Follow it` and `Not interested`, each with a description |
| Setup and member interviews | The interview tools ask through `ask`, one topic a batch |
| Idea cards of kind `ask` | The card carries one structured question. Its chosen option becomes the chat message, and "Other" opens the composer |
| Fact-sheet gaps | A batch with purpose `profile`, at most one a day |

## 7. Storage

Migration `0120_questions_structured.sql` rebuilds `questions` (SQLite cannot change a CHECK) with these changes:

- adds purpose `chat`;
- adds state `waiting`;
- adds columns `header`, `gap`, `recommended`, `session_id`, `round`;
- keeps `options_json`, now an array of objects. A plain string reads as `{label}`.

Answers stay in the question store, which is in RAG. A batch expires when its plan stops or ends. A chat batch expires when its session is deleted.

## 8. Routes (under `/api/v1/me`)

| Method and path | Body | Answer |
|---|---|---|
| `GET /questions?session_id=` | | `{batches: [{id, purpose, anchor_id, session_id, round, title, state, questions: [Question with id]}]}`: open and waiting batches, newest first; `session_id` narrows to one chat. A chat surface that sees a `giap-questions__ask` `tool_call` fetches this to learn the batch's ids |
| `POST /questions/batches/{id}/answer` | `{answers: [{question_id, choices: [], text?}]}` | 204. Resumes a waiting chat turn, or queues the next round |
| `POST /questions/batches/{id}/enough` | | 204. Section 4 |
| `POST /questions/{id}/answer`, `POST /questions/{id}/skip` | | Unchanged |

## 9. Surfaces

The same card everywhere, drawn with Jarida Ink:

- one question at a time, with a pager for a batch;
- the header chip, the text, and the options as radio buttons (or checkboxes when `multi`), each with its description;
- the recommended option first, marked "Recommended";
- the preview of the focused option;
- "Other" with a text field, and **Done**.

Where it appears:

- **inline in chat**, on desktop and phone: drawn from the `tool_call` frame, and replaced by the answers once sent;
- **My Goose and Home**, on desktop and phone: open and waiting batches, each with its plan or chat.

Questions are not pushed.
