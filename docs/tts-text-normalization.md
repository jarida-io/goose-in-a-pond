# TTS text normalization

How model output becomes text Kokoro can say: split into speech units as it streams, stripped of
markdown, and with symbols, units and abbreviations spelled out ("5km" as "5 kilometers", not
"k m"). All of it is in `crates/pond-voice/src/text.rs`.

## Where it runs

```
model tokens -> ThoughtFilter (reasoning removed) -> ReplySplitter::push(text)
  -> SpeechPipeline -> VoiceOutput::synthesize_unit(unit)
       tokenizer::chunk: strip_markdown_for_speech -> normalize_for_speech
                         -> phonemize (espeak-ng, per run of speech, punctuation kept)
                         -> vocab ids -> chunks of at most 510 tokens
       each forward pass trimmed to its speech, then a gap unless the unit is the last
  -> held until what is still owed can follow, then played
```

| Step | Function | Callers |
|---|---|---|
| Hold a streaming reply and give it up as units | `ReplySplitter::push`, `pause` when a tool starts, `finish` at its end | `ChatService::stream_response_inner` (voice loop) |
| Split a whole text into units | `split_text` | `Announcer::speak_text` (`crates/pond-audio-out/src/announce.rs`, spoken scheduled tasks, once their reply is complete), `KokoroOutput::speak` |
| Synthesise and play one reply | `SpeechPipeline` (`crates/pond-core/src/models/services/voice/speech_pipeline.rs`) | the voice loop |
| Markdown to plain text, then normalize | `strip_markdown_for_speech` -> `normalize_for_speech` | `tokenizer::chunk` in `crates/pond-adapters-kokoro/src/tokenizer.rs`, so every Kokoro synthesis: the voice loop, `AudioBus`, `POST /api/v1/tts`, voice previews |

Normalization happens inside the Kokoro adapter, not in the callers, so every path gets it once.
If normalizing leaves nothing, the raw text is phonemized instead. The token stream shown in the
UI and stored in history is the model's text, not the normalized one.

## Splitting into speech units

Kokoro shapes the pauses of whatever it is given in one forward pass: about 0.35 s at a comma,
0.5 to 0.6 s at a full stop. So the pond does not cut a reply at its punctuation. A reply is held
while the model writes it and handed to Kokoro whole.

A unit is a `SpeechUnit`: its text, and `last` when nothing follows it. `ReplySplitter` gives one up:

| When | What is given up |
|---|---|
| The stream ends (`finish`) | Everything held, the final unit marked `last` |
| The model stops writing to use a tool (`pause`) | Everything written so far |
| The held text passes one pass (`push`) | The longest start of it that fits |

One pass holds 200 estimated tokens (`UNIT_TOKEN_BUDGET`, about 1.1 per character, so 181
characters or some 11 s of speech). Kokoro takes 510, but on the nano its speech is even only to
about 200: by 300 it speaks 5 to 8% faster and costs more work for each second of speech.

A text longer than a pass is the one place the pond picks a cut (`fitting_cut`): the last sentence
end (`.`, `?`, `!` or an ellipsis, with closing quotes or brackets, then whitespace) or line end
that fits; failing that the last space. A full stop after a digit is not an end, so a decimal or a
list number stays with what follows it. `split_text` cuts a text that is all there the same way
(announcements, greetings, the news).

## The hold before the first sound

Units are synthesised one at a time (`SpeechPipeline`). An engine slower than its own speech (the
nano's Kokoro takes 1.25 to 1.4 s for each second) cannot have the second unit of a long reply
ready when the first ends, and would stop between them. So a sound that starts from silence is
held until the units still owed can follow it without a gap (`hold_secs`): the pipeline predicts
each owed unit's synthesis and speech from the unit it has just measured, and logs the wait as
`kind="speech_hold"`. A reply of one unit is never held, nor is one on an engine that keeps ahead.
The working tone plays through the hold.

Speech outside a turn (`Announcer`, `KokoroOutput::speak`) synthesises all of its units before it
plays any: nobody is waiting on its first word.

## Between units

Each forward pass is trimmed to 30 ms before its first sound and 50 ms after its last (a frame
counts as sound within 40 dB of the loudest and above -50 dBFS): Kokoro puts about 0.3 s of
near-silence before and 0.5 s after every pass. A unit that another follows then ends in 0.5 s of
silence (`UNIT_GAP_SECS`, divided by the speed), about what Kokoro leaves at a full stop inside a
pass; the `last` unit ends at rest.

`POST /tts` takes `{text, last?}`. Without `last` the audio ends in the gap, so a caller that plays
several units back to back needs no silence of its own, and sends `last: true` with the final one.
A text longer than a pass is cut as above and comes back as one WAV. The DAC wake pre-roll is
played only before the first audio after idle, never between units of one reply.

The style row also sets the pace: Kokoro's voice packs hold one style per utterance length, row
`n - 1` for `n` tokens, and a higher row speaks slower. Units past 125 tokens share row 124
(`STYLE_LENGTH_CAP`), because at their own rows long units ran about 9% slower than short ones.

## Markdown

`strip_markdown_for_speech` works line by line, joins lines with a space (with a comma when a list
line on either side has no mark of its own), then calls `normalize_for_speech`:

- Code fences (```` ``` ```` or `~~~`) and everything inside them are dropped, as are horizontal
  rules and images.
- Headers, blockquote markers, list markers (`-`, `*`, `+`, bullet, `1.`) and task checkboxes
  (`[ ]`, `[x]`) are removed.
- Links keep their text; bold, italic, strikethrough and inline code keep their content.

## `normalize_for_speech`: order of checks

One pass, character by character. The first rule that matches consumes the characters:

1. **Time** at a digit not preceded by a digit: `H` or `HH` (0 to 23), optional `:MM`, optional
   `am`/`pm` not followed by a letter. A bare number is not a time. `3:45pm` -> "3 45 PM",
   `15:30` -> "15 30", `3:00pm` -> "3 PM".
2. **Unit suffix** right after a digit, with at most one space (`UNIT_SUFFIXES`).
3. **Degree sign**: `°C` "degrees Celsius", `°F` "degrees Fahrenheit", `°K` "kelvin", bare "degrees".
4. **Percent**: " percent".
5. **Abbreviation** at a word boundary (`ABBREVIATIONS`).
6. **Dot**: between digits "point", unless the digits run into a unit (`3.5GHz` -> "3.5 gigahertz");
   between letters "dot" (`google.com`); otherwise kept as punctuation.
7. **Currency symbol** followed by a number (`CURRENCY_SYMBOLS`): number first, then the unit,
   singular for `1`, `1.0` and `1.00`. Thousands separators are dropped: `$1,250.50` -> "1250.50 dollars".
8. **Kenyan shilling prefix**: `KSh`/`KES` in any case, optional `.` and space, then a number:
   `KES 150,000` -> "150000 Kenyan shillings". Alone, it reads "Kenyan shillings".
9. **Ampersand** next to whitespace: "and".
10. **At sign** next to whitespace: "at".
11. **Number sign** before a digit: `#5` -> "number 5".
12. **En dash**: between digits " to " (`3–5` -> "3 to 5"), otherwise a pause (", ").
13. **Standalone symbol** (`STANDALONE_SYMBOLS`).
14. Anything else passes through.

Runs of whitespace left by substitutions are collapsed at the end.

Examples from the tests: `192.168.1.1` -> "192 point 168 point 1 point 1", `v3.2` -> "v3 point 2",
`meet @ 3pm` -> "meet at 3 PM", `Hello.` -> "Hello.".

## Tables

### `UNIT_SUFFIXES` (104 entries)

Matched in table order and the first match wins, so a unit is listed before any unit it starts
with. A unit followed by a letter is not a match ("5mining" is not "5 minutes").

| Group | Suffixes |
|---|---|
| Compound | km/h, mi/h, KB/s, MB/s, m/s, ft/s, fl oz |
| Data | KiB, MiB, GiB, TiB, KB, MB, GB, TB, PB, EB |
| Data rate | kbps, Mbps, Gbps, bps |
| Energy | kWh, kcal, BTU ("B T U"), MJ, kJ, cal, eV, Wh, J |
| Frequency | THz, GHz, MHz, kHz, Hz |
| Power, voltage, current | GW, MW, kW, mW, W; kV, mV, V; mA, μA, A |
| Resistance | MΩ, kΩ, Ω |
| Pressure | MPa, kPa, mmHg, atm, bar, psi ("P S I"), Pa |
| Sound | dBA ("D B A"), dB |
| Duration | hrs, sec, min, ms, ns, μs, hr |
| Speed | mph |
| Area and volume | km², cm², m², ft², in², cm³, m³, ha |
| Length | km, cm, mm, nm, μm, mi, ft, yd, m |
| Mass | kg, mg, μg, lbs, lb, oz, st ("stone"), g |
| Volume | mL, dL, kL, gal, qt, pt, L |
| Currency codes | KES, kes, KSh, Ksh, ksh, KSH ("Kenyan shillings") |

### `CURRENCY_SYMBOLS` (19 entries)

| Symbol | Spoken | Symbol | Spoken |
|---|---|---|---|
| $ | dollar / dollars | ₦ | naira |
| £ | pound / pounds | ₱ | peso / pesos |
| € | euro / euros | ₺ | lira |
| ¥ | yen | ₴ | hryvnia / hryvnias |
| ₹ | rupee / rupees | ₵ | cedi / cedis |
| ₽ | ruble / rubles | ₡ | colon / colones |
| ₩ | won | ₫ | dong |
| ₪ | shekel / shekels | ₭ | kip |
| ₣ | franc / francs | ₮ | tugrik / tugriks |
| ₧ | peseta / pesetas | | |

### `ABBREVIATIONS` (44 entries; `approx.` is listed twice)

Matched case-insensitively at the start of a word (start of text, or after whitespace, `(`, `,`,
`"`, `'`, `[`).

| Group | Abbreviations |
|---|---|
| Latin | e.g. (for example), i.e. (that is), etc. (etcetera), vs. (versus) |
| Titles | Dr., Mr., Mrs., Ms., Prof., Jr., Sr. |
| Organisations | Inc., Corp., Ltd., Dept., Govt., Assn. |
| Places | St. (saint), Ave., Blvd., Ft. (fort), Mt. |
| Reference | No., Vol., Ch., Pg., Fig. |
| Measures | Approx., Max., Min., Temp., Est. (established) |
| Months | Jan. to Dec., without May |

### `STANDALONE_SYMBOLS` (36 entries)

| Group | Symbols |
|---|---|
| Math | ± (plus or minus), × (times), ÷ (divided by), ∞, ≈, ≤, ≥, ≠, √ (square root of), π, ² (squared), ³ (cubed) |
| Fractions | ½, ⅓, ⅔, ¼, ¾, ⅕, ⅖, ⅗, ⅘, ⅙, ⅚, ⅛, ⅜, ⅝, ⅞ |
| Legal | U+00A9 (copyright), U+00AE (registered), U+2122 (trademark), § (section), ¶ (paragraph) |
| Typographic | dagger and double dagger (dropped), bullet and em dash (a pause, ", ") |

## Adding an entry

1. Add it to the right `const` table in `crates/pond-voice/src/text.rs`. In `UNIT_SUFFIXES`, put
   it before any shorter unit it begins with.
2. Add a test in the same file's `tests` module.
3. Run `cargo test -p pond-voice`.

The `tests` module covers these rules (`normalize_*`), markdown (`markdown_*`), the thinking
filter (`filter_*`), the splitter, wake-word matching and Whisper artifacts.

## The browser voice path

`pond-desktop/src/modes/voice/webAudioUtils.ts` carries TypeScript versions used by
`WebVoiceBackend` before it posts each unit to `/api/v1/tts` (where Kokoro normalizes again):

- `ReplySplitter` and `splitText` port the splitter, and `holdSecs` the hold. `WebVoiceBackend`
  times each `/api/v1/tts` request sent alone, and holds the first sound by what the units still
  unanswered will take. Its thinking tone plays until that sound starts.
- `stripMarkdown` ports `strip_markdown_for_speech`.
- `normalizeForSpeech` is not a port: a short regex list covering times, a few units (kg, MB/s,
  KB/s, GB, MB, KB, ms, Hz), $, €, £, Kenyan shillings, e.g., i.e., Dr., Mr., Mrs., °C, °F, %, +, &,
  ±, √, π and three fractions.

The desktop app's own voice mode never runs these; it uses the Rust path in the voice child.
