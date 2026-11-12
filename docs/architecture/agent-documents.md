# Goose's own documents: soul, heartbeat, scratchpad

Three things Goose uses by itself, each with a part a person edits and a part Goose runs or
writes. They came from the pattern OpenClaw and Hermes use (`SOUL.md`, `HEARTBEAT.md`,
`MEMORY.md`); the research is in
[docs/research/2026-11-12-agent-memory-design.md](../research/2026-11-12-agent-memory-design.md).
Before this they were three rows in the prompt-style table that nothing read.

| | A person edits | Goose does |
|---|---|---|
| **Soul** | Who Goose is | Speaks as it in every prompt style |
| **Heartbeat** | The checklist | Goes through it by itself while the house is quiet, and acts within limits |
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
An owner keeps at most eight of each; a ninth drops the oldest. A follow-up nobody came back to
in 30 days is dropped. A member's lessons ride the first turn of each of their conversations in
`<lessons>`, next to their own notes, never the shared prefix. Follow-ups are shown only to the
heartbeat. A member reads both and removes any with `GET /me/notes` and `DELETE /me/notes/{id}`.

## Heartbeat

The heartbeat is the proactive review, made to run by itself with something to go through.

- **When.** On the inference lane like every chore: never during a turn, only after the house
  has been quiet, at most once an hour (`HEARTBEAT_EVERY`), and stopped the moment someone
  speaks. It runs when `proactive_review_enabled` (on by default) and `ext_orchestrator_enabled`
  (off by default, as before: nothing runs agents on a Pond that has not allowed them) are both
  set.
- **What it is given.** The recent events of the house, the person's local time, the checklist,
  and Goose's open follow-ups for that member. It may read memory, devices, sensors, weather and
  the web. It holds no tool that acts.
- **What it may answer.** A JSON array, empty when nothing needs anyone. Each element is a
  suggestion or one of four acts, named by `"do"`.

| `do` | What code does with it | Limits |
|---|---|---|
| absent, `suggest` | A proposal the member is shown and approves, as before | 3 a run, 6 a day |
| `remind` | A reminder on the member's own list, pushed at its time; `when` is read in plain words | 2 a run, 3 a day, within 14 days |
| `note` | A follow-up in Goose's notes | 3 a run with `done` |
| `done` | Closes the one follow-up its words fit | |
| `pause_watch` | Pauses the one web watch of the member's that its words fit, only when that watch has failed three checks in a row, and writes a follow-up saying so | 1 a run |

The model never performs an act. `proactive_review::interpret_heartbeat` checks each one (a
`when` that names no time, one that has passed or is too far off, an act with no words, an act
that is not on this list) and `HeartbeatDeps::carry_out` in `pond-server` does it. Anything that
reaches further than the member's own list or Goose's own notes is a suggestion: devices,
messages, forms and money stay behind the member's tap. A reminder Goose set carries
`source: "goose"` on the to-do, so the member sees whose it is and deletes it like any other.

Starting or resuming a web watch is never an act: a watch needs an address a person gave, and
a watch the member paused stays paused until they say otherwise. The heartbeat proposes it and
the member taps.
