# Knowledge tools for small models

A standalone bench. It replays GIAP's **real** prompt: the system prompt, user-turn envelope and
27-tool array that the provider shim actually sent, captured with `GIAP_CAPTURE_PAYLOAD` from an
isolated scratch pond on 2026-10-14. It runs the prompt against `llama-server`, swaps **only**
the knowledge tools, and scores what a small model does with them. No Goose, no crates. Stdlib
Python plus one `llama-server` at a time.

The design it informs is
[`docs/architecture/small-model-tools.md`](../../docs/architecture/small-model-tools.md).

## The questions

1. **E1, calling.** Given a question that needs a lookup or a computation, does the model
   call a knowledge tool, with a faithful argument? Does it call one when it should not, or
   take a request that belongs to another group?
2. **E2, reading back.** Given the right call (forced), does the model answer correctly from
   the result, briefly and in its own words, at what prefill cost?
3. **E3, discovery.** Under `tool_selection_mode = "minimal"`, does the model load
   `giap-knowledge` through the hatch, and does always loading it fix what the hatch does not?
4. **E4, getting a sum calculated.** On the compute, no-tool and other-group cases only: does a
   tool named like the calculators in function-calling data (`calculator(expression)`), or a
   prompt rule that names the operations, get the call that `calculate` alone does not?
5. **E5, thinking on.** E1's `V1` arm with reasoning enabled, for the two Gemma models: is
   thinking the lever that the tool surface is not?
6. **E6, web search beside knowledge.** 12 questions only the live web answers
   (`cases-web.json`), asked beside the earlier cases. Is `search_web` called when it is
   needed, and does it take questions `look_up` or `calculate` should answer?
7. **E7, web read-back.** On SYNTHETIC fixtures (`fixtures/web/synthetic.json`; Brave's terms
   forbid benchmarking on its results): snippets only, against passages extracted from the
   pages, against pages that could not be read. `kt/rankers.py` compares the passage rankers
   on retrieval alone, the production MiniLM included (run it with the pond's venv Python).

## Arms

| arm | what changes | against |
|---|---|---|
| `V0` | nothing: the shipped `get_wikipedia_article` + `compute_answer` | — |
| `V1` | two tools split by the act (`look_up`, `calculate`), one **required** `question` argument in the user's words, when-descriptions that name no backend, no dead `limit` | V0 |
| `V2` | one tool (`look_up`) for both reading and working out | V1 |
| `V0P`, `V1P` | as V0 / V1, plus ONE sentence changed in `<tool-usage>`: specific facts and worked-out numbers are never answered from memory | V0, V1 |
| `M0` | minimal mode as shipped: toolkit only, groups listed by blurb | — |
| `M1` | minimal, with a sharper `giap-knowledge` blurb | M0 |
| `MC` | minimal plus the V1 knowledge tools always loaded (knowledge as core) | M0 |
| `A_V0R0` | E2: shipped schema, shipped result (card JSON + first 4,000 bytes of the article) | — |
| `K_V0R0k` | E2: shipped, no Wolfram AppID (compute returns the setup message) | A |
| `B_V0R1` | E2: shipped schema, new result format (lead + 2 BM25 passages chosen by the QUESTION, plain text, ~1 KB) | A |
| `C_V1R1` | E2: the redesign, schema and result | A |
| `D_V1R1o` | E2: the redesign with an ORACLE passage (the sentence holding the answer), the retrieval ceiling | C |
| `V1P2` | E4: V1 plus a `<tool-usage>` sentence naming the arithmetic operations and `calculate` | V1 |
| `V3`, `V3P2` | E4: `calculate` renamed `calculator(expression)`, without and with that sentence | V1, V1P2 |
| `V1T` | E5: V1 with thinking on (`--reasoning-budget -1`, `enable_thinking`) | V1 |

## Cases

`cases.json`: 42 cases with labels fixed **before** any run.

| class | n | what it checks |
|---|---:|---|
| `r` read | 15 | encyclopedic facts, Kenyan proper nouns (Ngũgĩ, Kimathi, Lamu); `r12`–`r15` have answers beyond the shipped 4,000-byte window |
| `c` compute | 11 | arithmetic a small model gets wrong in its head, percentages, units, a date difference |
| `x` compound | 2 | one question needing both a lookup and a computation |
| `d` define | 2 | definitions; no tool exists for them since 2026-09-10, so any route counts |
| `n` none | 6 | chit-chat, rewriting, `2 + 2`, today's date (in the system context) |
| `o` other | 4 | weather, memory, schedule, device control. A knowledge call here is a **steal** |

Tool results in E2 are frozen fixtures. `fetch_fixtures.py` makes the same MediaWiki call
`fetch_article_summary` makes, and compute results are exact values, so runs compare designs and
not the network.

## Running it

```bash
python3 -I fetch_fixtures.py        # once; fixtures are committed
./run.sh                            # every model, every experiment (hours on a contended M4)
./run.sh e1 gemma-4-E2B-it-qat-UD-Q4_K_XL
ONLY=r01,c01 ./run.sh e2            # a subset
python3 -I report.py                # the tables below
```

`run.sh` serves one GGUF at a time from the pond's own model directory with
`--jinja --reasoning-budget 0` (the household runs `thinking_mode = off`), one case at a time.
Concurrent cases looked faster but each slot lost Gemma's sliding-window checkpoint and
re-prefilled the whole prompt. The runs shared the GPU with another application, so
milliseconds are not comparable and E2 compares **prompt tokens**.

## Models

Every GGUF the pond had on this Mac that runs under Homebrew's `llama-server` (build 9110):
Gemma 4 E2B and E4B (QAT, the household's), Llama 3.2 3B, Granite 4.1 3B and Nemotron 3 Nano
4B. Nanbeige 4.2 3B is not in the results: that build has no `nanbeige` architecture.
DeepSeek-R1-Distill 1.5B was left out as a reasoner without tool calling. The non-Gemma
families ran E1 (arms V0, V1, V1P) and E4 only.

## Captures

`payloads/*.json` is the capture from `main` at `e440485b`, the prompt every model in the main
run saw. `payloads/68eef8cb/` is a capture from `main` at `68eef8cb`. That build trimmed the
`<answer-contract>` example and moved the turn budget into the system prompt, with tools
byte-identical. The decisive arms were re-run on it:

```bash
PAYLOADS=$PWD/payloads/68eef8cb OUT=$PWD/results-68eef8cb \
  E1_ARMS=V0,V1 E3_ARMS=M0,MC E2_ARMS=A_V0R0,K_V0R0k,C_V1R1 ./run.sh "e1 e3 e2" <models>
RESULTS=results-68eef8cb python3 -I report.py
```

`capture.sh` makes a new capture from whatever `target/release/pond-server` was built from.

## Fidelity

The bench is only worth something if a model behaves in it the way it behaves in the pond.
Checked: E4B in the bench calls `get_wikipedia_article {"topic": "Wangari Maathai"}` for `r01`,
exactly the call the real pond's E4B made on the captured turn. llama-server and GIAP's
in-process engine both render the GGUF's own chat template, but they are different parsers.
Treat the comparison between arms as the finding, not the absolute rates.

## Results

[`results.md`](results.md) is `report.py`'s output for both captures. The design document
quotes it and says what each number means.
