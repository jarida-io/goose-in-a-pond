# Member memory: a profile of each person, and Goose's own rules

What the Pond remembers about each member, how strictly it writes, and the notes Goose keeps for
itself. This is the contract for `giap-memory`, extraction, consolidation and the phone's
"About you" screen.

## Why it changed

Memories were a flat list of sentences in eight segments, one of them general knowledge, written
by three routes with three different bars: extraction (strict), consolidation (model-judged) and
the chat model's own `save_memory` (no bar at all: an empty call saved the person's whole
message, a keyword table guessed its segment, and importance, tier and tags were the model's to
invent). Ideas and news could not ask "what does this person eat" or "are they
seeing someone" without reading every sentence.

## What the research says (2026-11-12)

| Source | What it does | What we take |
|---|---|---|
| Letta / MemGPT | Small always-in-context blocks (`human`, `persona`) with character limits; background agent rewrites them | A capped, always-shown core; background writes |
| Mem0 | Per candidate fact: fetch nearest memories, then ADD, UPDATE, DELETE or NOOP | Supersede in code, never append blindly |
| LangMem | Profile (one document, latest state wins) versus collection (many notes); procedural memory is the prompt | Single-valued slots plus lists; rules live with the prompt |
| Zep / Graphiti | Facts carry validity intervals; a contradiction closes the old fact, nothing is deleted | `valid_until`, archive instead of delete |
| ChatGPT saved memories (reported) | Save what stays true for months and changes later answers; never sensitive categories unless asked | The write policy below |
| Claude memory tool, Hermes | Agent-written notes with hard size caps; over-cap writes fail until compacted | The scratchpad cap |
| OpenClaw | SOUL.md in the prompt, MEMORY.md curated by the agent, HEARTBEAT.md run on a timer, `HEARTBEAT_OK` when nothing needs attention | Soul, scratchpad, heartbeat (see agent-documents.md) |
| AMU, A-Mem, LongMemEval | Typed records behind an admission gate beat free text on 1-4B models; recall degrades past ~3k tokens or ~10 items | Closed vocabulary, at most six recalled items a turn |

No published head-to-head of slot profiles versus free-text lists on 4B on-device models was found;
the slot choice rests on the last row and on E4B filling one named top-level field reliably.

## The profile

Every memory is about one member (or the household) and says what it is about with one word from
a closed list, `about` (`pond_core::user_data::domain::member_profile::About`). The word decides
the heading the fact is filed under (the row's `segment`), whether a new one replaces the old,
how much it weighs and how long it lasts.

| Heading shown | `about` | One at a time | `segment` |
|---|---|---|---|
| Standing instructions | `instruction` | no | `instruction` |
| Who they are | `name`, `birthday`, `work`, `home`, `language` | yes | `identity` |
| People | `partner` | yes | `relationship` |
| People | `child`, `pet`, `family`, `friend` | no | `relationship` |
| Food and health | `diet` | yes | `food_health` |
| Food and health | `allergy`, `health` | no | `food_health` |
| Routines | `routine` | no | `routine` |
| Preferences | `likes`, `dislikes` | no | `preference` |
| Preferences | `style` (how Goose should answer them) | yes | `preference` |
| Goals | `goal` | no | `project` |
| Places | `place` | no | `place` |
| Dates | `date` (a birthday or anniversary that comes round each year) | no | `date` |
| Right now | `now` (true for days or weeks) | no | `context` |

- A new value for a one-at-a-time `about` closes the member's old row (`superseded_by`), in code.
  The model never names what it replaces.
- A `now` row carries `valid_until`, 14 days on, and is not read after it.
- `allergy`, `health` and `partner` are marked sensitive (`About::sensitive`): for the member's own
  turns, ideas and recipes, never for a notification's words or the house speaker.
- `segment` gained `food_health`, `place`, `date` and `instruction`. `knowledge` and `correction`
  remain for rows filed before the profile; nothing new is filed under `knowledge`, and a
  correction is saved as the fact it sets right, under its own `about`.
- Migration `0092` adds `about` and `valid_until`, and files the two segments that can only be
  about one thing (`routine`, `project`). Every other older row waits for the re-sort.

## The write policy

One function, `member_profile::admit`, stands in front of every route by which the Pond itself
writes a memory: the chat model's `save_memory`, batch extraction and the re-sort.

1. Only what a member says about themselves, that will hold for months, and that would change a
   later answer.
2. One fact a row, with its `about`. No `about`, or a word outside the list: not saved.
3. One whole sentence in the third person that names the member. A sentence in the first person,
   one that names nobody it is filed under, a fragment, or a pronoun with nothing before it: not
   saved. The reply says how to write it.
4. Never a question they asked or something they looked up (that is an interest), a task they gave
   the assistant, anything the assistant said, or tool and web content.
5. A thing on one day is a reminder (`add_reminder`), not a memory. Only a yearly date is kept, as
   `date` or `birthday`.
6. Guesses are not written. Extraction keeps what the member said; consolidation may merge and
   close rows, never add a fact nobody stated.
7. The same fact said again is not saved twice: the same sentence, or for a list fact a vector
   within 0.97 of an existing row of that `about`. The bar is high on purpose; a lower one drops
   "likes hiking" as a copy of "likes biking".
8. A member's rows reach only that member's turns, plus the household's shared rows.
9. "Forget that" deletes the row. It takes words from the memory, not an id; when several rows fit,
   nothing is deleted and the model is told to ask which.

What a refused save tells the model is `NotKept::advice`: it starts "Nothing was saved", gives the
reason, and says what to send or to ask instead, because a small model that is not told this says
"saved" in prose.

## What a turn carries

- Once a conversation: the member's standing instructions, `name`, `diet`, `allergy` and `style`
  (`About::always_shown`), at most eight rows, weightiest first.
- Each turn: the rows nearest the message, as before (`memory_relevance::select_for_turn`, inside
  the turn's memory token budget).
- Both are shown as the profile: a heading on its own line, its facts under it. A fact an earlier
  turn of the conversation already shows is not shown again, whatever heading it stood under.
- Never: closed or expired rows.

## Tools (`giap-memory`)

| Tool | Arguments |
|---|---|
| `save_memory` | `content`: one sentence naming the person; `about`: one word of the list |
| `recall_memories` | `query` for the facts nearest it; none for the whole profile, in profile order |
| `forget_memory` | `what`: words from the memory |

Importance, tier, tags, segment and the ids of superseded rows are no longer the model's to
invent; `about` implies them. The keyword classifier that guessed a segment from the sentence
(`auto_classify_segment`) is gone, and an empty `content` no longer saves the person's message.

## Routes

`/api/v1/me/about` is the member's own view and hand: the profile by heading, add, reword,
re-file, forget, and the archived rows with restore. Shapes are in
[my-goose-hub.md](my-goose-hub.md#0a-about-you-the-members-profile). A fact the member writes
there is theirs to word, so its sentence is not judged; it is filed under the `about` they pick.
`/api/v1/memories` keeps its older shape for the desktop's memory screens.

`POST /api/v1/lane/jobs/memory_resort/run` starts the re-sort without waiting for its next look.

## Extraction

The extraction prompt names each memory's `about` from the same list (without `now`: it keeps what
lasts), and a reply in the older `kind` shape is still read. A fact filed by extraction is `long`
tier at most, never `permanent`: it was heard in passing, not told to the Pond. A new value for a
one-at-a-time `about` is not dropped as "related" to the old one; it replaces it.

## Existing memories

A lane job re-sorts every live row that has no `about`, ten at a time while the house is quiet: the
model names each row's `about`, or says it is not a fact about a member; code applies the write
policy. Rows that fail are archived with the reason, never deleted, and can be restored. The job
resumes where it stopped. A row the model gives no usable answer for twice is left as it is: still
recalled, shown under "Other". The re-sort never closes one row in favour of another: on the nano
the model filed three different preferences under `style`, and closing the older two as replaced
hid facts that were still true. Several older rows may therefore share a one-at-a-time `about`
until the member next states that fact, which closes them all. A row consolidation merges comes
out unfiled and is filed on the next pass.

## Goose's own notes

Facts about members never go in Goose's scratchpad, and Goose's rules and lessons never go in a
member's profile. The scratchpad, the soul and the heartbeat are in
[agent-documents.md](agent-documents.md).
