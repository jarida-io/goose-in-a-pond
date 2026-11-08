# The inference bus

The on-device model has one context. Every call to it goes through one bus:
`inference_bus()` in `crates/pond-core/src/models/domain/inference_bus.rs`, built on the generic
`Bus` in `crates/pond-bus`. A claim queues in its lane, one holder uses the model at a time, a
holder in a lower lane is asked to yield when a higher one waits, and every change is published.

## Lanes

| Lane | Claim | Who |
|---|---|---|
| 2 | `Turn` | A person's turn, from its start to its end, its own helpers included |
| 1 | `Task` | A long task's step, which holds the bus across its helper's calls |
| 0 | `Chore` | Side calls: titles, summaries, memory extraction and consolidation, suggestions |

Within a lane, first come first served. A higher lane waiting sets the holder's `yielding`, and
the holder stops at its next token.

## The doors

Two places call the chat model, and both hold the bus for the whole call:

- `GiapProviderShim::stream` (`crates/pond-adapters-goose/src/provider_shim.rs`): every agent call,
  turns and helpers.
- `GooseProviderAdapter::complete` (`crates/pond-adapters-goose/src/provider_adapter.rs`): every
  side call through `LlmProvider`.

A call runs under the hold its caller gives it, else it claims the bus for itself:

| Hold | Where it comes from | The call |
|---|---|---|
| `Floor` | `claim_device_for_turn`, kept in the session's `SessionControls`; a child of the turn inherits it through the device ledger | Uses the turn's floor; claims nothing |
| `Held` | `LongTaskHelpers::run`, as a task-local (`running_under`) | Takes one burst of the step's `HeldFloor` |
| `Lane` | `running_under(InferenceHold::Lane(..))` | Claims the bus in that lane |
| none | | Claims in the door's lane: a helper's call `Task`, an agent call `Turn`, a side call `Chore` |

A hosted provider has no one context to share, so its calls take no claim.

Calls inside a hold never claim the bus again: a turn that claimed and then queued behind its own
floor would wait forever. Run new model work under a hold, or let its door claim for it.

## A stop

The door installs `request_context::stopping_when` around the call. The goose engine asks it
before it takes the model and on every generated token, including tool-call and thinking tokens
that send nothing. A stopped call keeps what its cache decoded and its stream just ends; the
caller knows it was cut, having asked. A prefill already running finishes first: a voice turn
claims at the wake, so that drain happens while the person is still speaking.

- An agent call below a turn (a helper's, under `Held` or its own floor) is read whole through the
  shim and, if it was cut before it finished, asked again once it holds the bus anew. goose never
  sees the cut.
- A side call through `LlmProvider` returns an error, and its chore runs again on a later tick.
- A turn's calls stream live and are never asked to yield: nothing outranks a turn.

## A long task's step pauses

A step's helper holds the bus across its calls, so a chore cannot take the model between them
and evict the helper's cache. When a turn queues, `HeldFloor::give_way` releases the bus if no
call is running, or the running call stops at its next token. The helper's run, its goose session
and its tool results stay in memory. Its next call queues in the `Task` lane and, after the turn,
loads the role snapshot and prefills its transcript instead of regenerating it. The step keeps its
attempt and its session: it is never rerun because a person spoke.

## Published state

- Trace lines with `kind="inference_bus"`: `held` (with `waited_ms`), `yield`, `free` (with
  `held_ms` and `yielded`).
- `kind="generation_stopped"` from the engine, and `kind="inference_call_resumed"` when a helper's
  cut call runs again.
- `inference_bus().subscribe()` for code; `turn_has_the_model()` for "is a person's turn on".
- The inference lane reads the bus: a turn queueing cancels the chore holding the lane slot.
- The long-task view sends `paused_for_turn`, and the card says "Paused for your conversation".

## Not on this bus

- `gpu_gate` keeps ONNX Runtime setup apart from ggml compute. Compute runs side by side, so
  Whisper never queues behind a decode.
- The tool caller (`pond-adapters-local-inference/src/tool_caller.rs`) is a separate small model
  in its own slot, and runs inside a turn's tool call.
- The embedder is a separate model with its own slot (`embedding_slot_runner.rs`).
- The desktop voice child runs in its own process.
