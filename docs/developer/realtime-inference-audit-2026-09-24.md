# Realtime inference audit, 2026-09-24

Where a turn's time goes on the live serving path today, measured bottom-up: the llama.cpp
engine first, then the prompt bytes the engine is handed, then the agent loop above it. Mac
(Apple M4, 24 GB, Metal) throughout; the Orin numbers quoted are the device's own from
`turn_metrics` and earlier device runs. Every scratch measurement used an isolated
`POND_DATA_DIR` with the models **hard-linked** in (never symlinked, see
`scripts/pai-bench.sh`), `tool_selection_mode = "minimal"`, `thinking_mode = "auto"`,
`llm_temperature = 0`, `goal_check_enabled = true`, the same four-message script, and the
provider payload of every inference captured with `GIAP_CAPTURE_PAYLOAD`.

## 1. What the real pond recorded

`turn_metrics` in the household pond's `pond_logs.db`, read-only. Averages per day, E4B-qat.

| day | turns | prompt tok | TTFT ms | prefill ms | prefilled tok / turn | decode tok/s |
|---|---:|---:|---:|---:|---:|---:|
| 2026-09-11 | 13 | 6,937 | 2,046 | 818 | 858 | 15.4 |
| 2026-09-17 | 7 | 4,329 | 5,249 | 18,159 | 6,142 | 16.4 |
| 2026-09-24 | 18 | 5,200 | 68,466 * | 30,472 | 6,237 | 14.0 |

\* two turns timed out at 300 s and 760 s with zero output; the median TTFT that day was 5.6 s
and the worst finished turn 41 s. Individual rows: 3 to 6 inferences per turn, 2,457 to 13,333
tokens re-prefilled per turn, `reengagements` 1 to 2 on several rows, decode 8 to 19 tok/s
(the daily binary is `target/debug`, see section 3).

Between the two dates the branch handed context management back to goose (C0 to C3,
2026-09-11), which re-enabled goose's `<turn-context>` injection on every provider call.
Section 4 shows that block is where the re-prefill comes from.

## 2. The engine is at the hardware ceiling in release

Same model (gemma-4-E2B-it-Q4_K_M), same four turns, quiet machine.

| binary | prewarm prefill (1,210 tok) | no-tool turn TTFT | no-tool turn prefill | decode tok/s |
|---|---:|---:|---:|---:|
| `target/debug` (the daily server) | 3,772 ms, 314 tok/s | 1,727 to 1,902 ms | 3.3 to 4.0 s | 26.8 to 30.4 |
| `target/release` | 1,771 ms, 683 tok/s | 1,027 to 1,044 ms | 1.8 s | 32 to 46.5 |
| Homebrew `llama-bench` b9110, Metal, `-fa 1` | pp512 672 tok/s, pp2048 584 tok/s | | | tg128 43.8 (± 17.8) |

The release engine matches the standalone build on both prefill and decode. There is no
wrapper overhead to remove at this layer: the fixed cost between prefill end and first token
is 60 to 310 ms per inference in release, and the larger gaps (up to 1.6 s on E4B) are tokens
held back while the parser decides whether a `<think>` or tool-call opener is one, not
overhead.

The debug run above was contended by a concurrent release build, so its absolute numbers are
pessimistic; the prewarm ratio (2.1x) was measured before that build started and stands.

Consequence: `npm run dev:server` runs `cargo run -p pond-server -- serve` (debug), and the
server on this machine today was `./target/debug/pond-server serve --native`. The household
pays 1.5 to 2x on every token for it. `scripts/stage-server-sidecar.sh` already builds the
packaged sidecar in release.

Also measured at this layer, both worth knowing and neither worth fixing today:

- A fresh data dir hashes every real GGUF under `models/gguf` with SHA-256 at first start
  (`hf_cache_migration::sha256_first_40`). Debug: 11 minutes for 9.3 GB, at 100% of one core,
  before the server binds. Release: about 30 s. One-time per new real file.
- The release binary dies with `SIGABRT` in a C++ static destructor (`mtmd_helper_logger`,
  `__cxa_finalize`) on exit, after serving cleanly. Same class as the known `exit 134` of the
  chat CLI.

## 3. Speculative decoding loses on Metal

`speculative_decoding_enabled` defaults to `true` and the E2B drafter is present on this pond,
so the daily server was decoding with MTP at draft depth 4. Same turns, release binary.

| configuration | decode tok/s, turns 2 to 4 (30 to 56 output tokens) | turn 1, long output |
|---|---|---|
| MTP off | 32.0 / 42.4 / 40.6 | 46.5 tok/s over 746 tokens |
| MTP depth 4 (`mtp_draft_max = 4`, the default) | 29.5 / 39.9 / 31.4 | 31.5 over 505 |
| MTP depth 8 | 26.5 / 38.3 / 29.2 | 29.7 over 584 |

0.64x to 0.94x. Metal's mat-vec path repeats a per-position kernel at or below 8 positions
(`project_mtp_batch_cost_curve`), so verifying drafts costs about what decoding them costs, and
depth 8 does not recover it here either. On the Orin the same code measured 1.8x over eight
real turns. The switch should be off on macOS and on for CUDA; until the default is
platform-aware, turning it off in Settings is a free 10 to 30% on every Mac answer.

## 4. Where the re-prefill comes from: prompt bytes that move

The KV prefix cache works exactly as designed. What it is handed is not append-only. Every
inference's final payload was captured and consecutive payloads diffed.

### 4.1 Goose prepends `<turn-context>` to a message the engine already cached

`inject_moim` (goose, `agents/moim.rs`) inserts a `<turn-context>` block — current time to the
minute, working directory, optionally compaction and turn-budget lines — at the FRONT of the
last user-role message, on a clone, for each provider call. It is never stored. So:

- Within a tool turn the holder is the same user message, but the block's bytes and position
  shift as goal-nudge and steer messages become the newest user-role message; the previous
  holder changes from its first byte and everything after it is decoded again.
- Across turns, turn N's user message loses the block on turn N+1 (it was never stored), so
  the divergence sits at the start of that message rather than at the end of the cache.
- When the model produces no assistant text (a thinking-only turn), goose merges consecutive
  user messages into ONE growing message. The block then moves from the middle of that message
  to its front on every call and the **entire conversation** re-prefills each time.

Measured on E2B, release binary, prefill plans read off `prompt prefill plan`:

| case | cached tokens | reused | re-prefilled per inference |
|---|---:|---:|---:|
| ordinary no-tool turn, first inference | 2,715 | 2,555 | 160 (previous holder's tail + new message) |
| same turn, completeness-check inference | 3,115 | 2,644 | 471 |
| tool turn, each inference after the first | 1,738 to 4,740 | **721** (the tools block ends at 721) | 1,000 to 5,150 |
| merged-message session (R4), every call | up to 5,175 | 721 to 743 | whole conversation: 10,443 / 16,310 / 21,235 tokens per turn |

The payload diff for the merged-message case shows the block moving from after the steer text
to the top of the single user message, with nothing else about system prompt or tools changing
(21 of 21 consecutive pairs byte-identical on both).

The fix is placement, not removal: append the block as the last content item of the holder.
Landed in the fork as commit `36413f065` on `feat/llama-cpp-2-0.1.156-oai-fork` (also fork
`main`; ledger row in `docs/goose-patch-management.md`); the holder stays byte-identical up to
the block and only the block and what follows it re-prefills. The companion timing patch is
`afe7adc69`. Verification runs are in section 6.

### 4.2 GIAP's own per-turn churn

- **The completeness check** (`goal_check_enabled`, default on) appends a synthetic user
  message ("Finish anything still outstanding for this: ...") after every answer that called no
  tool and runs the model **again**. The second answer is suppressed from the stream (fork patch
  "the completeness check must not reach the user"). Cost per no-tool turn: one extra
  inference (about 1.0 s TTFT plus its decode on E2B), the nudge message permanently in history,
  and 470 tokens of `<turn-context>` churn. `inference_count = 2` on every no-tool turn in every
  run here is this.
- **Re-engagement** (`EMPTY_TURN_STEER`): a turn that reasons and emits nothing is re-run with
  the steer appended to the user text, up to twice. E2B at temperature 0 did this on 4 of 8
  first turns across the release runs; R4 hit it on every turn and three of its four turns still
  ended with no answer after 5 inferences and 14 to 35 s of prefill each.
- **`<system-context>`** is now stable in history (C1 stopped stripping it), so it costs
  context (about 350 tokens per turn kept forever) but no longer costs re-prefill.

### 4.3 The lane did not evict the cache

The summary-refresh job takes the lane slot every 30 s once a conversation is idle past
`summary_idle_secs`, cancels on user activity via a 500 ms watcher, and its one observed
inference ran as `SacrificialContext` (prompt 1,214 tokens against a 4,157-token cache), so the
chat prefix survived. A refresh whose transcript exceeds half the cached tokens would instead
run `FullPrefillInPlace` and evict the conversation; that path was not reached in these runs,
and the return-turn cost that looked like eviction on the real pond is explained by 4.1. The
engine-side protection (a memory-gated second retained slot, so a non-matching side prompt
never evicts a conversation) is designed, not built; on the Orin its gate would keep one slot
for E4B and allow two for E2B.

## 5. Thinking is the wall clock on E4B

E4B-qat, release, same turns: `reasoning_tokens` 208 to 280 per turn, `completion_tokens` 400
to 480 for answers of 26 to 118 characters. "What time is it right now?" spent 419 tokens, about
14 s of decode at 30 tok/s, on a one-sentence answer. E2B with `thinking_mode = "off"` (R6)
answered the same first turn in 48 completion tokens against 746 with thinking on (R1); TTFT was
unchanged at about 1 s because TTFT is prefill-bound. This is the agent layer and it is the
largest single wall-clock term for E4B users; it is reported here, not changed.

## 6. Verification of the MOIM placement patch

Same script, same release build with only the fork patch applied. "Check" is the
completeness-check inference that follows the visible answer on a no-tool turn.

| model | metric | prepended (R1 / R3) | appended (R7 / R8) |
|---|---|---:|---:|
| E2B | check inference: reused of cached | 2,644 / 3,115 (471 redone) | 2,310 / 2,407 (97 redone) |
| E2B | check inference TTFT | 1,011 to 1,103 ms | 270 to 386 ms |
| E2B | next turn, first inference: tokens redone | 589 | 147 |
| E2B | no-tool turn: tokens prefilled (turn 2 / 3 / 4) | 1,683 / 1,133 / 1,153 | 727 / 677 / 685 |
| E4B | no-tool turn: tokens prefilled (turn 3 / 4) | 1,092 / 1,111 | 662 / 678 |
| E4B | check inference TTFT (turn 3 / 4) | 1,962 / 2,169 ms | 749 / 752 ms |
| E4B | tool turn: tokens prefilled (turn 1 / 2) | 4,009 / 3,647 | 3,945 / 3,086 |

The captured payloads show the block at the END of the last user message in every call
(offsets 233 to 1,577 of messages 404 to 1,748 chars long). The model's behaviour did not change
in any observable way: same tools called on the same turns, answers of the same shape.

What remains on tool turns is goose's own history rewriting, not the block: on E4B the prompt
shrank mid-turn (2,934 to 2,618 tokens with reuse falling to 1,340; 4,428 to 4,338 with reuse
3,061) when the completeness-check and tool-pair machinery replaced messages. That is item 4 of
the plan below. The hatch itself still costs one full re-prefill per session the first time a
group is enabled, because the tools block sits before the conversation (item 8).

A second engine patch landed alongside: `prefill_ms` now waits for the GPU before it is
stamped. Metal returns from `decode` once the graph is submitted, so a 160-token warm prefill
read 13 to 33 ms and 250 to 360 ms of its work surfaced as a gap between "prefill end" and the
first token. TTFT was measured at the first emitted piece and was already honest; the rate
columns derived from `prefill_ms` were not.

## 7. Ranked plan, lowest layer first

| # | layer | lever | evidence | status |
|---|---|---|---|---|
| 1 | binary | run the daily server from `target/release` | 1.5 to 2x on decode and prefill (section 2) | recommendation; `npm run dev:server` and the `--native` launch both build debug |
| 2 | engine config | speculative decoding off on Metal, on for CUDA | 0.64 to 0.94x with it on (section 3) | setting today; platform-aware default proposed |
| 3 | prompt bytes | `<turn-context>` appended, not prepended | section 4.1 | fork patch landed, verification pending in section 6 |
| 4 | prompt bytes | keep history append-only: no rewrite of a stored message by steer or nudge; `<turn-context>` disabled for local providers if the residual still costs | section 4.1, 4.2 | proposal |
| 5 | agent loop | completeness check only after a turn that used tools, or off | 2 inferences per no-tool turn (4.2) | proposal, Jerry's call: it was measured worth its cost on 2026-08-12 for a seven-tool turn |
| 6 | agent loop | thinking off or bounded for short answers; re-engagement cap | section 5, 4.2 | proposal |
| 7 | engine | second retained KV slot, memory-gated, so side prompts never evict a conversation | section 4.3 | designed, not needed by today's evidence |
| 8 | tools | the minimal-mode hatch costs a round trip and a tools-block prefix change per session | R3, R4 plans | later, after 3 to 6 |

Nothing below the engine (kernels, `n_ubatch`, KV quantisation) is justified by today's Mac
numbers: release GIAP prefill and decode equal the standalone build. The Jetson keeps its
measured tuning block; every device number in this document is quoted from the device.

## 8. Reproducing

```bash
# isolated pond, hard-linked models, release binary, four turns, engine plans + payload captures
SQLX_OFFLINE=true cargo build --release -p pond-server
scripts/pai-bench.sh --no-build            # the blessed harness; this audit used a smaller sibling
```

Read per inference from the log with `RUST_LOG=info,goose_local_inference=debug,giap::kv=debug,
pond_adapters_goose=debug`: `prompt prefill plan` (plan, cached_tokens, prompt_tokens),
`kind="inference"` (engine_ttft_ms, engine_prefill_ms, cause), `provider payload size`, and the
`payload-NNNN-<tools>t-<hash>.json` files that `GIAP_CAPTURE_PAYLOAD=<dir>` writes. Two
consecutive payloads whose `messages[k]` differ at the first byte are the whole diagnosis.
