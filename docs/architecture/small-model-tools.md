# Tools for small models, from the ground up

**Status:** design, measured, not implemented. The knowledge tools are the first group redesigned.
**Written:** 2026-10-14. Payload captured on `main` at `e440485b`; the decisive arms re-run
on a capture from `68eef8cb` (section 2).
**Evidence:** [`experiments/knowledge-tools/`](../../experiments/knowledge-tools/). It replays the
prompt the provider shim really sends against five small model families.

**In short.**

- Replace the two knowledge tools with `look_up` and `calculate`. Each takes the user's own
  question, required. The server does what the model was being asked to do: it finds the
  article, picks the passage that answers the question, and does the arithmetic exactly, in
  process, with no key.
- The UI card leaves the model's context.
- Measured against what ships:
  - more answers right from the same result (26 vs 23 of 28 on both Gemma models);
  - about 900 fewer prompt tokens on every lookup turn;
  - facts deep in an article found (0 to 3 of 4);
  - no worse for any model family that calls tools, and much better for some (Llama 3.2 13 to
    27 of 28, Granite 0 to 9).
- Also measured, and not fixable by tool design:
  - E2B with thinking off answers facts and sums from memory whatever it is offered; with
    thinking on it calls for 27 of 28 (D6);
  - E4B calls a calculator for none of 11 sums as shipped. The redesign gets it to 2 (6 on
    current main), and one prompt sentence naming the operations to 6 (9 on main) (D2);
  - under `minimal` (this pond's setting) E4B reaches knowledge for 2 of 28 questions that
    need it (7 on current main); always loading knowledge takes that to 9 (13) (D1).
- Section 7 holds the decisions that are Jerry's.

The household runs Gemma 4 E4B (Mac) and E2B/E4B (Orin) with thinking off, on an 8-16K window,
offered between 2 and 41 tool schemas depending on `tool_selection_mode`. Every tool GIAP ships
was written with that model in mind, but by rules (`docs/developer/mcp-server-standard.md`) that
were never measured against it. This document replaces judgment with measurement, one group at a
time, starting with knowledge: the group every factual question needs.

---

## 1. What the knowledge tools are today

`giap-knowledge` serves two tools (`crates/pond-mcp-server/src/knowledge.rs`, `wolfram.rs`):

| tool | argument | backend | result |
|---|---|---|---|
| `get_wikipedia_article` | `topic` (optional), `limit` (optional) | MediaWiki extract, exact title then search | card JSON + the first 4,000 bytes of the article + source |
| `compute_answer` | `query` (optional) | Wolfram\|Alpha Full Results API, needs an AppID | card JSON + up to 5 pods; **without an AppID, a setup message** |

Found while mapping it, each confirmed against the code or a captured payload:

1. **The model reads the UI card.** Both tools prefix their text with
   `[[[mcp-ui:knowledge:{...}]]]`. `extract_ui_hint` (`pond-api/src/routes.rs`) strips it for
   the SSE stream and its doc comment says "the LLM only sees the clean text". The captured
   post-tool payload shows otherwise: the tool message the model receives is 4,573 chars and
   opens with ~570 chars of card JSON repeating the article's first 300 characters. Goose hands
   the raw tool result to the provider; the SSE layer is a separate copy.
2. **A dead parameter in every turn.** `get_wikipedia_article` advertises `limit`, described as
   "(search_wikipedia only)", a tool removed on 2026-09-10.
3. **Nothing is required.** `required: []` on both, by the standard's own rule ("make everything
   optional"). The server then rescues bad calls by scanning ten alternative keys and, as a last
   resort, stripping question prefixes off the user's last message.
4. **The server's instructions advertise six tools.** `get_info` lists `search_wikipedia`,
   `define_word`, `search_books` and `explore_computation`, none of which exist. None of the
   three captured payloads carries them (the shim replaces Goose's system prompt), so on the chat
   path they reach no model; they are wrong anyway, and anything else that reads them is misled.
   `format.rs` and `pond-api/src/tool_context.rs` also still name `search_wikipedia`.
5. **The ToolCaller path is dead code.** `resolve_topic` and the standard both make a FunctionGemma
   ToolCaller the "primary" parameter source; `main.rs` hard-wires it to `None`, and CLAUDE.md
   rules a separate tool-calling model out of the main loop.
6. **A keyless pond cannot compute.** Without a Wolfram AppID, `compute_answer` returns "requires
   an API key", so arithmetic, the one thing a small model reliably gets wrong in its head, has no
   working tool on a fresh install.
7. **In the narrowed modes knowledge is not on offer.** For "Who was Wangari Maathai?":
   `minimal` (Jerry's own pond) offers only the toolkit and E4B answered from memory without
   loading the group; `relevant` scored the question and loaded **`giap-schedule`**, not
   `giap-knowledge` (`kind="tool_selection"` trace, session `kt-relevant`).

---

## 2. What the bench measured

The bench (`experiments/knowledge-tools/`, README there) replays the payload the provider shim
captured from a scratch pond: GIAP's system prompt, its user-turn envelope, and all 27 tools
offered in `all` mode (or the 2 of `minimal`). Only the knowledge tools are swapped. 42 cases
were labelled before any run: 15 encyclopedic (four with the answer past the shipped 4,000-byte
window), 11 computations, 2 compound, 2 definitions, 6 that need no tool, and 4 that belong to
another group. Each model was served by `llama-server --jinja` with thinking off, as the
household runs, at temperature 0. Fidelity check: in the bench E4B makes the call the real
pond's E4B made for the captured turn, `get_wikipedia_article {"topic": "Wangari Maathai"}`.

### Calling (E1, `all` mode, 27 tools)

28 cases need a knowledge tool.

| | V0 shipped | V1 two tools, `question` | V2 one tool | V0 + prompt line | V1 + prompt line |
|---|---:|---:|---:|---:|---:|
| **E2B** called | 2 | 3 | 1 | 4 | 5 |
| E2B wrong from memory | 9 | 9 | 10 | 10 | 8 |
| **E4B** called, read (of 17) | 15 | 16 | 14 | 15 | 16 |
| **E4B** called, compute (of 11) | 0 | 2 | 0 | 0 | 2 |
| E4B wrong from memory | 3 | 3 | 4 | 3 | 3 |
| false calls (8 cases) / steals (4) | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
| prompt tokens, a no-tool turn | 3,839 | 3,813 | 3,739 | 3,865 | 3,839 |

What the memory answers got wrong, verbatim: Barack Obama Sr. "was born in Honolulu"; M-Pesa in
Afghanistan "was Safaricom" (it was Roshan); 1234 × 17 = 20972 (E2B), 21008 or 21978 (E4B);
2,500 × 52 = 13,000; 3¹³ = 159432324851200; 22,952 days given as 10,711 or 21,777. Every
argument either Gemma model wrote was faithful to the question, diacritics included.

### Discovery (E3, `minimal` mode)

| knowledge reached, of 28 | M0 shipped hatch | M1 sharper group line (+8 tok) | MC knowledge always loaded (+158 tok) | `all` mode, V1, for reference |
|---|---:|---:|---:|---:|
| E2B | 0 | 0 | 1 | 3 |
| E4B (reads, of 17) | 2 | 6 | 9 | 16 |

Both models use the hatch correctly for weather, schedules and devices (`enable_tool_group`
with the right name), so the hatch works. It just isn't used for facts that feel already
known. Under `relevant`, the live selector scored "Who was Wangari Maathai?" and loaded
`giap-schedule` instead of `giap-knowledge`.

### Reading back (E2, the right call forced)

| | A shipped: card JSON + 4 KB head | K shipped, no Wolfram key | B 1 KB, passages chosen by the topic | C redesign: 1 KB chosen by the question | D oracle passage |
|---|---:|---:|---:|---:|---:|
| E2B correct (of 28) | 23 | 2 of 13 | 22 | **26** | 27 |
| E4B correct (of 28) | 23 | 1 of 13 | 22 | **26** | 27 |
| deep-fact cases (of 4), both | 0 | — | 0 | **3** | 3 |
| result bytes, median | 4,523 | 159 | 526 | 536 | 382 |
| prompt tokens, median | 4,910 | 3,936 | 4,041 | **4,015** | 3,963 |

- **The redesign is more correct and ~900 tokens cheaper on every lookup turn**, on both
  models. At the 862 tok/s prefill PAI-3 recorded on the Orin in August, that is about a second
  per lookup. This is an estimate from an older engine build's rate, not a device measurement.
- **B against C is the case for the `question` argument.** The same short format scores 22
  when passages are chosen by the topic and 26 when chosen by the question.
- **A long result lets the prior win.** Arm A's result named Tamirat Tola as the 2024 marathon
  winner; E2B answered "Eliud Kipchoge".
- **A keyless compute tool is worse than none.** After "Wolfram|Alpha requires an API key",
  both models give up ("I cannot compute…") on sums they partly get right unaided.

### Getting a sum calculated (E4: the 11 sums, 8 no-tool cases, 4 other-group cases)

| `calculate` called, of 11 | V1 | V3: renamed `calculator(expression)` | V1 + arithmetic rule | V3 + rule |
|---|---:|---:|---:|---:|
| E2B | 1 | 0 | 2 | 1 |
| E4B | 2 | 2 | **6** | 4 |
| E4B, current main's prompt | 6 | 4 | **9** | 7 |
| Llama 3.2 3B | 11 | 11 | 11 | 11 |
| Granite 4.1 3B | 8 | 5 | 9 | 10 |
| Nemotron 3 Nano 4B | 0 | 0 | 0 | 0 (no tool call of any kind) |

The rule is one sentence in `<tool-usage>`: "You cannot do arithmetic reliably: for any
multiplication, division, percentage, power, unit conversion or count of days, call
`calculate`, however easy it looks." It costs 26 tokens. It added no false calls in any
family (Llama's 5 of 8 is the same with or without it) and took no request from another group.
The name is not the lever: `calculator(expression)` never beat `calculate`, and for Granite
it was worse (5 against 8).

### Other families (E1, arms V0 / V1 / V1P)

| called, of 28 (reads of 17, sums of 11) | V0 shipped | V1 | V1 + prompt line | false calls (of 8) |
|---|---:|---:|---:|---:|
| Llama 3.2 3B | 13: all 11 sums; all 15 encyclopedia questions sent to `compute_answer` | **27**: 16 reads, 11 sums | 27 | 5 in every arm |
| Granite 4.1 3B | 0 | **9**: 1 read, 8 sums | 10 | 0 |
| Nemotron 3 Nano 4B | 0 | 0 | 0 | 0 |
| Nanbeige 4.2 3B | not run: Homebrew llama.cpp 9110 has no `nanbeige` architecture | | | |

- **Each family has its own blind spot, and the redesign is never worse.** E2B calls little,
  E4B reads but will not compute, Granite computes but will not read, and Llama calls
  everything. V1 matched or beat V0 for every family that calls tools at all.
- **Llama is the strongest case for naming the act.** Under V0 it sent all 15 encyclopedia
  questions to `compute_answer`, whose description claims "geography data" and "anything
  computed rather than read". Under V1 it routed 27 of 28 correctly.
- **Granite is the case for `calculate`.** It never touched "Wolfram|Alpha… compute_answer",
  and called `calculate` for 8 of 11 sums.
- **A verbatim `question` costs a weak JSON writer.** Llama wrote two calls that `llama-server`
  could not parse: unescaped quotes copied from the question (`"What does the word "ephemeral"
  mean?"`), and several calls joined with `;`. The engine's call repair (or a tolerant parser)
  is the mitigation; asking for paraphrase would cost every other family.
- **Nemotron tells us nothing about tool design.** With thinking off it called no tool at all,
  weather and memory included. It also told "Remind me to call mum at 6pm" that "the reminder
  is set" without calling anything, which is an invented action, and it gave the
  answer-contract sentence for 15 of 28.

### On current main (`68eef8cb`)

Main moved during the work. #539 removed the `<answer-contract>` example and moved the turn
budget and the date note into the system prompt; the 27 tool schemas are byte-identical. A
fresh capture from a release build of `68eef8cb` (`payloads/68eef8cb/`) was replayed for the
deciding arms on both Gemma models:

| | E2B, first capture | E2B, main | E4B, first capture | E4B, main |
|---|---:|---:|---:|---:|
| E1 called, V0 → V1 (of 28) | 2 → 3 | 4 → 7 | 15 → 18 | 15 → 22 |
| E1 E4B sums calculated, V0 → V1 (of 11) | | | 0 → 2 | 0 → **6** |
| E3 knowledge reached, M0 → MC (of 28) | 0 → 1 | 0 → 4 | 2 → 9 | 7 → 13 |
| E2 correct, A → C (of 28) | 23 → 26 | 23 → 26 | 23 → 26 | 23 → 26 |
| E2 deep facts, A → C (of 4) | 0 → 3 | 0 → 3 | 0 → 3 | 0 → 3 |
| E2 keyless compute (of 13) | 2 | 2 | 1 | 1 |
| parcel-sentence leaks per E1 arm | 1–2 | 0 | 1–2 | 0 |

Every direction holds, and the result-format numbers are identical. Main's prompt makes both
models a little readier to call tools, most visibly E4B's sums: 6 of 11 under `calculate`
without any added rule. #539's removal of the example ended the leak completely.
`relevant` still loads `giap-schedule`, not knowledge, for the biography question
(`payloads/68eef8cb/relevant.json`).

### Thinking on (E5)

E1's `V1` arm again, with reasoning enabled (`--reasoning-budget -1`, `enable_thinking`), on
the read, compute and no-tool cases:

| V1 | called, thinking off (of 28) | called, thinking on | false calls on | median to the first step, on vs off |
|---|---:|---:|---:|---:|
| E2B | 3 | **27** (16 reads, 11 sums) | 0 of 6 | 4.1 s vs 2.0 s |
| E4B | 18 | **28** (17 reads, 11 sums) | 0 of 6 | 5.7 s vs 3.5 s |

This is the largest effect in the bench, and it is not a tool-design effect. With thinking off
these models do not stop to ask whether they know. With it on, both use the knowledge tools
almost every time, calculator included, and still leave chit-chat alone. The timings flatter
thinking. A thinking-off answer without a tool is the whole reply, while a thinking-on call
still has to read its result back. The runs also shared a contended GPU. What thinking costs
a household turn is an Orin measurement (decision D6).

---

## 3. The rules

Each rule states what it is, the evidence, and what enforces it. They apply to every GIAP tool
offered to a small model; the knowledge tools are the first to be rebuilt on them.

A note on the size of the evidence: each arm has 28 cases that need a tool, temperature 0, one
run. A difference of one to three cases is within what rephrasing a single question could
move. Only the large effects (model family, result format, keyless compute, the leak) are
claimed as findings; the small ones are reported as "no worse".

**R1. A tool helps only a model that calls it, so measure the call rate before tuning the
wording.** The largest effects in the bench are not design choices at all. They are which
model it is and whether it thinks. With the same prompt and schemas, E4B called a knowledge
tool for 15 of 17 read questions and E2B for 2; Granite calls for sums and not facts, E4B the
reverse. With thinking off, no description, name or tool count moved E2B past 7 of 28 (E1, E3
`MC`, both captures). Thinking on took it to 27 (E5). A design is judged per model family, on
the bench, never from a description that reads well. *Enforced by:* the bench is the gate for
any tool change (step 7 of the plan).

**R2. Name the act; the backend is an implementation detail.** `look_up` and `calculate`, not
`get_wikipedia_article` and "Wolfram|Alpha". It is the largest effect of any schema change in
the bench, and it is not about Gemma. Llama 3.2 sent every encyclopedia question to
`compute_answer` under the shipped names and routed 27 of 28 correctly under the new ones.
Granite ignored "Wolfram|Alpha" entirely and called `calculate` for 8 of 11 sums. The Gemma
models moved less (E4B 15 to 18, or 22 on current main; E2B 2 to 3), and no family got worse.
It is also 26 tokens cheaper, and it makes the backend a server-side decision. DuckDuckGo to
Wolfram has already happened once, an offline encyclopedia may happen next (decision D5), and
neither should rewrite the prompt prefix. *Enforced by:* a test over every `giap-*` tool's name
and description for vendor names.

**R3. One required argument, carrying the user's own words.** `question`, required. Small
models mostly copy text faithfully. Every Gemma and Granite call had a faithful argument,
diacritics included (`Ngũgĩ wa Thiong'o`). Llama is the exception: it turned `Ngũgĩ` into
`Ngôgó` and cut arguments at an apostrophe (`men's` became `men`). Search does not rescue
either: on the live API, "Ngôgó wa Thiong" finds nothing, and "Who won the men" finds
*X-Men: The Last Stand*. A model with that failure needs its engine's call repair, not a better
schema. What small models do badly is decide what the tool needs. Given the whole question,
the tool can choose WHICH part of an article answers it. The bench's cleanest ablation: the
same short result format scored 22 of 28 when passages were chosen by the topic and 26 when
chosen by the question, and the deep-fact cases went from 0 of 4 to 3 (E2, arms B and C, both
Gemma models). *Enforced by:* schema tests (`required` non-empty for every tool with an input).

**R4. The result is the answer, sized to the answer, in plain text, and nothing else.** Lead
plus two chosen passages, about 1 KB, against 4.5 KB of article head with card JSON. It was
more often correct (26 against 23 of 28 on both Gemma models), cost about 900 fewer prompt
tokens on the turn, and left no buried answer for the model's prior to override: arm A's
result named the 2024 marathon winner and E2B still said "Eliud Kipchoge". UI data rides in
its own `audience: ["user"]` block, never in the model's text. *Enforced by:* a test that the
agent-visible projection of every builtin's result contains no `mcp-ui` marker, and a per-tool
byte budget in `format.rs`.

**R5. Deterministic work is done by the tool, and exactly.** Arithmetic, units and dates are a
parser, not a service: the prototype evaluator answers 13 of the 14 compute-shaped cases with
no network and no key. The shipped design returns a setup message instead, after which E2B and
E4B answered 2 and 1 of 13 sums correctly (E2, arm K). *Enforced by:* `calc.rs` unit tests,
including a held-out phrasing set.

**R6. Nothing in a prompt, a description or a result that the model can copy as an answer.**
On the captured prompt, the `<answer-contract>` example ("The parcel arrives on Thursday…")
came back as the answer to "How many seconds are in 3.5 hours?", with no tool involved at
all. It came back again whenever a tool returned something the model could not use (E1, E2,
both Gemma models). #539 removed that example the same day for its prefill cost; the
lesson generalises to tool descriptions (FunctionGemma copies them into argument values,
recorded in the dual-gemma lab) and to results, where a setup message is the thing copied.
*Enforced by:* `it_states_the_rule_with_no_example_to_copy`, and the bench's leak counter.

**R7. No dead surface.** Every advertised parameter is read; every tool name in a result or an
instruction exists. `limit` named a removed tool; `get_info` named four. *Enforced by:* the
existing `the_tool_inventory_parser_sees_the_tools_that_are_there` pattern extended to
`get_info` strings and to every schema property.

**R8. A prompt rule is a design input, measured like one, and only specific rules move a small
model.** "Answer from knowledge only what cannot have changed" is read literally by a 2B model:
a biography and a multiplication have not changed since training, so both are answered from
memory (E1). A general rewrite ("any number you would have to work out") moved E2B from 2 to 4
of 28 and E4B not at all. A sentence naming the operations and the tool moved E4B's sums from 2
to 6 of 11, or 6 to 9 on current main (E4). Neither added a single false call.

---

## 4. The knowledge tools, redesigned

Two tools, split by the act a user is asking for, each taking the user's own words. Every
change below is one the bench measured or a defect from section 1; nothing is speculative.

### `giap-knowledge__look_up`

```text
description: Read what the encyclopedia says about a person, place, event, organisation or
             idea. Use it for any factual question you are not certain of.
question:    string, REQUIRED. "The user's question, in their own words."
```

The server does the reduction the model used to be asked for:

1. **Search terms are made locally.** Stopwords out of the question; only the remaining terms
   leave the pond, through `traced_get_with` and `record_egress` as today.
2. **MediaWiki search** (`srlimit` 3) picks the article; a disambiguation page falls through to
   the next result.
3. **The same extract call** `fetch_article_summary` makes today.
4. **Passages chosen by the question, not the topic.** The lead (at most 450 chars), then the
   two paragraphs that best match the full question by BM25 over that one article (light
   stemming, IDF within the article so the title's own words weigh little). About 1 KB.
   No model in the tool. Ranking passages with the GGUF embedder is the upgrade once it is
   resident.
5. **The model's copy is plain text it can say aloud:**
   `Title (encyclopedia)` / lead / passage / passage / `Source: url`. No Markdown, no JSON.
6. **The card travels beside the text, not inside it**, as its own content block annotated
   `audience: ["user"]`. Goose's `Message::agent_visible_content` already drops such blocks
   before the provider (`goose-provider-types/src/conversation/message.rs`), so this needs no
   fork patch and follows the MCP spec. `extract_ui_hint` reads the card from that block and
   emits the same `ui` object, so neither UI changes.

### `giap-knowledge__calculate`

```text
description: Work out an exact number: arithmetic, percentages, unit conversions, dates and
             differences between them. Use it for every calculation, however simple it looks.
question:    string, REQUIRED. "The calculation, as the user asked it."
```

1. **In-process first.** An exact evaluator (`calc.rs`; prototype and grammar in
   `experiments/knowledge-tools/kt/calc.py`): symbols or words for the four operations and
   powers, `N percent of M`, square roots and decimal places, a small unit table (length, mass,
   volume, time, temperature), and date differences against the pond's own clock. Rationals where
   it can, so `1234 × 17` is `20978` and not a float. No network, no key, no model.
2. **Wolfram\|Alpha second**, only when an AppID is configured and the local grammar did not
   parse the question (distances between cities, live currency, physical data).
3. **Otherwise a dead end that forbids estimating:** "Could not work this out exactly: <what
   was not understood>." through `format_dead_end`, never the setup message: after it, both
   Gemma models failed 11 and 12 of 13 sums (E2, arm K).
4. **One line back:** `1234 × 17 = 20978`.

### What goes

| removed | why |
|---|---|
| `get_wikipedia_article`, `compute_answer` (names) | backend names; renamed to the act. The direct-dispatch schemas in `dispatcher.rs`, `ToolCallChip` labels and both cards' `toolPattern` move with them |
| `limit` | names a tool removed 2026-09-10 |
| `required: []` | the schema must say what matters; the `extra` catch-all stays, since it is free (`#[schemars(skip)]`) and absorbs `topic`/`query` from a model that remembers the old names |
| the ToolCaller branch of `resolve_topic` | `main.rs` passes `None`; CLAUDE.md rules the split out |
| the `last_user_message` fallback | with `question` required an empty call is a malformed call; answering it from a process-global "last user message" is the cross-session race `session_meta.rs` documents for `current_session_id` |
| the `[[[mcp-ui:…]]]` prefix in model text | the model reads it (section 1) |
| six-tool `get_info` instructions | four of the six do not exist |


---

## 5. What this does to the rest of the pond (checklist §2.2)

| question | answer |
|---|---|
| Moves the **KV prefix**? | Once. Two schemas change, so the first turn after the upgrade re-prefills from the knowledge tools onward. If knowledge becomes core (decision D1) its tools sort into tier 0, which moves everything after the core block once. No per-turn movement afterwards; renaming a backend never touches the prefix again, because names no longer carry the backend. |
| Adds **preamble** tokens? | V1 is 26 tokens smaller than V0 in `all` mode. Making knowledge core adds 158 tokens to `minimal` (measured, E3 `MC`) and about the same to `relevant` (decision D1). A lookup turn's prompt drops by a median 895 tokens (4,910 to 4,015, E2), which is working set, not preamble. |
| `profile_id` can be `None`? | No new path; neither tool reads a profile. |
| New **egress** without `record_egress`? | No new host. `look_up` sends search terms instead of a model-chosen entity; the terms come from the user's question, so the PAI-2 consent picture is unchanged in kind but worth one line in its document: what leaves is "the content words of the question". `calculate` adds no egress at all for the questions it parses locally, and removes Wolfram traffic for them. |
| Puts a **secret** on `Settings`? | No. The Wolfram AppID stays in the secret store. |
| Lets a **Guest** reach personal data? | No. Neither tool reads personal data; `giap-knowledge` is not on the guest denylist today and need not be. |
| Blocks a turn on an **LLM call**? | No. Passage selection is lexical; the evaluator is a parser. |
| Acts **without approval**? | No side effects. |


---

## 6. Plan

Each step is its own commit and leaves the pond working. Gates per CLAUDE.md: fmt, the fast-crate
clippy/test set, `cargo check -p pond-server -p pond-adapters-goose`, `scripts/live-test.sh`
(the SSE `ui` field is a handler change), then the bench re-run against the live tools.

1. **Card out of the model's context.** `prepend_knowledge_hint` and Wolfram's `with_ui_hint`
   return a user-audience block; `extract_ui_hint` reads it (keeping the prefix parser for user
   MCP servers that still use it). Test: the agent-visible projection of a knowledge result
   contains no `mcp-ui`. Independent of everything else; the cheapest win.
2. **`calc.rs`** in `pond-mcp-server`, ported from the prototype, with the bench's compute
   cases plus a HELD-OUT phrasing set as unit tests (the prototype was written with the bench
   cases in view, so its 13/14 is an upper bound).
3. **`calculate` and `look_up`** replace the two tools; passage selection; required
   `question`; dead code out; `get_info` rewritten; `format.rs` and `tool_context.rs` stop
   naming removed tools; dispatcher schemas, `ToolCallChip`, `KnowledgeCard`/`WolframCard`
   patterns and `Extensions.tsx` updated in both UIs.
4. **The `<tool-usage>` sentence** (decision D2), in all three personality templates of
   `prompts.rs`, with the existing prompt tests extended.
5. **Discovery** (decision D1): knowledge core, or a sharper blurb, per E3.
6. **`docs/developer/mcp-server-standard.md`** rewritten to section 3's rules, so the next group
   is built to them rather than to "make everything optional".
7. **Verification on the Mac**, then **the Orin** last: the bench again with the live tools
   instead of fixtures, then `pai-bench` and a household turn on the device, where the result
   size is the number that matters (prefill per result token is a hardware-only measurement).


---

## 7. Decisions that are Jerry's

Each comes with a recommendation; none is made in code until Jerry says so.

**D1. Is knowledge core?** Under `minimal` (this pond's own setting) E4B reached knowledge
for 2 of 28 questions that needed it, against 15 of 28 when the tools are simply present. A
sharper line in the dormant-groups list (`M1`, 8 tokens) made it 6; loading knowledge always
(`MC`, 158 tokens) made it 9. On current main's prompt the same two arms read 7 and 13. Under
`relevant`, the selector loaded `giap-schedule` for a biography question, on both captures.
*Recommendation:* make `giap-knowledge` core. Every factual question needs it, and no opening
message predicts one better than any other, which is the same reason memory is core
(`CORE_RATIONALE`). Its 158 tokens in `minimal` buy the largest gain measured in that mode.
Take the sharper blurb as well, since it costs nothing and helps any mode that lists the
group.

**D2. The `<tool-usage>` sentence.** A general rewrite ("never answer from memory a specific
fact… or any number you would have to work out") moved E2B by two cases and E4B by none (arms
`V0P`, `V1P`). A rule that names the operations and the tool did move E4B: "You cannot do
arithmetic reliably: for any multiplication, division, percentage, power, unit conversion or
count of days, call `calculate`, however easy it looks." With it, E4B called `calculate` for 6
of 11 sums against 2 without it, and on current main's prompt 9 against 6. There were no false
calls on the 8 no-tool cases and no steals, in any family (E4, `V1P2`). Renaming the tool
`calculator(expression)` never helped (`V3`: 2 and 4 of 11).
*Recommendation:* adopt the arithmetic sentence with `calculate`, 26 tokens (measured), in all
three personality templates. E4 did not run the read cases under it. The plan's bench re-run checks
that reads are unharmed before it ships.

**D3. Wolfram|Alpha behind `calculate`.** *Recommendation:* keep it as the second backend, used
only when an AppID is configured and the local grammar did not parse. Absent a key it costs
nothing; present, it answers what a parser cannot (city distances, live currency).

**D4. The answer-contract example. Done upstream.** It was copied as an answer when a small
model had nothing better: the parcel sentence came back for "How many seconds are in 3.5
hours?" with no tool involved, and again whenever a tool returned a setup message. #539
(2026-10-14) removed the example for its prefill-chunk cost and for exactly this copying;
`it_states_the_rule_with_no_example_to_copy` now holds it out. The bench measured the leak on
the prompt from before that change. Nothing further to decide.

**D5. An offline encyclopedia.** The system prompt tells the model the pond has "no data ever
leaving it", and `look_up` sends search terms to Wikipedia. A local Kiwix/ZIM index behind
`look_up` would make that true for knowledge and work without internet. Because R2 keeps the
backend out of the tool's name and description, it would be a server-side change with no
prompt change. *Recommendation:* not in this redesign; decide after it lands, with the
download size and the PAI-2 consent question on the table.

**D6. Thinking is the lever the tool surface is not.** With thinking off, E2B made at most 7
of 28 knowledge calls in any arm on either capture, and answered the rest from memory, about a
third of them wrongly. With thinking on, E2B called 27 of 28 and E4B 28 of 28, with no false
calls (E5). The household runs `thinking_mode = off` because thinking is dead air before the
first token, recorded in the live-TTFT notes. So the trade is real, and it is Jerry's:

- **Thinking on** where answers matter more than first-token time. It called on almost
  every case here, and it is slower by a factor the Orin has to measure.
- **A bounded reasoning budget.** If the decision to call comes early in the reasoning, a small
  budget would buy most of the effect for little of the cost. That is a guess. The next bench
  arm (`--reasoning-budget 64` and `128`) tests it on the Mac, and the Orin prices it.
- **Retrieval the pond does itself**, the way `<memories>` already is: an index, an embedder
  and a per-turn token cost, with no model decision involved.
- **Accepting it**, and saying in the model recommendation text that with thinking off, E2B's
  facts come from its weights.

*Recommendation:* run the bounded-budget arm next, then price the winner on the Orin before
choosing. Thinking-off E2B should not be presented as a model that looks things up.

**D7. Rename without aliases.** Nothing outside the pond calls the knowledge tools by name:
the desktop matches them for labels and cards, and the dispatcher only describes their
arguments. *Recommendation:* rename in one step, with both UIs updated in the same commit.

---

## 8. Web search (`search_web`), the first tool built on these rules

**Status:** built 2026-10-15 on `feat/web-search`, not merged, not run on the Orin, not run
against Brave with a real key. Jerry chose the backend: **Brave Search API, with the
household's own key**, settable from the desktop and from Goose on the Go (its PR:
jarida-io/goose-on-the-go#130). §8.8 records what was built where the contract below was
amended, and what is still open. `search_web`
searches, then fetches the top result pages, extracts their readable text, and feeds the
model the passages that answer the question.

### 8.1 Why Brave, and why not DuckDuckGo

Primary sources, read 2026-10-15:

| backend | why not, or why |
|---|---|
| **DuckDuckGo** | No web search API exists. The Instant Answer API returns abstracts and definitions: asked "what time does junction mall close" it returned nothing at all. Its own API page (an archived copy, found through search) says it "is not a full search results API" because DuckDuckGo "does not have the rights to fully syndicate" its results. The only route to real results is scraping its HTML pages, outside its authorised use and brittle against its rate limiting. Possible later as an opt-in backend, by Jerry's decision only. |
| Google Custom Search JSON API | Closed to new customers; shuts down 2027-01-01. |
| Tavily | §9.2 takes a "perpetual, and irrevocable license" to store and use every query. |
| Mojeek | AI use explicitly allowed, £2 per 1,000, no free tier; a later backend behind the same port. |
| SearXNG run by the pond | No key, but scrapes Google/Bing/DuckDuckGo against their terms, and costs RAM the Orin does not have. |
| **Brave** | $5 per 1,000 searches, with $5 of free credit a month (about 1,000 searches). A card confirms identity. The household is the customer, which is what Brave's terms permit (§4(a): a key is not shared outside the customer's own contractors; §3(b)(iii): the API is not made available to third parties). |

**Brave terms that bind the implementation:**

- §3(b)(i): no storing or caching Search Results beyond transient use. Nothing is cached to
  disk.
- §3(b)(xiii): Search Results may not be used to evaluate or benchmark models. The bench uses
  hand-written fixtures for web cases, never Brave output.
- §3(b)(xii): no redistribution of results.
- §4(d): attribution is optional, and if shown it must read "POWERED BY BRAVE" with the logo.

**One conflict is Jerry's to settle.** Chat history persists tool results. Where pages were
fetched, the model's copy carries the pages' own text (third-party content, not Brave's), plus
titles and domains. Brave descriptions appear only when a page could not be read.

### 8.2 The tool

```text
name:        giap-knowledge__search_web
description: Search the web for anything current, local or specific that the encyclopedia
             will not have: news, opening hours, prices, products, businesses, recent results.
question:    string, REQUIRED. "The user's question, in their own words."
```

The `extra` catch-all absorbs `query`/`q`/`topic`. The tool is **offered only while a key is
stored**. `giap-knowledge` always lists it, so no Goose or GIAP tool cache ever goes stale.
The per-turn allow-set drops `giap-knowledge__search_web` when `BRAVE_SEARCH_API_KEY` is
absent. The requirement table is policy in `pond-core` (`tool_group.rs`); the secret check is
mechanism in `pond-mcp-server`. A key pasted on the phone puts the tool in the next turn's
prompt with no restart. The handler re-checks the key at call time, which covers the race.

### 8.3 Pipeline

1. **Search.** `GET https://api.search.brave.com/res/v1/web/search`:
   - parameters `q=<question>`, `count=8`, `extra_snippets=true`, `safesearch=moderate`,
     `text_decorations=false`;
   - headers `X-Subscription-Token: <key>`, `Accept: application/json`;
   - 6 s timeout;
   - `country` is not sent: Kenya is not a documented value.
2. **Fetch.** The top 3 `http(s)` results are fetched concurrently:
   - 5 s per page and a 7 s deadline overall;
   - 1.5 MB cap, `text/html` only, at most 3 redirects;
   - every URL and redirect hop goes through `check_egress`, so `allowlist` and `offline` modes
     are honoured, and a refused page falls back to its snippets silently.
3. **Guard.** Page fetches use a client whose DNS resolver refuses every non-public address:
   - loopback, private, link-local, CGNAT, unique-local, multicast, unspecified;
   - IPv4-mapped IPv6 forms of all of those;
   - IP-literal URLs in those ranges as well.

   A search result must never be able to make the pond read its own API, the Matter server or
   the household's router. Brave's own host uses the ordinary client.
4. **Extract.** HTML is parsed with `scraper`, a new workspace dependency:
   - drop `script`, `style`, `noscript`, `template`, `svg`, `iframe`, `nav`, `header`,
     `footer`, `aside`, `form` and `[aria-hidden=true]`;
   - keep text blocks from `p`, `li`, `h1`–`h4`, `td`, `th`, `blockquote`, `pre`, `dd`;
   - collapse whitespace, and drop non-heading blocks under 40 chars.
5. **Rank** (amended 2026-10-15, after E7). The candidates are page blocks plus Brave's
   `description` and `extra_snippets`. They are scored with BM25 against the full question,
   with two changes the bench showed are needed:
   - **Suffix stemming before the five-character cut.** Strip one of `ations ation ings ing
     edly ers ies ied ed es ly er s`, then cut to five, so "opening" meets "open".
   - **An answer-type boost.** When the question asks a time, price, date or count ("what
     time", "close", "how much", "cost", "when", "how many"…), passages carrying that shape (a
     clock time, a currency amount, a year or date, a number) get the top score added.

   This is retrieval inside a tool the model already chose to call, not routing. Plain BM25 put
   the answer in the top two passages for 4 of 6 web cases; the amended ranker did for 6, with
   Wikipedia unchanged at 11 of 17. The production MiniLM embedder managed 5, so the tool needs
   no model (`kt/rankers.py`).
   - Take the best passages up to about 1,800 bytes, at most 2 per source.
   - A source whose page was read contributes page text, not Brave's description.
   - The ranker is pure, so it lives in `pond-core`, where `look_up` will reuse it.
6. **Model's copy, plain text, no URLs, no Markdown:**

   ```text
   Web results for: <question>
   <Title> (<domain>[, <age>])
   <passage>
   <passage>
   <Title> (<domain>)
   <passage>
   ```

7. **Card.** The card goes in a separate content block annotated `audience: ["user"]` (Goose
   drops those before the provider). It carries the `[[[mcp-ui:search:{...}]]]` marker with
   `{query, results: [{title, url, domain, snippet}]}`. The SSE layer finds the marker in any
   block, not only the first.
8. **Misses** go through `format.rs`, naming real tools only:

   | condition | what the model is told |
   |---|---|
   | no results | `format_no_results`, offering `look_up` |
   | 401/403 | "the web search key was rejected", and the settings status is set to `rejected` |
   | 402/429 | "this month's web search allowance is used up" (`quota`) |
   | network failure | `format_api_error` |
   | key removed mid-turn | `format_dead_end` |

9. **Egress.** `api.search.brave.com` joins `KNOWN_PUBLIC_SUFFIXES`. Page hosts classify as
   they do today. Each request is recorded through `traced_get_with`, attributed to
   `search_web`.

### 8.4 HTTP contract (both apps use only this)

All three routes require a token, like `/secrets`, and none returns the key.

| route | does | answers |
|---|---|---|
| `GET /api/v1/web-search` | report state | `{"provider": "brave", "configured": bool, "status": "ok" \| "not_configured" \| "rejected" \| "quota" \| "unverified"}` |
| `PUT /api/v1/web-search/key` `{"key": "..."}` | validate with ONE Brave request (`q=test`, `count=1`), then store as `BRAVE_SEARCH_API_KEY` | 200 `{"configured": true, "status": "ok"}`; 422 `{"error": "Brave did not accept this key."}` on 401/403, nothing stored; 200 `"quota"` when the key is valid but spent; 200 `"unverified"` when Brave could not be reached (stored, checked on first use) |
| `DELETE /api/v1/web-search/key` | remove the key | 204 |

### 8.5 What each app shows

The same content in every surface. On the desktop it is one shared `connections/WebSearchPanel`,
mounted wherever `ConnectionsPanel` already is: classic `sections/Context.tsx` and hub
`hub/views/settings/Connections.tsx`, so there is one form. On Goose on the Go it is a new
screen under Profile → Integrations.

- **A status line:**

  | status | shows |
  |---|---|
  | `ok` | "On, searching with Brave" |
  | `not_configured` | "Off" |
  | `rejected` | "Brave did not accept the key" |
  | `quota` | "This month's free searches are used up" |
  | `unverified` | "Saved, not yet checked" |

- **A masked, write-only key field** with Save, and Remove when a key is stored. The key is
  never read back.
- **Help text:** "Get a free key at api-dashboard.search.brave.com. The free monthly credit
  covers about 1,000 searches. Brave asks for a card to confirm who you are." The URL is a
  link.
- **The privacy line, always visible:** "When the pond searches the web, your question is
  sent to Brave, and the top pages are read from their sites."

### 8.6 Tests and gates

- Unit tests:
  - the resolver guard (every refused range, mapped forms, an IP literal, a redirect to
    loopback);
  - the extractor, on hand-written HTML: boilerplate dropped, blocks kept;
  - the ranker;
  - the result formatter (no URL, no Markdown, budget held);
  - each Brave status mapping, against a stub server;
  - the per-turn gate: no key, no tool; a key, the tool; removing it drops it again;
  - the agent-visible projection of a result: no `mcp-ui`.
- The three routes against a stub Brave.
- `scripts/live-test.sh`, since the change adds routes.
- An `#[ignore]` live test against Brave, run by hand with a real key.
- Bench E6 and E7 (§8.7) are re-run against the live tool before it ships.

### 8.7 What the bench measured for web search

Both experiments ran on current main's captured prompt (`68eef8cb`), thinking off.

**E6, calling.** 12 questions only the live web can answer (opening hours, prices, news, train
times), asked beside the 40 earlier cases. The arm is `S1`: `look_up`, `calculate` and
`search_web`.

| `S1` | web questions to `search_web` | encyclopedia it took | sums it took | false calls |
|---|---:|---:|---:|---:|
| E2B | **12/12** | 4/17 | 0/11 | 0/8 |
| E4B | **12/12** | 3/17 | 0/11 | 0/8 |
| Llama 3.2 3B | 7/12 | 0/17 | 0/11 | 4/8 |
| Granite 4.1 3B | 2/12 | 0/17 | 0/11 | 0/8 |

E2B is the model that almost never looks anything up, and it calls `search_web` every time
the question is plainly current ("today", "right now", "this month"): those read as live, and
live is what the system prompt says to call tools for. The other families are weaker. Granite
sent seven web questions to `get_current_weather`; Llama sent two there and three to `look_up`.

**E7, reading back.** Six SYNTHETIC cases (`fixtures/web/synthetic.json`, hand-written,
because Brave's §3(b)(xiii) forbids benchmarking on its results), with the `search_web` call
forced. Each snippet looks like a real hit but lacks the answer; the page text has it.

| correct / honest "the result does not say" (of 6) | snippets only (the old SearXNG format) | pages, plain BM25 | pages unreadable | pages, amended ranker |
|---|---:|---:|---:|---:|
| E2B | 0 / 6 | 4 / 2 | 0 / 6 | **6 / 0** |
| E4B | 0 / 6 | 4 / 2 | 0 / 6 | **6 / 0** |

**E8, finding it under `minimal`.** The household's own mode lists dormant groups by the text
before the first colon of each description. For giap-knowledge that is "General reference
knowledge and factual lookup", which says nothing about the web. The description now begins
"Looking things up, working things out and searching the web".

| `minimal`, knowledge loaded | web, old line | web, new line | encyclopedia, old | encyclopedia, new | false calls / stolen |
|---|---:|---:|---:|---:|---:|
| E2B | 7/12 | **12/12** | 0/17 | 0/17 | 0 / 0 |
| E4B | 10/12 | **12/12** | 7/17 | **13/17** | 0 / 0 |

**The same description under `relevant`** (`kt/selection.py`). This replicates
`select_groups` with the production MiniLM: core groups, any group scoring at least 0.28, and
the top non-core scorer as a rescue. It reproduces the live capture's choice of `giap-schedule`
for "Who was Wangari Maathai?". The reason shows in the scores: every group scores between
−0.1 and 0.15 against these questions, so the rescue decides. The new description takes
encyclopedia questions from 5/17 to 10/17 and web questions from 2/12 to 5/12. Better, still
unreliable, and more evidence for decision D1.

Reading the pages is the whole difference between a web search that answers and one that does
not. Neither model invented an answer in any arm: given results without the answer, both said
so. The caveat is the size of the evidence. Six synthetic cases, written by the same hand
that wrote the boost patterns, make 6/6 an upper bound. The live-tool re-run in §8.6 is the
real test.

### 8.8 As built, and what is still open

**Where the build differs from §8.3:**

- **Ranking.** The ranker fuses plain and boosted BM25 by reciprocal rank (k = 60). When two or
  fewer pages were read, it quotes up to four passages a page within 2,400 bytes; otherwise
  two a page within 1,800.
- **Extraction.** It ports Mozilla Readability's unlikely-candidate rule, with its retry
  without the class test when a page yields under 500 characters, and adds:
  - a "most popular / trending" rail pattern;
  - drops for `cite`, `sup`, reference lists and navboxes, and the
    navigation/contentinfo/complementary/endnote roles;
  - table rows kept whole, so infobox rows survive;
  - headings prefixed onto the blocks under them;
  - long blocks split into runs of whole sentences.
- **Stemmer.** It also drops a trailing `e` ("close" and "closing" meet), and "how long",
  "distance" and "length" count as number questions.

**Measured on ten real pages** (frozen in `fixtures/web/real/`, chosen before tuning; the answer
is in every page's extracted text):

- The answer reaches the model's copy on **6 of 10**, against 5 before the last iteration.
- No citation, menu, rail or footer text reaches the copy.
- The misses:
  - two infobox rows ("Line length 472.3 km", an elevation) that share no word with the
    question;
  - two answer sentences, on GOV.UK and the BBC, that share almost none.
- More misses sat in the extractor, on pages dropped while choosing the set: a 26-character
  opening-hours paragraph under the 40-character rule, text in plain `div`s, and a Bootstrap
  `media-block` that Readability's "media" pattern drops.
- The synthetic six stay at 6 of 6, and the Wikipedia seventeen go from 10 to 13.

**Found on the way, now fixed:**

- The voice child (`chat --voice --json-events`) never installed a secret store, so every
  keyed tool was keyless in voice. `run_chat` now installs a read-only one, which also turns
  Wolfram on in voice for a pond with an AppID.
- `enable_tool_group` could have widened a withheld tool back in. It cannot now.

**Open, each recorded rather than engineered around:**

| item | what | whose |
|---|---|---|
| History keeps results | Brave §3(b)(i) allows only transient storage of Search Results; chat history persists the tool message. Where pages were read, that message carries the pages' own text, not Brave's. | Jerry |
| Guests can spend the allowance | `giap-knowledge` is not on the guest denylist, so a guest's question can spend the household's Brave credit. | Jerry |
| 429 means two things | Brave answers 429 for a per-second burst as well as a spent quota; both show "used up". | follow-up |
| The environment wins | The secret store reads the environment first, so an exported `BRAVE_SEARCH_API_KEY` turns the tool on, and `DELETE` cannot turn it off. | follow-up |
| The direct dispatcher | On the mistral.rs backend (off by default) and the quarantined pond-agent, `convert_rmcp_result` joins every text block, so the card reaches the model and there is no per-turn key gate. | follow-up |
| Status is per process | `rejected`/`quota` learned by the voice child never reach the serve process's `GET`; a restart reports a stored key `ok`. | follow-up |
| Live Brave | The `#[ignore]` test `live_brave_search_answers_a_local_question` needs a household key; no run has spent a real search. | Jerry, with a key |

### 8.9 Every tool that needs a key is off while its key is missing

Jerry, 2026-10-15: "disable tools that need API keys and the API keys are not available".

| tool | key | how it is turned off |
|---|---|---|
| `giap-knowledge__search_web` | `BRAVE_SEARCH_API_KEY` | per turn: withheld from the allow-set (§8.2) |
| `giap-knowledge__compute_answer` | `WOLFRAM_APP_ID` | the same per-turn gate. Keyless, it only returned a setup message, and with it on offer both Gemma models got fewer sums right than with no tool at all (E2, arm K: 2 and 1 of 13) |
| catalogue extensions: github, brave-search, slack | the key their catalogue entry marks `required` | at start and stop: never started without the key, and stopped when it is deleted |

**Why extensions are gated differently.** An extension the pond never starts offers no tools, so
no per-turn bookkeeping is needed for it. The rule for "cannot run without" is
`SecretRequirement::blocks_install`, which install and the lifecycle share, so the two can never
disagree. A sign-in (`oauth_flow`) never blocks; music declares nothing required.

**What was wrong underneath, now fixed.** Installing a catalogue extension saved its server row
with the key values in `env`: a plaintext copy in `pond_system.db`, beside the encrypted store.
Start-up began from that copy, so deleting a key never turned the extension off.

- Rows are now saved without key values.
- Start-up scrubs existing rows once, injects keys from the store only, and skips an extension
  whose key is missing, logging its name and the key's name.
- `DELETE /api/v1/secrets/{key}` stops every running extension that needs that key, and keeps
  it installed; saving the key again restarts it.

**Not gated.** `book_ride` needs a linked Uber account, not a key. No UI can enter a Wolfram
AppID today: the desktop's API-keys screen went with the settings catalogue on 2026-08-17. So
`compute_answer` stays off on every pond until a key arrives by `PUT /api/v1/secrets/WOLFRAM_APP_ID`.

**Verified.** Tests for the table, the rule, the gate and the start-up env preparation. In
`pond-api/tests/keyed_extensions.rs`:

- install refuses without the key and starts nothing;
- install with the key saves no key value;
- deleting the key stops the extension and keeps it installed;
- deleting an unrelated key stops nothing;
- a restart without the key is refused by name, and saving the key restarts it.

`scripts/live-test.sh` passed: 145 checks, then 23 after a restart, then the auth phase.
