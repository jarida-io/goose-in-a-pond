# TTS text normalization

How model output becomes text Kokoro can say: split into speech units as it streams, stripped of
markdown, and with symbols, units and abbreviations spelled out ("5km" as "5 kilometers", not
"k m"). All of it is in `crates/pond-voice/src/text.rs`.

## Where it runs

```
model tokens -> ThoughtFilter (reasoning removed) -> split_reply_units(buf, true, first)
  -> KokoroOutput::synthesize(unit)
       tokenizer::chunk: strip_markdown_for_speech -> normalize_for_speech
                         -> phonemize (espeak-ng, per run of speech, punctuation kept)
                         -> vocab ids -> chunks of at most 510 tokens
  -> play
```

| Step | Function | Callers |
|---|---|---|
| Split a streaming reply into units | `split_reply_units(text, allow_clauses, first)` | `ChatService::stream_response_inner` (voice loop), `AudioBus::speak_stream` (`crates/pond-audio-out/src/bus.rs`, spoken scheduled tasks) |
| Split a whole text into units | `split_speech_units(text, allow_clauses)`, the same with `first` false | `AudioBus::speak_text`, `KokoroOutput::speak` |
| Markdown to plain text, then normalize | `strip_markdown_for_speech` -> `normalize_for_speech` | `tokenizer::chunk` in `crates/pond-adapters-kokoro/src/tokenizer.rs`, so every Kokoro synthesis: the voice loop, `AudioBus`, `POST /api/v1/tts`, voice previews |

Normalization happens inside the Kokoro adapter, not in the callers, so every path gets it once.
If normalizing leaves nothing, the raw text is phonemized instead. The token stream shown in the
UI and stored in history is the model's text, not the normalized one.

## Splitting into speech units

`split_reply_units` returns `(units, remainder)`; the remainder waits for more tokens, and the
caller flushes it when the stream ends. With `allow_clauses = true` (every caller in the voice
path) a reply starts playing after its first clause, not its first sentence. The caller passes
`first` until a unit of the reply has been split off, which caps that unit's length: Kokoro
synthesises each unit whole at about real time, so the first sound waits on the first unit.

| Rule | Detail |
|---|---|
| Sentence ends | `.`, `?`, `!` followed by whitespace or the end of the buffer |
| Clause marks | `,` `;` `:` and em dash, only with `allow_clauses`, only once at least 3 words (`MIN_CLAUSE_WORDS`) have accumulated since the last split, and only when whitespace follows (never at the end of the buffer, where the next token may continue the number or word) |
| Numbers | `,` and `:` between digits do not split (`1,000`, `3:45`); `.` after a digit does not split when a digit follows or the buffer ends there (a decimal still streaming) |
| Abbreviations | `.` after a known abbreviation (`Dr`, `Mr`, `etc`, `e.g`, `a.m`, month names and others in `is_abbrev_suffix`) or a single letter does not split |
| Newlines | A blank line after at least 3 words splits; a single newline does not; a newline with nothing before it is dropped |
| First unit | With `first`, the unit also ends after 6 complete words (`FIRST_UNIT_MAX_WORDS`) when no mark comes first; a word is complete once whitespace follows it. Later units have no word cap |
| Force flush | Past 250 bytes (`MAX_BUF`) the buffer is cut at the last whitespace |

`split_sentences(text)` is the same with `allow_clauses = false`.

## Markdown

`strip_markdown_for_speech` works line by line, joins lines with a space, then calls
`normalize_for_speech`:

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

The `tests` module has 109 tests, among them 51 `normalize_*`, 16 `split_*`, 11 `markdown_*` and
5 `filter_*`; the rest cover wake-word matching and Whisper artifacts.

## The browser voice path

`pond-desktop/src/modes/voice/webAudioUtils.ts` carries TypeScript versions used by
`WebVoiceBackend` before it posts each unit to `/api/v1/tts` (where Kokoro normalizes again):

- `splitSpeechUnits` ports `split_speech_units` but splits clauses after 4 words, not 3, and has
  no first-unit word cap.
- `stripMarkdown` ports `strip_markdown_for_speech`.
- `normalizeForSpeech` is not a port: a short regex list covering times, a few units (kg, MB/s,
  KB/s, GB, MB, KB, ms, Hz), $, €, £, Kenyan shillings, e.g., i.e., Dr., Mr., Mrs., °C, °F, %, +, &,
  ±, √, π and three fractions.

The desktop app's own voice mode never runs these; it uses the Rust path in the voice child.
