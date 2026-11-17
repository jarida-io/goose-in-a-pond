# Goose's own documents: soul, heartbeat, scratchpad

Three things Goose uses by itself, each with a part a person edits and a part Goose runs or
writes. They came from the pattern OpenClaw and Hermes use (`SOUL.md`, `HEARTBEAT.md`,
`MEMORY.md`); the research is in
[docs/research/2026-11-12-agent-memory-design.md](../research/2026-11-12-agent-memory-design.md).
Before this they were three rows in the prompt-style table that nothing read.

| | A person edits | Goose does |
|---|---|---|
| **Soul** | Who Goose is | Speaks as it in every prompt style |
| **Heartbeat** | The checklist | Goes through it by itself while the house is quiet, and leaves itself jobs that code carries out |
| **Scratchpad** | The rules for memory | Follows them; writes its own lessons and follow-ups |

The three editable texts are rows of `prompt_templates` named `soul`, `heartbeat` and
`scratchpad`, so the existing routes read, save and reset them (`GET/PUT /prompts/{name}`,
`POST /prompts/{name}/reset`). They are not prompt styles: `prompts::is_prompt_style` is false for
them and `Settings.prompt_style` never names one. Each is cut to 1,500 characters where it enters
a prompt. Migration `0093` renames the scratchpad's row from `memory`, which is what members'
memories are called.

## Soul

`prompts::with_agent_documents` replaces the `<identity>` block of whichever style is in use with
the soul's, before the template is rendered, so `{{assistant_name}}` and the other placeholders
work in it. The styles differ in manner, never in who is speaking. It is part of the system
prefix: editing it costs one re-prefill, and nothing else changes it. Only a person edits it;
Goose has no tool that writes it.

## Scratchpad

Two parts, kept apart because one is a person's and one is Goose's.

**The rules** are the scratchpad row. They replace the style's `<memory-rules>` block, in the
system prefix: use the profile, what `save_memory` keeps and never keeps, that a fact is said to
be remembered only after the tool said so, and where Goose's own notes go. The policy they
describe is enforced in code as well ([member-memory.md](member-memory.md)).

**Lessons and follow-ups** are rows of `agent_notes` (migration `0093`), which Goose writes with
`note_to_self {text, kind}` in `giap-memory`:

- a `lesson` is how to work better, learnt from a mistake or a correction;
- a `note` is something to come back to.

They are never facts about people. Each belongs to the member whose conversation it came from
(or to the household for a household turn) and reaches only that member's turns, like a memory.
An owner keeps at most eight of each; a ninth drops the oldest. A member's lessons ride the
first turn of each of their conversations in `<lessons>`, next to their own notes, never the
shared prefix. A follow-up is shown to one heartbeat, which turns it into a job or lets it go,
and is deleted once that heartbeat has answered; one no heartbeat reached in 30 days is dropped.
A member reads both and removes any with `GET /me/notes` and `DELETE /me/notes/{id}`.

## Heartbeat

The heartbeat is the proactive review, made to run by itself with something to go through.

- **When.** On the inference lane like every chore: never during a turn, only after the house
  has been quiet, at most once an hour (`HEARTBEAT_EVERY`), and stopped the moment someone
  speaks. It runs when `proactive_review_enabled` (on by default) and `ext_orchestrator_enabled`
  (off by default, as before: nothing runs agents on a Pond that has not allowed them) are both
  set.
- **What it is given.** The recent events of the house, the person's local time, the checklist,
  their plans (active anchors), the jobs it left itself that are still waiting, the follow-ups
  their conversations left it, their web watches that keep failing, and the conditions a job
  may wait for: a time, their next turn, their saved places, and this pond's cameras and
  switched devices by label. It may read memory, devices, sensors, weather and the web. It
  holds no tool that acts.
- **What it may answer.** A JSON array, empty when nothing needs anyone. Each element is a job
  it leaves itself or a suggestion, named by `"do"` (`HEARTBEAT_RECIPE` in
  `user_data/services/intentions.rs`).

| `do` | What code does with it | Limits |
|---|---|---|
| `intend` | An intention: one act, done once its condition holds, until a deadline | 3 a run, 20 waiting |
| absent, `suggest` | A proposal the member is shown and approves | 3 a run, 6 a day |

The heartbeat sets no reminder, writes no to-do and keeps no notes. The whole design is in
[idea-engine.md](idea-engine.md) section 8; in short:

- **Kept.** `proactive_review::interpret_heartbeat` reads the suggestions and
  `intentions::parse_intents` the jobs. `intention_runner::close_review` in `pond-server` checks
  each job in code (`intentions::validate`: a condition that can be checked, a deadline within
  30 days, nothing a waiting job, an open to-do or a schedule already says), puts the survivors
  to one critic call, stores what is left, and logs the run as an engine job of kind
  `heartbeat` whose outcome says how many were kept and why each of the others was not. A
  critic that cannot be read, or that a turn cut short, keeps none.
- **Carried out.** `intention_runner` checks conditions with no model call: times on a
  30-second tick, cameras, sensors and devices from the event bus, a place crossing from
  `POST /me/places/{id}/event`, and a member's next turn from `ChatService` as it saves the
  turn (in this process: a turn taken by a separate `chat --voice` process is not heard). A met
  intention is carried out and marked `done`; one past its deadline is marked `expired`. Both
  are written to the member's Goose activity.

| `act` | What happens when the condition holds |
|---|---|
| `push` | An `alert` notice with the words, and the same line in the member's Goose thread |
| `refresh` | The plan's shown deliverables are queued to be made again, class `deadline` |
| `look_into` | The errand role looks it up with reading tools; the answer goes to the Goose thread and a notice. Four a day |
| `ask` | One notice opening the plan's open questions |
| `propose_todo` | An offer the member answers with Add or No (`/me/todos/offers`); never a to-do |

Anything that changes a device, spends, sends or schedules is a suggestion: it stays behind the
member's tap. Starting, pausing or resuming a web watch is never done unasked: the brief lists
the watches that keep failing, and the heartbeat tells the member or proposes the pause.
