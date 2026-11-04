# TTS text normalization

How model output becomes text Kokoro can say: split into speech units as it streams, stripped of
markdown, and with symbols, units and abbreviations spelled out ("5km" as "5 kilometers", not
"k m"). All of it is in `crates/pond-voice/src/text.rs`.

## Where it runs

```
model tokens -> ThoughtFilter (reasoning removed) -> ReplySplitter::push(text, pipeline.pace())
  -> SpeechPipeline: pack_units -> VoiceOutput::synthesize_unit(unit)
       tokenizer::chunk: strip_markdown_for_speech -> normalize_for_speech
                         -> phonemize (espeak-ng, per run of speech, punctuation kept)
                         -> vocab ids -> chunks of at most 510 tokens
       each forward pass trimmed to its speech, then the pause the unit's boundary calls for
  -> play
```

| Step | Function | Callers |
|---|---|---|
| Split a streaming reply into units | `ReplySplitter::push`, `pause` when the stream stalls or a tool starts, `finish` at its end | `ChatService::stream_response_inner` (voice loop), `AudioBus::speak_stream` (`crates/pond-audio-out/src/bus.rs`, spoken scheduled tasks) |
| Split a whole text into units | `split_text` | `AudioBus::speak_text`, `KokoroOutput::speak` |
| Synthesise and play one reply | `SpeechPipeline` (`crates/pond-core/src/models/services/voice/speech_pipeline.rs`) | the voice loop and both `AudioBus` paths |
| Markdown to plain text, then normalize | `strip_markdown_for_speech` -> `normalize_for_speech` | `tokenizer::chunk` in `crates/pond-adapters-kokoro/src/tokenizer.rs`, so every Kokoro synthesis: the voice loop, `AudioBus`, `POST /api/v1/tts`, voice previews |

Normalization happens inside the Kokoro adapter, not in the callers, so every path gets it once.
If normalizing leaves nothing, the raw text is phonemized instead. The token stream shown in the
UI and stored in history is the model's text, not the normalized one.

## Splitting into speech units

A unit is a `SpeechUnit`: its text and the `Boundary` that ends it (`Word`, `Clause`, `ListLine`,
`Sentence`, `Paragraph` or `End`). The first unit of a reply ends early, so the first sound does
not wait on a whole sentence: Kokoro synthesises a unit whole, at about real time on the nano.
Later units are whole sentences. A later sentence is cut at a clause mark only when the pipeline
is behind: when the audio queued for the speaker, less the synthesis still owed, is shorter than
the sentence so far plus 40 tokens would take (`Pace::can_wait_for`).

| Rule | Detail |
|---|---|
| Sentence ends | `.`, `?`, `!` or an ellipsis, with any run of them and closing quotes or brackets, then whitespace. A mark at the end of the buffer waits for the next token, which says whether a paragraph or list line follows; a stall of 200 ms (`SENTENCE_HOLD`) or a tool call releases it with what has arrived |
| Clause marks | `,` `;` `:` and em dash, once at least 3 words have accumulated, and only when whitespace follows. Always for the first unit; later only when behind. A mark that ends its line is left to the newline |
| Numbers | `,` and `:` between digits do not split (`1,000`, `3:45`); `.` after a digit does not split when a digit follows or the buffer ends there (a decimal still streaming). A number of up to three digits starting a line, then `.`, is a list marker, not a sentence end |
| Abbreviations | `.` after a known abbreviation (`Dr`, `Mr`, `etc`, `e.g`, `a.m`, month names and others in `is_abbrev_suffix`) or a single letter does not split |
| Newlines | A blank line ends a unit after any words (`Paragraph`). A single newline ends one when its line or the next is a list item (`-`, `*`, `+`, bullet, `1.`, `1)`), and the unit gets a comma if it has no mark (`ListLine`). Any other single newline is a wrapped line |
| First unit | Ends at its first clause mark after 3 words, or its sentence end |
| Budget | A unit is cut before it passes 200 estimated tokens (`UNIT_TOKEN_BUDGET`, about 1.1 per character): at its last clause mark, else its last space |

`pack_units` joins units waiting behind a slow synthesis into one pass, within one paragraph and the
budget, and only when the joined unit can still be ready before the queued audio runs out. Fewer,
longer passes waste less: every pass synthesises its own edge silence.

## Pauses between units

Each forward pass is trimmed to 30 ms before its first sound and 50 ms after its last (a frame
counts as sound within 40 dB of the loudest and above -50 dBFS), and is followed by the pause its
boundary calls for, divided by the speed. Kokoro puts about 0.3 s of near-silence before and 0.5 s
after every pass, so before this each boundary was about 0.8 s whatever ended it.

| Boundary | Pause | Inside one Kokoro pass, for comparison |
|---|---|---|
| `Word` (a cut inside a phrase) | 0.10 s | |
| `Clause` | 0.30 s | about 0.35 s at a comma |
| `ListLine` | 0.40 s | |
| `Sentence` | 0.50 s | 0.5-0.6 s at a full stop |
| `Paragraph` | 0.80 s | |
| `End` | none | |

`/tts` and other plain-text callers have no boundary to pass, so the text's last mark picks it
(`Boundary::from_text_end`): a phone gets 0.5 s, not 0.8 s, at a paragraph end. The DAC wake
pre-roll is played only before the first audio after idle, never between units of one reply.

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

The `tests` module has 115 tests, among them 53 `normalize_*`, 11 `markdown_*` and 6 `filter_*`,
the splitter and packing tests, and the rest on wake-word matching and Whisper artifacts.

## The browser voice path

`pond-desktop/src/modes/voice/webAudioUtils.ts` carries TypeScript versions used by
`WebVoiceBackend` before it posts each unit to `/api/v1/tts` (where Kokoro normalizes again):

- `ReplySplitter` and `splitText` port the splitter. With no pipeline to ask, a caller passes
  `behind` instead of a pace: `WebVoiceBackend` passes it while nothing is playing.
- `stripMarkdown` ports `strip_markdown_for_speech`.
- `normalizeForSpeech` is not a port: a short regex list covering times, a few units (kg, MB/s,
  KB/s, GB, MB, KB, ms, Hz), $, €, £, Kenyan shillings, e.g., i.e., Dr., Mr., Mrs., °C, °F, %, +, &,
  ±, √, π and three fractions.

The desktop app's own voice mode never runs these; it uses the Rust path in the voice child.
