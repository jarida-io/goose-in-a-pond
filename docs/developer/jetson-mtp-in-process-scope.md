# In-process MTP: scope

> **2026-09-24 -- speculative decoding was taken out of the llama.cpp engine** (goose `743649d98`:
> on Metal the drafter cost 6-36% of decode; on the Orin it had measured 1.8x over eight real turns,
> at the price of a second context, the no-VMM CUDA pool, a drafter download and a model reload per
> switch flip). GIAP's side of it -- the drafter provisioned at startup since `38b733c5`, and a
> Settings > Models switch built the same day -- is commented out rather than deleted, so this
> document is the record of how to bring it back, not a description of what runs.

Measured on the Orin: upstream llama.cpp's MTP gives **47.7 tok/s against the vendored
engine's 15.8** on `gemma-4-E4B-it-qat`, at 87 % draft acceptance, quality unchanged.
See [`jetson-engine-bakeoff.md`](jetson-engine-bakeoff.md). This scopes bringing that
in-process rather than adopting a sidecar.

## UNBLOCKED — the fork builds, 2026-09-08

Route 2 was taken and it works. `~/Documents/Jarida/llama-cpp-rs-giap` is llama-cpp-rs
**0.1.156 with the 0.1.146 OpenAI-compat surface re-applied**, and GIAP builds against it:

```
cargo check -p pond-server -p pond-adapters-goose   Finished
cargo test  -p goose-local-inference --lib          146 passed; 3 failed
```

The three failures are the documented pre-existing host-memory context-cap tests,
unchanged. `cargo fmt --check` clean.

**Both APIs now coexist in one crate** — proven by a compile-only probe importing
`model::ChatTemplateResult`, `openai::{ChatParseStateOaicompat, OpenAIChatTemplateParams}`
and `speculative::{MtpSpeculative, MtpSpeculativeParams}` together. That combination does
not exist in any published version.

### What the port actually needed

**16 of the 17 `common/chat.h` symbols `wrapper_oai.cpp` uses were unchanged**, and
0.1.156's `chat.h` is otherwise a superset. Restored verbatim from 0.1.146:
`wrapper_oai.{h,cpp}`, the `llama_rs_grammar_trigger` and
`llama_rs_chat_template_result` structs plus the free function, `src/openai.rs`, and
`model.rs`'s `GrammarTriggerType` / `GrammarTrigger` / `ChatTemplateResult` /
`apply_chat_template_oaicompat` / `impl ChatTemplateResult`, with `ChatParseError` and
one `ApplyChatTemplateError` variant in `lib.rs`.

Three genuine adaptations:

| Change | Fix |
|---|---|
| `common_chat_msg_diff_to_json_oaicompat` deleted upstream (header *and* definition) | Vendored into `wrapper_oai.cpp` as a static. Pure serialization over `common_chat_msg_diff`, whose layout is **byte-identical** between the trees — so it would fail to compile, not silently misbehave, if that changed. |
| `params.thinking_end_tag` → `thinking_end_tags` (string → vector) | `.empty()` on the vector |
| `dup_string_array` / `dup_trigger_array` dropped | Restored into `wrapper_oai.cpp`, not `wrapper_utils.h` — `wrapper_common.cpp` includes the latter and has neither `<vector>` nor `common/chat.h`, so putting them there broke the compile they were meant to fix |

Two upstream signature changes on GIAP's side: `LlamaSampler::penalties` gained
`n_vocab` (so `build_sampler` now takes the model), and `MtmdBitmap::from_buffer` gained
a `placeholder` flag.

### What is NOT done

**MTP is not wired.** The fork makes it *reachable*; items 4–6 below (the `SessionKv`
ownership problem and the speculative generation loop) are untouched. Nothing has been
measured in-process — the 2.8× is still a llama-server number.

**The fork has no home.** `[patch.crates-io]` uses a path relative to the workspace root,
so the fork must sit beside the repo on every machine that builds it, including the
Jetson (`~/llama-cpp-rs-giap` next to `~/goose-in-a-pond`). A git dependency removes that
and is the right answer once there is somewhere to push it.

**It is a second fork to maintain**, beneath the goose fork, and llama.cpp moves under
both. The three adaptations above are the maintenance surface, and the byte-identical
struct is the one that could bite silently if upstream ever reshapes it.

---

## Why it was blocked (superseded by the above, kept for the reasoning)

The bump was attempted. **It cannot be done.** `llama-cpp-2` removed the
OpenAI-compat chat templating in `0.1.147` — the version immediately after ours — and
added MTP in `0.1.151`. There is no version that has both:

| version | `openai.rs` | `speculative.rs` (MTP) |
|---|---|---|
| **0.1.146** (pinned today) | **present** | — |
| 0.1.147 – 0.1.150 | — | — |
| 0.1.151 – 0.1.156 | — | **present** |

Bumping to reach MTP therefore *removes* the API the live serving path renders every
prompt with. `cargo check -p goose-local-inference` against `=0.1.156` fails with 13
errors; 8 of them are this one cause. On the `-sys` side, `wrapper_oai.{h,cpp}` are
deleted outright with no replacement anywhere in the crate.

### What is lost, and why it is not a port

`apply_chat_template_oaicompat` → `ChatTemplateResult` is not a formatting convenience.
It carries three things the engine depends on:

| Used for | Sites |
|---|---|
| Rendering the prompt with native tools JSON, `enable_thinking`, `parallel_tool_calls` | `inference_engine.rs:1641, 2361, 2629`; `mod.rs:81` |
| **Deciding whether a model supports native tool calling at all** — `template_result_supports_native_tool_calling` does a dry run and reads the answer off the result | `mod.rs:59, 104` |
| **Streaming tool-call parsing** — `streaming_state_oaicompat()` is what turns a token stream into `tool_calls` | `inference_native_tools.rs:37` |

`ChatTemplateResult` is also a public field of the engine's own `PreparedGeneration`
(`inference_engine.rs:305`), so the type is threaded through four modules.

Replacing it means GIAP renders the Gemma chat template itself *and* writes its own
streaming tool-call parser. The fork has `native_tool_parsing.rs` (323 lines) but it
parses text into a message — it is not the incremental parser the streaming path needs,
and `enable_thinking` plus the native-tool capability probe have no substitute at all.

This is exactly the failure mode the recorded history warns about: a `--jinja` streaming
tool-call parser breaking on GIAP's payload is what blocked the direct-llama.cpp route
once already. Re-implementing that parser to gain decode speed trades the thing that
works for the thing that is fast.

### What this means for the recommendation

The bake-off's finding stands — **upstream MTP is worth 2.8× decode on this board** — but
it is not reachable by bumping the crate. The routes that remain:

1. **Wait.** `openai.rs` may return, or the MTP API may be backported. Cheap to check
   before each attempt: the table above is two `curl`s.
2. **Fork `llama-cpp-2`.** Re-apply `openai.rs` and `wrapper_oai.{h,cpp}` from 0.1.146 on
   top of 0.1.156. Mechanically plausible — they are self-contained — but it adds a
   second vendored fork to maintain beneath the goose fork, and llama.cpp's own
   `chat.cpp` will have moved underneath them.
3. **Sidecar after all.** The bake-off measured `llama-server` doing this today with no
   GIAP changes at all. The costs are real (a second supervised process, wall-clock
   telemetry, re-solving `SacrificialContext` server-side) but they are *known*, whereas
   the cost of writing a streaming tool-call parser is not.
4. **Do nothing.** 15.8 tok/s is the current experience and nothing is broken.

My earlier recommendation — "bump the engine, do not adopt a sidecar" — assumed the bump
was available. It is not, and route 3 deserves a fresh look on that basis rather than
being dismissed for costs that are smaller than the alternative's.

### Cost of the check

Two hours, and it landed before any of the hard work in the scope below was started. That
was the point of ordering it first.

---

## Headline: smaller than it looked

I previously called this "new engine work, not a flag". That was wrong on the main
point. **`llama-cpp-2 0.1.156` already ships a safe MTP API** — I had checked
`ModelSettings.draft_model` (which only MLX consumes) and llama.cpp's
`common/speculative.cpp` (which is not part of the public C API) and concluded the loop
would have to be written. It does not.

`llama-cpp-2-0.1.156/src/speculative.rs`:

```rust
pub struct MtpSpeculativeParams { pub n_max: i32, pub n_min: i32, pub p_min: f32 }
pub struct MtpSpeculative<'model> { /* RAII over llama_rs_mtp_speculative */ }

impl<'model> MtpSpeculative<'model> {
    pub fn new(target: LlamaContext<'model>, draft: LlamaContext<'model>,
               params: MtpSpeculativeParams) -> Result<Self, MtpSpeculativeError>;
    pub fn begin(&mut self, prompt_tokens: &[LlamaToken]) -> Result<(), _>;
    pub fn process(&mut self, batch: &LlamaBatch<'_>) -> Result<(), _>;
    pub fn draft(&mut self, n_past: i32, id_last: LlamaToken,
                 prompt_tokens: &[LlamaToken]) -> Result<Vec<LlamaToken>, _>;
    pub fn accept(&mut self, n_accepted: u16) -> Result<(), _>;
}
```

That is the whole draft/verify/accept cycle, backed by `llama_rs_mtp_speculative_*` in
`llama-cpp-sys-2`'s `wrapper_common.cpp`.

## Four things gate it, in order of risk

### 1. The bump is mandatory, not optional — 0.1.146 cannot load the drafter

| | 0.1.146 (vendored today) | 0.1.156 |
|---|---|---|
| `gemma4-assistant` arch | **absent** — only `GEMMA4` | present |
| `MtpSpeculative` | absent | present |
| `llama_model_n_layer_nextn`, `_n_embd_out` | — | present |

So there is no partial path. Without the bump the drafter does not load at all, which is
also why the device's `~/llama.cpp` (a separate, current build) could run it while the
in-process engine could not.

### 2. `common` is a default feature, and GIAP disables defaults

Both `Cargo.toml` and `goose/Cargo.toml` pin:

```toml
llama-cpp-2 = { version = "=0.1.146", default-features = false, features = ["sampler", "mtmd"] }
```

`MtpSpeculative` lives behind `common` (`llama-cpp-2/common → llama-cpp-sys-2/common`),
which is in `default` but excluded by `default-features = false`. **Add `"common"`
explicitly to both files.** This pulls `wrapper_common.cpp` and llama.cpp's `common/`
into the build — a compile-time cost on a board where a cold CUDA build is 40–90 minutes,
and worth measuring before assuming it is free.

### 3. The real design problem: `MtpSpeculative` wants to own both contexts

`MtpSpeculative::new` **takes** `LlamaContext` by value for both target and draft. The
engine's `SessionKv` also owns its context, and does so carefully:

```rust
pub(super) struct SessionKv {
    /// Declared before `_model`: fields drop in declaration order, so the
    /// context is destroyed before the model allocation it points into.
    ctx: LlamaContext<'static>,
    ...
    _model: Arc<LlamaModel>,
}
```

That `'static` is a lifetime transmute over an Arc-stable address, with a hand-written
`unsafe impl Send` justified on the model-slot mutex serialising every access. Handing
that context to `MtpSpeculative` means either:

- **(a)** `SessionKv` holds `MtpSpeculative` instead of a bare context, and every existing
  path (`reuse_prefix`, `prefill_plan`, `clear_kv_cache_seq`, `state_seq_save_file`) goes
  through `spec.target_context_mut()`; or
- **(b)** MTP gets its own slot variant and sessions using it lose the prompt-session
  cache.

**(a) is the only acceptable one.** The KV cache is worth more than MTP on the metric that
matters most here: it turns a follow-up turn from a full re-prefill into a resume
(measured 12.4 s → 0.65 s TTFT). Losing it to gain decode would be a bad trade, and the
bake-off's own follow-up numbers would have caught it.

The wrapper exposes `target_context()`, `target_context_mut()` and `draft_context_mut()`,
so (a) is mechanically available. The work is threading it through, plus re-justifying the
`Send` impl for a struct that now owns two contexts.

### 4. Memory and the second context

The drafter is 57 MB on disk (`mtp-gemma-4-E4B-it.gguf`), but it needs its own
`LlamaContext` and therefore its own KV allocation. Measured cost of the whole MTP
configuration on the device: **peak footprint 4,586 MB vs the baseline's 4,538 — +48 MB**,
with `MemAvailable` bottoming at 1,216 MB against an 812 MB reserve. It fits, and it fits
because llama.cpp's MTP drafter shares the target's KV cache rather than duplicating it.

That is measured through llama-server, not through this integration. **Re-measure after
wiring**, because an in-process arrangement that accidentally gives the drafter a full
`n_ctx` of its own would cost ~896 MiB at 16384 and blow the budget.

## Work items

| # | Item | Files | Risk |
|---|---|---|---|
| 1 | Pin `=0.1.156` + add `"common"` | `Cargo.toml`, `goose/Cargo.toml` (parent wins) | low; API drift across a 10-patch jump needs checking |
| 2 | Re-verify the engine's API surface | `llamacpp/inference_engine.rs` | low–medium |
| 3 | Resolve the drafter path | `ModelSettings.draft_model` exists and `resolve_model_path` already honours `GOOSE_LOCAL_DRAFT_MODEL`; only the MLX backend consumes it today | low |
| 4 | `SessionKv` owns `MtpSpeculative` | `inference_engine.rs` — the `'static` transmute, drop order, and `unsafe impl Send` all need re-justifying for two contexts | **high** |
| 5 | Speculative generation loop | `generation_loop` currently samples one token and decodes one batch; MTP replaces that with draft → verify → accept | medium |
| 6 | `DraftStats` telemetry | `ProviderStats.draft` is already `Option<DraftStats>` and hardcoded `None` at both call sites (`inference_native_tools.rs:250`, `inference_emulated_tools.rs:493`) — wire acceptance rate through | low |
| 7 | Registry plumbing | `apply_jetson_settings` stamps a `draft_model` and the derived contexts for both | low |
| 8 | Fork patch table | `docs/goose-patch-management.md` | low |

Items 4 and 5 are the work. Everything else is wiring.

## What would make this not worth doing

- **The bump regresses the KV cache.** Patches #7/#8/#10 (prompt-session cache, on-disk
  snapshot, KV type + `n_ubatch`) all live in `inference_engine.rs`. If 0.1.156 moved the
  API under them, the re-port cost could exceed the gain.
- **`common` inflates the device build materially.** Measure it on the first build.
- **The drafter cannot be made to share KV in-process.** Then item 4 collapses into
  option (b), and the trade is decode against prefix reuse — which the measured numbers
  say is a bad trade.

## Order

1. Bump and add `common` on a fork branch. Build on the **Mac** first — `cargo check -p
   pond-server -p pond-adapters-goose --all-targets` — and record what the API drift costs
   before touching the device.
2. `cargo test -p goose-local-inference` (three known host-memory context-cap failures are
   pre-existing; do not chase them).
3. Device build, then C1 re-run **without** MTP. That isolates the bump: does the vendored
   engine reach C2-baseline's 17.2 tok/s once it is current? If it does not, the gap was
   never engine vintage and item 4 is premature.
4. Only then items 4–6, and re-run C1 with MTP against the same payloads.
5. Re-verify PAI-3/4/5: decode tok/s feeds the output-reserve latency budget, and the
   prefix-stability rules assume the cache behaves as it does today.

Step 3 is the decision point and it is cheap. It is also the one this scope could be
wrong about: everything above assumes the 2.8× survives moving from llama-server into
GIAP's loop. The section below is what the Mac harness could and could not settle about
that.

## Measured on the Mac, 2026-09-08

Harness: `mtp.rs`'s two `#[ignore]`d tests, run against the real
`gemma-4-E4B-it-qat-UD-Q4_K_XL` + `mtp-gemma-4-E4B-it` pair, M4, Metal.

### Correctness: settled

Greedy equivalence holds at draft depth 1, 4, 8 and 12 — identical **token IDs**, not
merely identical text. Getting there cost three defects, none of which errored and all of
which produced fluent output:

| Defect | How it presented |
|---|---|
| draft context built without `ctx_other` | `failed to create draft context: null reference` |
| `common_speculative_process` never called | `draft: llama_decode[0] returned -1`, inside the drafter |
| KV rollback one position short, **and** the divergent token left undecoded | output diverged at token 3; plain 35 tokens, spec 3 |

The third is the one worth remembering: two independent off-by-ones whose *combined*
symptom was ordinary-looking text. Only comparing token IDs against an unspeculated run
catches that, which is why the test asserts on IDs and sweeps depth — the accept
arithmetic is indexed by draft length, so passing at depth 4 says nothing about depth 8.

### Performance: the loop shape is not the problem

| depth | speedup | drafts kept | tok/step | draft ms/step | verify ms/step |
|---:|---:|---:|---:|---:|---:|
| 1 | 1.14× | 84% | 1.84 | 3.6 | 44.0 |
| 4 | **0.87×** | 62% | 3.50 | 8.8 | 118.9 |
| 8 | **1.19×** | 50% | 5.00 | 16.0 | 112.0 |
| 12 | 1.10× | 33% | 5.00 | 22.8 | 116.3 |

Plain decode: 28.0 tok/s, 35.8 ms/token.

`process` and `rollback` are 0.0 ms/step at every depth. That is the check that the drafter
is sharing the target's KV cache — `common_speculative_mtp::process` skips its catch-up
decode only when `llama_get_ctx_other(ctx_dft) == ctx_tgt`, so a non-zero reading there
means `ctx_other` silently failed to take and every step is paying for a second forward
pass. Worth keeping in any telemetry that ships.

### Why depth 4 loses here, and why that does not transfer

The decisive measurement is the cost of one target forward as a function of batch size,
which is a property of the backend and nothing to do with the drafter:

| batch | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | **9** | 13 | 17 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| ms | 30.1 | 43.7 | 62.1 | 83.4 | 121.5 | 121.4 | 155.5 | 170.2 | **116.5** | 121.8 | 125.4 |
| ms/token | 30.1 | 21.9 | 20.7 | 20.8 | 24.3 | 20.2 | 22.2 | 21.3 | **12.9** | 9.4 | 7.4 |

Batch 9 costs *less* than batch 5. That is ggml-metal's `ne11 > 8` switch from the mat-vec
kernel to GEMM: at or below 8 positions Metal repeats a per-position kernel, so verifying
four drafts costs very nearly what decoding them one at a time costs and there is almost
nothing for speculation to save. Above 8 the cost goes flat, which is the regime
speculative decoding is designed for.

So the Mac's preference for depth 8 is a Metal artifact. **The default stays 4**, the value
the Orin bake-off measured at 2.8× and 87% acceptance, and `inference_engine.rs` carries
that reasoning at the call site so it does not get "fixed" from a Mac run.

### What this does not establish

No in-process number on the Orin. What the Mac shows is that GIAP's loop is not what would
stop the 2.8× — at depth 8 the in-process loop beats plain decoding on a backend whose
batch curve barely amortises at all, and the drafter itself costs 16 ms/step against a
112 ms verify.

Run `target_forward_cost_by_batch_size` on the device **first**, before any tuning. If the
Orin's curve is flat from batch 2 — which is what a bandwidth-starved unified-memory part
should give, and what would explain llama-server's 2.8× at depth 4 — then depth 4 is right
and the port should reproduce it. If that curve is steep, there was never a speedup
available in-process whatever the acceptance rate says, and the sidecar recommendation
would need revisiting.

### Native run on the real serving path, 2026-09-08

The measurements above call `generation_loop` directly. Driving `pond-server` in release
instead — so a turn goes `GooseAdapter` → `GiapProviderShim` → `LocalInferenceProvider` →
`inference_native_tools` → `generation_loop`, with 61 tools, a 7,916-token prompt and the
KV session cache live — found one thing no test could.

**In-process MTP deadlocked the server on first model resolution.** `resolve_model_path`
holds the registry mutex and then called a helper that took `get_registry().lock()` again;
std's `Mutex` is not reentrant. It fired only when a drafter was configured, because the
`None` case never runs the closure — so the plain path, every unit test that builds a
context directly, and the entire llama-server bake-off all missed it. The symptom was not
an error: pond-server sat at 0.1% CPU with 277 MB resident, no model loaded, still
answering `/health`, having logged `turn_start` and nothing after. Fixed by looking the
drafter up through the guard already held.

Two harness rules came out of the same session. A run must fail loudly if the drafter did
not load, or the arm silently becomes a duplicate of the baseline — the first attempt
reported "MTP" numbers for a process that had no drafter at all, because the drafter was
never registered and `draft_model_path` was `None`. And two pond-servers on one
`POND_DATA_DIR` deadlock on `registry.json.lock` while the survivor keeps answering curl,
so the port and data dir both need an exclusivity gate.

With the deadlock fixed and both arms run through the identical gated procedure
(prewarm, warm-up turn, then three timed turns, same 7,916-token prompt):

| arm | turn 1 | turn 2 | turn 3 | median |
|---|---:|---:|---:|---:|
| baseline | 22.11 | 22.20 | 23.16 | **22.20** tok/s |
| + MTP, depth 4 | 21.02 | 20.55 | 19.49 | **20.55** tok/s |

MTP is ~7% slower, and an earlier unmatched baseline run measured 25.9 tok/s median, so
between-run variance on this machine (~17%) is larger than the gap. The honest reading is
**not faster, possibly slower** — which is what the batch-cost curve predicts for depth 4
on Metal, and consistent with the 0.81x measured in isolation.

One thing production makes worse than the test: the registry ships
`sampling: Temperature 0.8`, not greedy. The accept rule here is "sample from the target at
each verified position, keep the draft if it matches" — correct at any temperature, since
every emitted token is drawn from the target's own distribution, but acceptance falls as
temperature rises. The 62% measured at temperature 0 is an upper bound on what production
sampling will see.

**Gap worth closing before the device run:** nothing surfaces draft acceptance in
`TurnStats`. `MtpSession` counts `drafted`/`accepted` and the phase timings, and the tests
read them, but a production turn reports none of it — so on the Orin there will be no way
to tell "drafting well and the hardware cannot use it" from "drafting badly" without
attaching a debugger.

Two harness properties worth carrying to the device: both live tests take a process-wide
mutex, because cargo ran them in parallel once and shared-GPU contention halved every
number without failing anything (plain decode read 12.3 tok/s against 28.0 measured alone,
and depth 8 read 0.52× against 1.19×); and `speculate` calls `llama_synchronize` after the
target decode, because Metal and CUDA both return from `llama_decode` before the graph has
run and the unsynchronised timings blamed the wrong phase entirely.

## On the Orin, 2026-09-08 — in-process MTP works

The fork is hosted (`jarida-io/llama-cpp-rs-giap`, private, pinned by SHA) and the device
builds it: 29m 17s release build with CUDA sm_87, service healthy afterwards. Cargo needed
`net.git-fetch-with-cli = true` in the nano's `~/.cargo/config.toml` — its libgit2 fetch
ignores the `gh auth git-credential` helper the box authenticates with, and the fork is a
private git dependency.

Measured with the service stopped, `cargo test -p goose-local-inference --release
--features cuda`, `n_ctx` 2048, greedy, unsloth MTP drafters:

| target | plain | depth 1 | depth 4 | depth 8 | depth 12 |
|---|---:|---:|---:|---:|---:|
| E2B-qat | 24.4 tok/s | 1.42x | 1.76x | **1.90x** | 1.65x |
| E4B-qat | 15.0 tok/s | 1.57x | 2.14x | **2.28x** | 2.08x |

Greedy equivalence holds at every depth on both models — the same assertion the Mac runs
make, now on the hardware that ships.

### The batch-cost curve is flat here, exactly as predicted

| batch | 1 | 2 | 4 | 8 | 9 | 17 |
|---|---:|---:|---:|---:|---:|---:|
| E2B ms | 31.4 | 30.9 | 38.7 | 62.8 | 39.7 | 50.9 |
| E4B ms | 51.6 | 47.9 | 65.4 | 105.1 | 73.3 | 89.2 |

A batch of 2 costs *less* than a batch of 1 on both. That is the bandwidth-bound regime
speculative decoding is designed for, and it is why the Mac's verdict did not transfer:
Metal repeats a mat-vec kernel below batch 9, the Orin streams the weights once. The
prediction recorded before this run — flat curve here, so depth 4 should work — held.

### Flash attention was worth more than the drafter

The first device run measured **1.45x** on E4B, and I nearly wrote that up as "in-process
gets half what the sidecar got". It was a configuration difference, not an engine one:
`ModelSettings::default()` leaves `flash_attention`, `type_k/v`, `n_batch` and `n_ubatch`
all `None`, so the test ran without flash attention and on an f16 KV cache, while the
bake-off's 2.8x came from llama-server run with `-fa on -ctk q8_0 -ctv q8_0 -b 512 -ub 128`.

Setting the shipped configuration moved E4B from 1.45x to 2.14x at the same depth, and the
drafter's cost fell from 54.3 to 19.5 ms/step. The shortfall was being charged to the
drafter's account, which is where it would have stayed if the phase timings had not been
there to contradict it.

**2.28x in-process against the sidecar's 2.8x.** The remainder is unexplained; the runs
differ in more than one way (`n_ctx` 2048 vs 16384, 35-token generations vs the bake-off's
workload, no `--cache-reuse`), so it is not yet attributable.

### What this does not settle

- **Depth 8 beats depth 4 on both models** (1.90 vs 1.76, 2.28 vs 2.14), against a default
  of 4. One run each, so not enough to move the default — but enough to measure properly.
- **Production samples at temperature 0.8**, and every figure here is greedy. Acceptance
  falls as temperature rises, so these are upper bounds on a real turn.
- **Nothing is wired into the running pond.** The drafter files are on the device
  (`mtp-gemma-4-E2B-it.gguf`, `mtp-gemma-4-E4B-it.gguf`, both `gemma4-assistant`) but
  neither is in the registry, so the household pond is still decoding without speculation.
- **No end-to-end device number**, so the turn-level effect of a 1.9x decode is unmeasured.

## A real turn on the device, 2026-09-08 — 1.57x, then it crashes

Driven through `pond-server` on the Orin against an isolated data dir with hard-linked
weights, E2B-qat, 61 tools, ~7,840-token prompt, the pond's own derived `n_ctx` of 16384:

| arm | turn 1 | turn 2 | turn 3 | … | median |
|---|---:|---:|---:|---|---:|
| baseline | 27.5 | 31.1 | 31.0 | 8 turns, all clean | **31.0 tok/s** |
| + MTP | **48.7** | **CRASH** | — | — | — |

**1.57x on the turn that completed.** Then, creating the context for turn 2:

```
CUDA error: out of memory
  cuMemAddressReserve(&pool_addr, CUDA_POOL_VMM_MAX_SIZE, 0, 0, 0)
ggml/src/ggml-cuda/ggml-cuda.cu:106: CUDA error
```

Aborted, core dumped. The board had ~5 GB free, so this is CUDA *pool address-space*
reservation failing, not the weights failing to fit.

### What it is not

I guessed this was context churn — an MTP session builds two contexts (target and drafter)
where a plain one builds one, and the crash came on the fourth context created. Running the
baseline for **8 turns** disproves that: it creates a context per turn and never fails. The
second context is what breaks it, not the rate of creation.

It is also not a leaked context. `MtpSpeculative` owns both `LlamaContext`s as fields, so
its `Drop` frees the speculative state and then both contexts. That was worth checking
because `common_speculative_free` does not free the contexts itself -- llama-server frees
them separately -- and a wrapper that forgot them would leak exactly like this.

### What is still unknown

Whether a smaller window fixes it. I tried `GOOSE_CONTEXT_LIMIT=8192` and the run still
used 16384 and still crashed, because a pinned registry `context_size` outranks that env
var -- and `apply_jetson_settings` re-stamps `context_size` at every provider init, so
editing the registry by hand cannot test it either. **The code is the only knob**, which is
also why the drafter cannot be enabled by configuration: `apply_jetson_settings` builds a
fresh `ModelSettings { .., ..Default::default() }`, so a hand-set `draft_model` is erased
on the next provider build. The measurement above used `GOOSE_LOCAL_DRAFT_MODEL`, which is
read at resolve time and survives the stamping.

### What this means

The speedup is real and roughly what the isolated test predicted (1.57x end-to-end against
1.76x at depth 4 in the harness, on a turn that is 2-3 inferences rather than one). But
in-process MTP is **not shippable on this board at `n_ctx` 16384** until the second
context's allocation is accounted for.

The concrete gap: `jetson_context_size` derives the window from
`LLM_BUDGET_MB - model_mb - COMPUTE_BUFFER_MB` and knows nothing about a drafter. With MTP
on it must also subtract the drafter's weights and, on the evidence here, whatever the
second CUDA context reserves -- which is the number nobody has measured yet.

## Shipped and measured on the Orin, 2026-09-08

Eight consecutive real turns, E2B-qat, 61 tools, ~7,840-token prompt, drafter provisioned
by the pond itself from an empty scratch data dir:

| | turn 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| baseline | 25.1 | 31.0 | 30.8 | 28.6 | 32.0 | 28.5 | 31.1 | 31.1 |
| + MTP | 56.3 | 45.0 | 48.4 | 57.7 | 55.9 | 58.3 | 55.9 | 57.7 |

**~1.8x end-to-end, and no crash.** The turn-2 abort is gone.

### The crash was never the context window

`cuMemAddressReserve(&pool_addr, CUDA_POOL_VMM_MAX_SIZE, ...)` reserves **32 GB of GPU
virtual address space per pool**, and a backend context holds up to eight of them
(`pools[GGML_CUDA_MAX_DEVICES][GGML_CUDA_MAX_STREAMS]`, created lazily per stream). One
llama context survives that on an Orin; MTP needs two. The reservation is a fixed size
whatever `n_ctx` is, so shrinking the window -- the obvious reading of "CUDA out of memory"
-- could not have helped, and `jetson_context_size` was never the bug.

Fixed by building against `GGML_CUDA_NO_VMM`, already exposed as `llama-cpp-sys-2`'s
`cuda-no-vmm` feature. `ggml_cuda_init` now reports `VMM: no`, and the speedup survives it.

### The budget still owed the drafter

Separately from the crash, `jetson_context_size` sized the window from
`LLM_BUDGET_MB - model_mb - COMPUTE_BUFFER_MB` while a second set of weights would be
resident for the whole session. It now charges the drafter's real file size plus a 64 MB
allowance, **once, not per token** -- the drafter shares the target's KV through
`ctx_other`, which is visible as `process` costing 0.0 ms/step, so its cost is flat in
`n_ctx` and belongs in the budget rather than the slope. The startup line now carries
`drafter_mb`.

The 64 MB is deliberately above what I measured. The honest measurement -- `MemAvailable`
either side of building a drafter context -- read 38-47 MB at 8192 and 16384 against a
57 MB file, and it is an under-estimate: mmap'd weights come out of reclaimable page cache,
which `MemAvailable` counts as available. Two of its four rows read 0 MB for a context that
cannot cost nothing, so it is not a number to build on.

### Nobody has to know any of this

The drafter is derived from the chat model's family, fetched once, validated, registered and
attached. Failure at any step leaves the pond decoding exactly as before; the notification
is the last resort.

Three defects on the way there, all of the same shape -- code that looked right, compiled,
and never ran:

| Defect | How it presented |
|---|---|
| Registration lived in `pond-adapters-local-inference` | That crate feeds the QUARANTINED PondAgent loop. The live path is `GooseAdapter` building `LocalInferenceProvider` directly and never calls `new_with_data_dir`. Drafter on disk, validated, no registry row. |
| `draft_model` written to the wrong registry row | `apply_jetson_settings` stamps the settings spelling (`gemma-4-E2B-it-qat-UD-Q4_K_XL`); the engine loads the canonical stem (`gemma-4-E2B-it-qat`). Two rows, one registry. |
| The "already registered" early return | The function is called twice, with the two spellings. The first call registered the drafter, so the second -- the only one holding the id that matters -- returned before pointing the target at it. |

### The tuning row, fixed

**The same registry split was leaving the whole Jetson tuning block inert.** Read off the
device before the fix, the row inference actually resolves:

| setting | row the engine reads | row that was stamped |
|---|---|---|
| `context_size` | None | 16384 |
| `n_gpu_layers` | None | 99 |
| `flash_attention` | None | true |
| `type_k` / `type_v` | None / None | q8_0 / q8_0 |
| `n_batch` / `n_ubatch` | None / None | 512 / 128 |
| `n_threads` | None | 4 |

So the pond was running with an f16 KV cache instead of q8_0 and llama.cpp's default
2048/512 batch instead of 512/128. This repository's own recorded measurements for those two
settings -- KV 296 -> 157 MiB, peak footprint 437 -> 307 MB for the cache, and a compute
buffer of 522 MiB at `n_ubatch` 512 against 129 MiB at 128 -- put the avoidable footprint
at roughly 700 MB on a 7.6 GB board. Those are prior numbers from the source comments, not
re-measured here.

`n_gpu_layers` was the harmless one: `llama_model_default_params` sets it to `-1`, which
offloads everything, so the board was never accidentally on the CPU.

`apply_jetson_settings` now stamps **every row whose `local_path` resolves to the same
GGUF**, matched on the resolved path rather than on a name rule -- the two spellings come
from two different canonicalisers in two crates, and symlinks are followed because the
startup hf_cache migration turns `models/gguf` entries into links into `hf_cache` blobs.

After the fix, both rows carry `ctx=16384 fa=True k=q8_0 v=q8_0 batch=512/128 thr=4`, and
eight consecutive turns run at 51.3-53.1 tok/s (median 52) with `MemAvailable` bottoming at
2,317 MB. Note the decode spread narrows against the untuned run's 45-58 tok/s while the
median moves little; the tuning buys footprint, not throughput.

**Two rows for one file remains the real defect.** Nothing here removes it; stamping all of
them is what keeps the engine's row correct whichever one it picks.

### Still open

Everything above is greedy or default sampling on one board; production samples at
temperature 0.8, where acceptance is lower. And `TurnStats` still surfaces no acceptance
rate, so a future regression would be invisible without a debugger.
