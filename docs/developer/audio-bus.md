# The audio bus

`pond_audio_out::bus()` is the speaker for the whole process. It is used like a critical section:
there is one instance and one holder at a time, every sound reaches it the same way, and a queue
hands it on. It also publishes its state, so nothing has to ask it whether it is free.

## Claims and lanes

A sound calls `bus().claim(Claim, most, still_wanted)` and plays only while the returned `Floor`
holds the bus; dropping the `Floor` frees it. Claims queue in three lanes, served highest first and
in arrival order within a lane:

| Lane | Claims | Why |
|---|---|---|
| 2 | `Ping` | The wake ping answers the person. |
| 1 | `Turn`, `Speech` | A voice turn, and words said whole outside one (greeting, farewell, unprompted speech, the pairing code). |
| 0 | `Notice`, `Code`, `Sample`, `Chime`, `Test`, `Probe` | Reminders, previews, chimes, sound tests and speaker probes. |

A claim waiting in a higher lane asks a lower-lane holder to yield. `Floor::keep()` then answers false,
playback stops within 25 ms and the holder drops its floor. A notice cut this way queues again and picks
up at the sentence it was cut in.

A turn holds a `TurnFloor` from `begin_utterance` to `end_utterance`, its thinking included, so nothing
plays between its sentences. Nothing expires a hold: `ChatService`'s `TurnEnds` guard ends every turn,
the next `begin_utterance` releases a stale one, and a claim that waits a minute logs the holder.

## What it tells the rest of the pond

- `subscribe()`: a `watch` of `BusState`, who holds the bus and how many claims wait.
- `subscribe_held()`: whether anything holds it. `Quiet::follow` keeps Spotify paused for as long as
  this is true, and each hold waits once for that pause (`Floor::settle`) before its first sound.
- `kind="audio_bus"` trace lines for every hold, yield and release.

## The mic and speaker check

The device supervisor's check (`Prober::hears`) spans both buses, so it takes them in one order:
the speaker bus first, as a `Test` claim that waits out a reply, then the microphone owner's
exclusive slot (`MicHandle::run_exclusive`). The check holds both for the whole test, and the
microphone owner never waits on the speaker. A speaker probe takes the bus before it suspends a card.

## Rules

- Never open an output stream or play a sound without a `Floor`. `route::play_samples_exactly` and
  `play_chime` take the floor as an argument; a device probe takes one before it suspends a card.
- Never wait for the bus to be free and then play: claim it.
- A sound whose device takes nothing for 3 s stops and gives the bus back, and a stall on the
  routed speaker is reported to the device supervisor (`route::report_stall`).
- Take the speaker bus before the microphone's exclusive slot, never the other way round.
- A bus elsewhere in the pond follows the same pattern: one instance, one queued claim, published
  state.
