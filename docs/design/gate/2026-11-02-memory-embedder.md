# Design gate: the Memory embedder surfaces

The scoring sheet for jarida-io/goose-in-a-pond#739, "offer and watch the Memory embedder in both
UIs", run as `DESIGN.md` §9 asks on 2026-11-02. Two reviewers scored it independently against the
nine rules, the constraint translation, the accessibility floor, the copy standards and the brand
rules, from 28 screenshots (light and dark, classic and hub, the hub at 800x480 and 1024x600) and
the diff.

| Reviewer | Criteria | Average | Lowest | Decision |
|---|---|---|---|---|
| A | 26 | 4.00 | six at 3 | revise (light) |
| B | 27 | 3.85 | eight at 3 | revise |
| Together | | 3.93 | none below 3 | **revise** |

The same day, after the fixes, two reviewers re-scored it at 4.15 and 4.07, together 4.11, and chose
**release**. The re-score and its plan are under "Re-score after the fixes".

## Where they agreed, and the plan

Both put the same things lowest, and the fix landed in one pull request (`fix/memory-surfaces-gate`):

| # | Finding | Fix |
|---|---|---|
| 1 | One action, four names: "Read everything again", "Clearing", "Re-indexing", "reads your memories again" | "Read memories again"; status "Reading memories again: 40 of 95 items" everywhere |
| 2 | A model that failed or is loading still read "In use", and "Using X." on the hub | The badge and the hub's line follow the embedder: Loading, Waiting to load, Downloading, Chosen, not loaded |
| 3 | "Use" while search by meaning is off promised a re-read that does not happen | It says the model is set and is used once search is on |
| 4 | "Use" started a 295 MB download | "Download and use", with the size in its accessible name |
| 5 | An out-of-memory failure said "on the Gpu: out of memory" and offered only a retry | A plain sentence naming all-MiniLM-L6-v2 (23 MB) as the lighter choice; the raw reason behind Details |
| 6 | Lineage printed `embeddinggemma-2-256 at 256 dimensions`, and its bar counted summaries as memories | The model's name; the count is items |
| 7 | The intro promised Swahili recall whatever model is in use | Removed from the intro; it stays on EmbeddingGemma 2's row |
| 8 | The hub card was "Semantic search", the setting "Search memories by meaning" | "Search by meaning" |
| 9 | Nothing said what an offline pond does | Under `network_mode = "offline"`, a download is disabled with "Needs a connection to download" |
| 10 | Two fixture artefacts made contradictions in the screenshots | The in-use row is downloaded; the health mock's corpora add up |

## Still open

- Reviewer A: a fit reading on the Memory rows, so a model that will not fit is not learned after
  choosing it.
- Both: the classic settings switch is blue, not Jarida purple (inherited from the shared switch).
- The role card on the Jobs row names a chosen model that did not load with no state of its own.
- The hub's Memory tile does not take a press to its card; the slate re-read bar and the purple
  coverage bars sit stacked on Lineage; `role="status"` speaks every 3 s while reading again;
  "GPU" and "CPU" in the measured line; the 800x480 cost line is the faintest text on the card.
- Before the first health read arrives, a chosen model briefly shows "In use".

The fixes were re-scored the same day: see "Re-score after the fixes" below.

## Reviewer A

Merged as `bf38019c` (first parent `9af5e0e3`). Scored against `DESIGN.md` §9 at tree `d6696afb`.
Scale 0-5. Evidence: the 28 screenshots taken for the pull request (named below as `shots/`), the diff, and a pixel
re-measurement of text contrast taken from the screenshots (computed with the WCAG formula).

Paths below are relative to `pond-desktop/src/`.

### 1. Scores

#### §3 The nine rules

| # | Criterion | Score | Evidence |
|---|---|---|---|
| 1 | State is elevation, not hue | 4 | "In use" is a word plus a check, no hue (`components/models/MemoryModels.tsx:116-119`; `shots/classic-models-memory-offer-light.png`). It is flat where the page's picks raise the chosen one (`docs/design-system.md` "One pick is raised"); the CSS header (`styles/memory-models.css:3-4`) argues the deviation. The classic switch is HeroUI blue (4,133,247) in `shots/classic-settings-search-by-meaning-light.png` while hub is purple; pre-existing, but this change adds a row to it. |
| 2 | Spend the offset about twice per screen | 5 | The new parts spend none: every row, status box and button is flat in all 28 shots (e.g. `shots/classic-models-memory-downloading-light.png`). |
| 3 | The edge marks content, never chrome | 5 | The Memory group wears the 2px ink as content; the status box is a 1px hairline (`styles/memory-models.css:139`); buttons are hairline pills (`styles/model-marks.css:26-40`). `docs/design-system.md` states it. |
| 4 | Press is physical | 4 | No new card, so nothing to lift. Buttons reuse `.mm-btn` (hover tint, no pressed state; `styles/model-marks.css:45-48`). Transitions and the spinner switch off under reduced motion. Mostly not applicable. |
| 5 | Let form carry data | 4 | Download bar carries 117 of 295 MB (`shots/hub-models-memory-downloading-1024x600-light.png`); re-index bar carries 40 of 95 (`shots/classic-context-lineage-reindexing-light.png`); measured numbers are set in mono with a date (`shots/classic-models-memory-offer-light.png`). Deduction: on Lineage the new slate bar and the existing purple coverage bars sit stacked, saying related things in two fills. |
| 6 | Never invent meaning the data lacks | 4 | No model, state or engine is given a hue; slate-for-measure vs purple-for-download is explained (`styles/memory-models.css:175`). Deduction: a row reads "In use" while the line above it says the model did not load (`shots/classic-models-memory-failed-light.png`). |
| 7 | Every colour carries its measurement | 4 | `styles/memory-models.css:6-13` lists ratios; my pixel re-measure agrees: failed line 6.41:1 light / 11.21:1 dark, reason 5.62 / 5.64 on `--color-bg`. Not measured: the progress fill `--color-text-tertiary` (`:187`) and the hub tile note (`hub/hub.css:712`, 12.5px muted). |
| 8 | Touch the chrome, not the data | 4 | Hub buttons measure 44px in the pixels (Use, Choose a model, Try again, Pause, Other Memory models; `shots/hub-*`). Classic Memory rows also carry 44px `.mm-btn` (`shots/classic-models-memory-offer-light.png`): inherited convention, slightly inflating the dense view. |
| 9 | Say what happened, not that something did | 3 | Strong on the Models page: "Searching by keyword until EmbeddingGemma 2 finishes downloading", size before Use, `memoryChosenText` (`sections/models/memoryEmbedder.ts:163-171`). Deductions: Lineage shows "Re-indexing: 40 of 95" above bars that read 45 of 45 and 50 of 50 (`shots/classic-context-lineage-reindexing-light.png`; bars count `indexed_rows`, `sections/context/Lineage.tsx` ~l.205-215, not `matching`); "In use" beside "did not load". |

#### §5 Designing under constraint

| # | Criterion | Score | Evidence |
|---|---|---|---|
| 10 | Network: does it work with `network_mode = offline` | 3 | Keyword fallback is named and works offline ("Memory searches by keyword until then", `memoryEmbedder.ts:169`). But `Use` on a 295 MB file has no offline state: `hooks/useModelActions.ts:69-77` just calls `activateModel` and any failure arrives as a raw `say(e.message)`. No screenshot shows offline. |
| 11 | Device: the Orin is the low-end device | 3 | Polling is bounded for the board: 3 s, 15 s after 40 looks, paused when hidden, stopped when settled (`hooks/useEmbedderHealth.ts:9-12,80-84`). But Memory rows carry no fit reading, so the recommended model's out-of-memory failure is learned after choosing (`shots/classic-models-memory-failed-light.png`: "on the Gpu: out of memory"). `fitReading` exists in `sections/models/modelsView.ts:87` and is not used here. |
| 12 | Data: say the number before they spend it | 4 | "295 MB to download" on every row before Use; "117 MB of 295 MB" with Pause and Stop; measured 18 ms GPU / 41 ms CPU and 73% vs 39%, dated (classic offer shot). No confirmation before the 295 MB starts (right beside the size, so acceptable). Hub shots omit the measured block (fixture), so not judged there. |
| 13 | Screen: 1024x600 and 800x480 | 4 | `shots/hub-memory-semantic-failed-800x480-*` wraps the failed sentence cleanly and keeps 44px targets. The hub shots are cropped to the card, so page fit, scrolling and the Jobs tiles are not shown. |

#### §6 Accessibility floor

| # | Criterion | Score | Evidence |
|---|---|---|---|
| 14 | Contrast (4.5:1 text, 3:1 UI) | 4 | Re-measured from pixels: intro 5.40 dark / 6.22 light; MEMORY label 4.80 dark; failed 6.41 / 11.21; reason 5.62 / 5.64. Switch blue on white is ~3.7:1. Deduction for the unmeasured fill and 12.5px hub note noted in #7. |
| 15 | Keyboard and visible focus | 4 | All real `<button>`s; the fold button carries `aria-expanded` / `aria-controls` (`MemoryModels.tsx:204-209`); global `button:focus-visible` ring (`styles/base.css:250-256`). No screenshot shows a focus ring on a new control (the one shaded button in `shots/hub-models-memory-downloading-800x480-light.png` is ambiguous). |
| 16 | Screen readers | 4 | `role="alert"` for failed, `status` otherwise (`MemoryModels.tsx:49`); named progressbar (`:61-67`); icons `aria-hidden`; `Use` labelled "Use X for memory"; hub toggle labelled (`hub/views/settings/Memory.tsx:446`). Deduction: `role="status"` on a line that changes every 3 s while re-indexing is chatty. |
| 17 | Motion honours `prefers-reduced-motion` | 5 | `styles/memory-models.css:207-211` and `styles/model-marks.css:409-420` (transition and `.mm-spin` off). |
| 18 | Plain language, jargon explained on first use | 3 | "Re-indexing" is never explained where it appears; Lineage still prints "Read by embeddinggemma-2-256 at 256 dimensions" (`Lineage.tsx:198`) although `embedderTitle()` now exists; the failure detail says "on the Gpu" (`shots/classic-models-memory-failed-light.png`). The intro sentence is plain and inclusive (Swahili example). |
| 19 | Legible on a dimmed 800x480 panel; important lines visible | 4 | Hub dark text is >= 4.8:1 at 800x480. The status box and progress track are 1px hairlines and faint in dark (`shots/hub-models-memory-downloading-800x480-dark.png`), but the state is carried by text and icon, not by the line. |

#### §7 Voice and copy

| # | Criterion | Score | Evidence |
|---|---|---|---|
| 20 | Active voice; a control says what happens | 4 | "Use", "Turn on", "Try again" / "Trying again", "Download" / "Starting" (`memoryEmbedder.ts:86-91,115-117`). The role card's button for Memory says "Change" / "Choose" while the status box says "Turn on" (`shots/classic-models-memory-off-light.png`). |
| 21 | An action keeps its name through the flow | 3 | Lineage: button "Read everything again" -> "Clearing" -> status "Re-indexing: 40 of 95" (`Lineage.tsx:183`, `memoryEmbedder.ts:136`); the intro says "reads your memories again", the toast "reads your memories at the next quiet moment" (`hub/views/settings/Memory.tsx:319`). One action, four names. |
| 22 | Errors say what went wrong and what to do next | 3 | The sentence and the retry are right ("did not load, so Memory is searching by keyword. Try again, or choose another Memory model", `memoryEmbedder.ts:125-129`; `Try again` button in `shots/hub-memory-semantic-failed-*`). But for out-of-memory, retrying the same model rarely helps and nothing points at the 23 MB option; the only diagnosis is a raw filename plus "Gpu" line. |
| 23 | Empty or off is an invitation | 4 | "Search by meaning is off. Memory searches by keyword." with a Turn on button in both UIs (`MemoryModels.tsx:173-183`; `shots/classic-models-memory-off-*`); role card says "Switched off". |
| 24 | No emojis in UI or code | 5 | Scanned every added line in `pond-desktop/src` and `tests`: zero emoji code points; functional icons are `lucide-react` (`MemoryModels.tsx:2`). |

#### §8 Brand and attribution; both UIs

| # | Criterion | Score | Evidence |
|---|---|---|---|
| 25 | Brand rules (logo, attribution) | 5 | No logo, brand asset or attribution touched; `docs/design-system.md` updated with the new parts. Not exercised by any screenshot. |
| 26 | Both UIs carry the change | 4 | Classic: Models (`sections/Models.tsx`), Lineage, Memory coverage panel, Settings catalogue. Hub: Models detail (`hub/views/settings/Models.tsx:316`) and Memory detail (`hub/views/settings/Memory.tsx:436-475`); one shared `MemoryModels` / `EmbedderStatus`. Hub shots cover only offer, downloading and failed: hub loading, re-indexing, off, the tile "Built-in default" note and the toggle's off state are not in the evidence. |

**Average: 104 / 26 = 4.0**

### 2. Decision: revise (light)

No criterion is below 3 and nothing is a showstopper, so this is not a hold. Six criteria sit at 3, all on copy, failure
recovery, the device and the offline case. The one failure the PR itself demonstrates, EmbeddingGemma 2 out of memory on the GPU, has a
correct sentence and a retry, but an unhelpful path from there. Ship it as merged; take the three items below into the next
change.

Evidence caveats (so they are not mistaken for defects):
- "295 MB to download" next to "In use" in the loading, failed and re-indexing shots is almost certainly the fixture's
  `downloaded: false` default (`tests/e2e/helpers/model-mocks.ts:131`), because a model that is loading or failing is on disk.
  Regenerate those shots with `downloaded: true` (the row would read "On this device, 295 MB").
- The classic settings shot has a blank band at the top and empty inputs: a capture artefact, not scored.
- The 45/45 and 50/50 bars under "40 of 95" may also be fixture-shaped (`corpora` defaults to full in `memoryHealth`), but the
  code path counts `indexed_rows`, so a real mid-switch pond can show the same pair.

### 3. The three lowest-scoring items and the fix for each

Ties at 3 (#9, #10, #11, #18, #21, #22); picked by how many households meet them.

**A. Errors and jargon on a failed load (#22, #18).**
File: `sections/models/memoryEmbedder.ts`, the `failed` case (lines ~119-129), and `components/models/MemoryModels.tsx:71`.
Detect `/out of memory|oom/i` in `embedder.reason` and return a plain sentence with a different recovery: "There is not
enough free memory to load EmbeddingGemma 2 here. Choose all-MiniLM-L6-v2 (23 MB), or close other apps and try again."
Keep the retry. Move the raw reason into a collapsed "Details" disclosure (or `title`) and strip "Gpu" and the filename from
the visible line. Add a case to `memoryEmbedder.test.ts`.

**B. One name for the action (#21), plus the Lineage sentence (#18, #9).**
Files: `sections/models/memoryEmbedder.ts:136`, `sections/context/Lineage.tsx:183,198`, `hub/views/settings/Memory.tsx:319`,
`hooks/useMemoryJob.ts` (the `turnOn` message), `components/models/MemoryModels.tsx:169-170`.
Pick "Reading again" and use it in every place: the status line ("Reading your memories again: 40 of 95"), the Lineage
button ("Read everything again" -> "Reading again" while busy instead of "Clearing"), the intro and both toasts. In
`Lineage.tsx:198` replace `{health.model_id}` and "256 dimensions" with `embedderTitle(health.model_id)` and drop the
dimensions. Make the per-corpus bars subtract `mismatched` (`Lineage.tsx` ~205-215) so they cannot read 100% under a
"40 of 95" line.

**C. Fit before choosing, and the offline case (#11, #10).**
File: `components/models/MemoryModels.tsx`, `MemoryRow` (the `mm-mem__cost` span, line ~110), using `fitReading` / the device
budget from `sections/models/modelsView.ts:87,140` for rows that have `size_mb`. Show "Fits" or "Tight on this device" in
the same word-not-hue form the other rows use, and order a non-fitting recommendation below one that fits. Where
`network_mode` is offline and the file is not on disk, disable `Use` with the reason "Needs a connection to download"
(`MemoryRow` button, line ~121) instead of letting `useModelActions.ts:69` fail with the server's raw message.

## Reviewer B

**Change:** jarida-io/goose-in-a-pond#739, "offer and watch the Memory embedder in both UIs" (merge `bf38019c`, first parent `9af5e0e3`).
**Standard:** `DESIGN.md` §9 at `d6696afb`. Every reviewed file is the same at `bf38019c` and `d6696afb`, so the line numbers hold for both.
**Evidence:** the 28 shots in `p2-ui-shots/`, the four kiosk shots the PR updated (`pond-desktop/kiosk-screenshots/*-models-*.png`), and the diff.

### Caveats on the evidence

- **Fixture artefact 1.** The loading, failed and re-indexing shots show EmbeddingGemma 2 as "In use" next to "295 MB to download". The fixture sets `downloaded: false`, but the pond reports the model as loaded or trying to load. A real pond would show "On this device, 295 MB". I did not score the size line against the change. I did score the "In use" claim, because the code produces it in every state (`MemoryModels.tsx:192`).
- **Fixture artefact 2.** `classic-context-lineage-reindexing-*.png` shows "Re-indexing: 40 of 95" above corpus bars that read 45 of 45 and 50 of 50. The fixture overrides `matching` without touching `corpora`. The server builds `matching` from the per-corpus counts (`crates/pond-infra/src/sqlite_vector_index.rs:455`), so a real pond would show the same numbers in both places. I did not score this.
- **Gaps in the hub shots.**
  - The hub shots are cropped to one card, so I can't see where that card sits on the screen.
  - There are no hub shots of the loading, re-indexing or off states.
  - The hub Memory tile appears only in the kiosk shot.
- **Gaps elsewhere.**
  - No shot shows a focus state.
  - No shot shows a pond with `network_mode = offline`.
  - The top 380 px of `classic-settings-search-by-meaning-*.png` is blank. I can't tell whether that is the page or the capture.

### 1. Scores

| # | Criterion | Score | Evidence |
|---|---|---|---|
| **§3.1** | State is elevation, not hue | 4 | The model in use is marked by the words "In use" and a check, never by a fill. The failed state uses the warning colour only together with an icon and a sentence (`classic-models-memory-failed-light.png`; `memory-models.css:2-3`). |
| **§3.2** | Spend the offset about twice per screen | 5 | The change adds no offset. The rows lie flat on a hairline, so the page's existing offsets keep their meaning (`memory-models.css:46-56`; `classic-models-memory-offer-light.png`). |
| **§3.3** | The edge marks content, never chrome | 4 | The Memory group wears the 2px edge as content, and the status box stays on a hairline. In classic, though, the failed state stacks three frames: group, then box, then button (`classic-models-memory-failed-light.png`). |
| **§3.4** | Press is physical | 4 | Not violated, but not used either. Nothing new is raised. "Turn on" in the off state and "Use" on the recommended model are the screen's one question, yet both are plain `mm-btn` rather than the existing `mm-btn--ask` press (`MemoryModels.tsx:121`, `:179`). |
| **§3.5** | Let form carry data | 4 | The re-index bar is the fraction done and the download bar is bytes; the hub folds the list to the rows that matter. On Lineage, though, the re-index bar is slate while the coverage bars under it are purple: two encodings for one measure (`classic-context-lineage-reindexing-light.png`). |
| **§3.6** | Never invent meaning the data lacks | 3 | The intro says an English question finds a Swahili fact whatever model is in use. The same card measures the default at 39% (`MemoryModels.tsx:168-170`; `classic-models-memory-offer-light.png`). The progress bar is labelled "Memories re-indexed" but counts summaries too: 95 = 45 memories + 50 summaries (`MemoryModels.tsx:63`). |
| **§3.7** | Every colour carries its measurement | 4 | The `memory-models.css:1-13` header measures every text token in light and dark. The re-index fill (`--color-text-tertiary`, `memory-models.css:188`) is an indicator with no ratio recorded, and so is hub.css's new `--mut` note. |
| **§3.8** | Touch the chrome, not the data | 4 | Every new button is `mm-btn` at `--tap-min`, 44px. The fold button is 44px on the hub (`hub-models-memory-offer-800x480-light.png`). Classic uses the same full-size pills as the rest of its Models page, with no compact variant. |
| **§3.9** | Say what happened, not that something did | 3 | Every status line names the keyword fallback, which is excellent. But three claims contradict it: the row says "In use" (`MemoryModels.tsx:192`, `classic-models-memory-failed-light.png`), the hub says "Using EmbeddingGemma 2." (`hub/views/settings/Memory.tsx:454`, `hub-memory-semantic-failed-800x480-light.png`), and pressing Use while search is off reports "Now using … reads your memories again" (`useModelActions.ts:75`, `memoryEmbedder.ts:171`). Search by meaning stays off. |
| **§5 Network** | Works with `network_mode = offline` | 3 | Keyword search keeps Memory working with no network, and every state says so. Nothing in the change reads `network_mode`, though. "295 MB to download" and "fetched the first time it is used" never say what happens on an offline pond. There is no shot of one. |
| **§5 Device** | The Orin is the low-end device | 4 | The figures were measured on the Orin and the shot gives the date: "Measured on this kind of device, 31 Oct 2026" (`classic-models-memory-offer-light.png`). Polling backs off and pauses while hidden (`useEmbedderHealth.ts:9-12`, `:84`). But the rows give no RAM cost or fit (`ram_estimate_mb` 368 for EmbeddingGemma 2), even though the chat rows do and the failure shown is an out-of-memory error. |
| **§5 Data** | Say the number before they spend it | 4 | Every row gives its size before any press: "295 MB to download", "23 MB, fetched the first time it is used". A download shows bytes, with Pause and Stop. But the button that spends 295 MB says only "Use" (`MemoryModels.tsx:121-122`), where PickCard says "Download and use" (`PickCard.tsx:27`). |
| **§5 Screen** | 1024x600 and 800x480, at arm's length | 3 | At 800x480 the hub shows only the recommendation and the model in use (`hub-models-memory-offer-800x480-light.png`). But Memory is the last card on the hub Models screen, after the tall pick cards (`hub/views/settings/Models.tsx:315`). Its tile above is not a control, and "Choose a model" lands at the top of the screen (`Memory.tsx:458`). |
| **§6 Contrast** | WCAG 2.1 AA | 4 | Text is measured in light and dark (`memory-models.css:6-12`); the failed line is 6.41:1 and every dark shot is legible. The fill is unmeasured; I estimate it at about 4.8:1 in dark, which passes. |
| **§6 Keyboard** | Reachable and operable, visible focus | 4 | Every control is a native button covered by the global focus-visible rule (`styles/base.css:250`). The fold has `aria-expanded` and `aria-controls` (`MemoryModels.tsx:207`). No shot shows focus, so I couldn't confirm it visually. |
| **§6 Screen readers** | Semantic, labelled | 4 | The failed state is `role=alert` and the others `role=status`; the progress bar is labelled, and buttons are named "Use X for memory". The hub toggles gained labels. Two problems: the status region rewrites its count on every 3 s poll, so a screen reader hears constant updates, and the bar's name says memories while counting summaries. |
| **§6 Motion** | Honour reduced motion | 5 | The fill transition and the spinner both stop under `prefers-reduced-motion` (`memory-models.css:207-211`; `model-marks.css:422`). |
| **§6 Language** | Plain, jargon explained, acronyms spelled out | 3 | Replacing the GGUF / FastEmbed radio with "Search memories by meaning" is a real gain. Jargon remains: "Re-indexing"; the hub card title "Semantic search" (`Memory.tsx:437`); GPU and CPU never spelled out. Lineage also shows raw "embeddinggemma-2-256 at 256 dimensions" (`Lineage.tsx:198`) beside the new status line, and the failed state shows raw "on the Gpu". |
| **§6 Dim 800x480 panel** | Legible dimmed, at a distance | 3 | Dimming can't be judged from these shots. At 800x480, the size line that §5 Data relies on is the smallest, faintest text on the card (~12px slate mono). The intro takes three lines of slate (`hub-models-memory-offer-800x480-light.png`). |
| **§7 Tone** | Household's side, name what people control | 4 | "Search by meaning", "at a quiet moment" and "Built-in default" name the job rather than the engine. The intro overclaims, as scored under §3.6. |
| **§7 Controls and names** | A control says what happens; an action keeps its name | 3 | "Use" starts a 295 MB download. One process has three names: "reads your memories again" (intro), "Re-indexing" (status) and "Read everything again" (Lineage button). The setting is "Search memories by meaning" in classic but "Search by meaning" under a "Semantic search" title on the hub. Off is "the pond searches by keyword" (`catalogue.ts:268`) in one place and "Memory searches by keyword" in another. |
| **§7 Errors** | What went wrong, what to do next | 4 | "…did not load, so Memory is searching by keyword. Try again, or choose another Memory model." The pond's reason sits under it, and the can't-run case drops the retry (`memoryEmbedder.ts:119-129`). The cause, out of memory, appears only in raw engine text, and "Try again" is offered for an out-of-memory failure that retrying won't fix. |
| **§7 Off and empty states** | An invitation to act | 4 | Off is one sentence and a "Turn on" button (`classic-models-memory-off-light.png`). But the rows below still offer "Use", which does not turn search on, and the intro above still says Memory finds things by meaning. |
| **§7 No emojis** | lucide icons only | 5 | Only lucide icons (SearchX, Clock, Loader2, AlertTriangle, RefreshCw, RotateCw, Check, Chevron*). The added lines contain no non-ASCII characters apart from box-drawing rules in CSS comments. |
| **§8 Palette and type** | Jarida material | 3 | Quicksand for body, JetBrains Mono for sizes and measurements, purple only on the download bar, toggle and chip. But the classic switch for this setting renders iOS blue in both themes, where `settings-catalogue.css:608` expects brand purple (`classic-settings-search-by-meaning-light.png`). The blue is inherited, but this change's control is the one wearing it. A whole sentence is also set in mono (the measured summary). |
| **§8 Logo and attribution** | Logo rules, credit | 5 | The change doesn't touch any logo or attribution. Nothing to judge, so scored as not an issue. |
| **Both UIs** | Classic and hub both carry it | 4 | It ships in classic Models, Settings, Lineage and Memory coverage, and in the hub's Models card, tile note and Memory settings. The hub has no Lineage view, so nothing there carries the re-indexing line. |

**Average:** 104 / 27 = **3.85**, or 100 / 26 = 3.85 without the both-UIs row. No criterion scores below 3.

### 2. Decision: **revise**

This is not a hold. Nothing is below 3, and keyword search keeps every household's Memory working in every state.

It is not a clean release either. The change's own centre is "say which thing is running", and the screen contradicts itself on exactly that: "did not load" sits beside "In use", and "Using" appears while search is off. Fix the three items below as a follow-up before the P2 Memory surfaces are called done.

### 3. The three lowest items, with fixes

Eight criteria tie at 3. I broke the tie by what a household would actually meet.

**1. §3.9 Say what happened (3).** The row badge and the hub's Model line claim a state the status line denies.
- In `pond-desktop/src/components/models/MemoryModels.tsx`, pass `line.kind` into `MemoryRow`. Then:
  - Show "In use" only when the kind is `ready` or `reindexing`.
  - For `failed` or `missing`, show "Chosen, not loaded", using `mm-fit--big` and AlertTriangle.
  - For `loading` or `waiting`, show "Loading".
- In `pond-desktop/src/hub/views/settings/Memory.tsx:452-456`, build the Model sub-line the same way, for example "Chosen: EmbeddingGemma 2, not loaded".
- In `pond-desktop/src/hooks/useMemoryJob.ts:40-46`, when `off` is true, turn search on in the same press before `useFor`. Alternatively, disable the rows' Use while off and point them at "Turn on".
- Give `memoryChosenText` (`memoryEmbedder.ts:163`) an `off` argument, so it never says "reads your memories again" while nothing will.

**2. §7 Controls and names (3), which also caps §5 Data at 4.**
- In `MemoryModels.tsx:121-122`, when `m.acquire === "download" && !m.downloaded`, label the button "Download and use" with the lucide `Download` icon, following PickCard's convention. Put the size in its aria-label: "Download and use EmbeddingGemma 2 for memory, 295 MB".
- Give the re-read one name everywhere. Change `memoryEmbedder.ts:136` to "Reading memories again: 40 of 95", which matches the intro and Lineage's "Read everything again".
- In `hub/views/settings/Memory.tsx:437`, retitle the card "Search by meaning".
- In `settings/catalogue.ts:268`, change "Off, the pond searches by keyword" to "Off, Memory searches by keyword", so both UIs say one sentence.

**3. §3.6 Never invent meaning (3).**
- In `MemoryModels.tsx:168-170`, drop the cross-language promise from the intro: "Memory finds what the pond knows by meaning. A new model reads your memories again once, at a quiet moment." The Swahili claim already lives where it is true, in EmbeddingGemma 2's own reason and measured line.
- At `MemoryModels.tsx:63`, rename the progress bar to "Items read again" (memories and summaries). Make `memoryEmbedder.ts:136` say "items", or count only the memory corpus if the label stays "memories".

**Also at 3, for the plan after these:**
- **§5 Screen.** Make the hub Memory tile a button that scrolls to the Memory card (`hub/views/settings/Models.tsx:209`), or move the card up under the tiles, and land "Choose a model" there.
- **§5 Network.** Say "Needs the internet once (295 MB)" or "Not available offline" on rows that fetch, when `network_mode = offline`.
- **§6 Language.** Spell out GPU and CPU in the measured line, and use `embedderTitle` in Lineage's "Read by" line (`Lineage.tsx:198`).
- **§6 Dim panel.** Set the hub cost line at `--text-base` in `--color-text`.
- **§8 Palette.** Map HeroUI's switch accent to `--color-accent` in `settings-catalogue.css`.

## Re-score after the fixes

The ten fixes above merged in jarida-io/goose-in-a-pond#750. This re-runs `DESIGN.md` §9 on the same
surfaces in both UIs: the classic Models page's Jobs band and Memory rows, classic Settings ("Search
memories by meaning"), classic Context's Lineage, the hub's Models screen (job tiles and Memory card) and
the hub's Memory screen ("Search by meaning"). The old classic `sections/Memory.tsx` page is no longer
mounted, so its coverage panel was not scored.

Two reviewers scored independently against one list of 27 criteria, each having read the first round's
sheets and checked every "Still open" item. They scored the branch of this change at `ec50127b`: main
`ffe2ef23` plus six fixes for the open items. Then each checked the three commits their findings led to,
at `9780a768`, in an addendum. One last commit, `19216df0`, fixes a regression Reviewer A found in
`ac35e1e0` and the silent placeholder both reviewers named. Tests checked it; it was not re-scored.

Evidence: 124 screenshots of the branch and 114 of main, taken with Playwright against mocks. They cover
light and dark, classic at 1280x900 and the hub at 800x480 and 1024x600, in every Memory state: offer,
downloading, loading, failed, reading again, ready, off, offline, and a slow first health read. Keyboard
focus is shot too. They also include a replay of the live pond's own JSON, read from the Orin Nano on main
`ffe2ef23` through a test device that was revoked afterwards: EmbeddingGemma 2 ready, 86 of 86 items,
network open. As in the first round, the shots are kept outside the repository; the sheets name them by
file.

| Reviewer | Criteria | At `ec50127b` | At `9780a768` | Lowest | Decision |
|---|---|---|---|---|---|
| A | 27 | 4.11 | 4.15 | two at 3 | release |
| B | 27 | 4.04 | 4.07 | two at 3 | release |
| Together | | 4.07 | 4.11 | none below 3 | **release** |

**Decision: release**, for the tree this change merges. Nothing scores below 3, nothing is left at the
accessibility floor, and the first round's centre, saying which model is really running, now holds in
every state in both UIs. Reviewer B first made the release conditional on the off-switch contrast, then
withdrew the condition once `f7543ab8` fixed it.

### The scores side by side, at `9780a768`

| # | Criterion | A | B |
|---|---|---|---|
| 1 | §3.1 State is elevation, not hue | 4 | 4 |
| 2 | §3.2 Spend the offset about twice | 5 | 5 |
| 3 | §3.3 The edge marks content, never chrome | 5 | 4 |
| 4 | §3.4 Press is physical | 4 | 4 |
| 5 | §3.5 Let form carry data | 4 | 4 |
| 6 | §3.6 Never invent meaning | 4 | 4 |
| 7 | §3.7 Every colour carries its measurement | 4 | 4 |
| 8 | §3.8 Touch the chrome, not the data | 4 | 4 |
| 9 | §3.9 Say what happened | 4 | 4 |
| 10 | §5 Network: works offline | 4 | 4 |
| 11 | §5 Device: the Orin is the low-end device | 3 | 3 |
| 12 | §5 Data: say the number first | 4 | 4 |
| 13 | §5 Screen: 1024x600 and 800x480 | 4 | 4 |
| 14 | §6 Contrast | 4 (was 3) | 4 (was 3) |
| 15 | §6 Keyboard and visible focus | 4 | 4 |
| 16 | §6 Screen readers | 4 | 4 |
| 17 | §6 Reduced motion | 5 | 5 |
| 18 | §6 Plain language | 3 | 3 |
| 19 | §6 Dim 800x480 panel | 4 | 4 |
| 20 | §7 Tone | 4 | 4 |
| 21 | §7 A control says what happens; an action keeps its name | 4 | 4 |
| 22 | §7 Errors | 4 | 4 |
| 23 | §7 Off and empty states | 4 | 4 |
| 24 | §7 No emojis | 5 | 5 |
| 25 | §8 Palette and type | 4 | 4 |
| 26 | §8 Logo and attribution | 5 | 5 |
| 27 | Both UIs carry it | 5 | 4 |
| | **Sum, average** | **112, 4.15** | **110, 4.07** |

### Where they disagreed

Only on two criteria, and by one point each:

- **§3.3, the edge.** A gave 5: the tiles that became buttons kept their hairline, and the cards keep the
  2px edge. B gave 4: in classic's failed state the 2px group holds a 1px status box, which holds 1px
  pills, so three frames stack.
- **Both UIs.** A gave 5: every fix landed in both UIs wherever the surface exists. B gave 4: the classic
  Jobs card's Change lands on its list, but the hub Memory screen's "Choose a model" still opens Models at
  the top. A names the same gap under §5 Screen, so the two disagree on where it counts, not on whether it
  is there.

### The first round's "Still open" list, now

| Item | A | B | Change |
|---|---|---|---|
| A fit reading on the Memory rows | Open | Open | Left open: see the plan below |
| The classic settings switch is blue | Fixed | Fixed | `04f525be`; its off state, `f7543ab8` |
| The Jobs role card names a model that did not load with no state | Fixed | Fixed | `67f468a2` |
| The hub's Memory tile does not take a press to its card | Fixed | Fixed | `26a2f877`. The hub Memory screen's "Choose a model" still lands at the top of Models |
| The slate re-read bar and purple coverage bars stack on Lineage | Open | Open | The numbers now agree, 40 of 95 = 40 of 45 + 0 of 50 |
| `role="status"` speaks every 3 s while reading again | Fixed | Fixed | `71f557d5`. Checked in the markup and the tests, not heard with a screen reader |
| "GPU" and "CPU" in the measured line | Open | Open | The sentence comes from the pond, `crates/pond-core/src/models/domain/recommended.rs:109` |
| The 800x480 cost line is the faintest text | Fixed | Fixed | `ec50127b` |
| "In use" before the first health read | Fixed | Fixed | `930c685b`. A first read that errors, rather than one that is slow, still ends on "In use" |

The fit reading was left open on purpose. The live nano reports `available_for_llm_mb` 309 while
EmbeddingGemma 2, estimated at 368 MB, runs ready beside the conversation model. That budget is the room
for a conversation model, so a fit reading built on it would call a running model too big. The pond first
has to report the room an embedder has, measured on the Orin.

### Found and fixed during the re-score

| Finding | Found by | Change |
|---|---|---|
| The off switch, classic and hub, measured 1.19:1 and 1.45:1, under the 3:1 a control needs | A and B | `f7543ab8`: a slate track, 6.22:1 light and 4.80:1 dark |
| The hub Memory screen's switch read "on" before settings arrived, and stayed on if the memory list failed | A and B | `ac35e1e0` |
| The hub toggle's knob sat mid-track when off and past the end when on, so only the fill said which | Seen in the retaken shots; both reviewers then confirmed they had missed it | `9780a768` |
| After `ac35e1e0` the switch did not follow a refresh, "Compact memory nightly" never followed the settings, and a failed read left a silent placeholder | A, and B for the placeholder | `19216df0`: "Could not read this setting" in its place |

### The plan from here

Both reviewers put the same two criteria lowest, at 3:

1. **§5 Device.** Show what a Memory model needs while it runs (`ram_estimate_mb`, already in the
   catalogue) on each row, and a fit reading once the pond reports the room an embedder has. After an
   out-of-memory failure, keep all-MiniLM-L6-v2 in front of the hub's fold, so the way out the error names
   is one press away. In the pond, weigh the Memory recommendation against free memory, so "Recommended"
   is not shown on a model this pond cannot load.
2. **§6 Plain language.** Spell out GPU and CPU and name the default in `recommended.rs:109`. Say "This
   pond does not have room to load EmbeddingGemma 2 beside the conversation model": "not enough free
   memory" uses "memory" in two senses on a card titled Memory. On Lineage, replace "not indexed" with
   "not read yet".

Also named, at 4:

- The hub Memory screen's confirmation flash, `#16a34a` on `#f0fdf4`, is 3.15:1 at 13px, with no dark variant.
- A first health read that errors ends on "In use".
- The hub Memory screen's "Choose a model" should land on the Memory card.
- Lineage's "Read memories again" clears every vector without asking, and it can be pressed while a re-read runs.
- After Use while search is off, nothing marks the set model. Turn on can start a 295 MB download without naming its size.
- In light, the off and on switch fills share a lightness (1.09:1). A hollow off would read as empty at a glance.
- The measured result is a whole sentence in mono, and the Lineage bars still stack.
- `DESIGN.md` §2 gives `--color-bg` as `#FFF9F9` and the accent as `#8C4BFF`. The shipped tokens are `#FAF3E1` and
  the household's accent, Purple `#7C3AED`. Under §1 the shipped values win and the document owes the
  correction, with its ratios re-measured.

## Re-score: Reviewer A

Tree scored: branch `docs/memory-embedder-gate-rescore` at `ec50127b` (worktree
`.claude/worktrees/memory-gate-rescore`), six commits on main `ffe2ef23`. Standard: `DESIGN.md` section 9
in that tree. Evidence: the diff `ffe2ef23..HEAD -- pond-desktop`, the code it touches and the code those
surfaces call, 58 of the `after/` shots (every one the brief names, light and dark, plus the downloading,
loading, card and 1024x600 shots), 4 `before/` shots for comparison, the live pond's JSON in `../live/`,
pixel samples taken from the shots and WCAG contrast computed with the formula from token hex values.

Shots are named by file name (all in `shots/after/` unless marked `before/`). Code paths are relative to
`pond-desktop/src/` unless they start with `crates/`.

### Caveats on the evidence

- I ran nothing that builds or tests. The brief says `memory-embedder.spec.ts` passes; `e2e-full.log` in the
  gate folder stops at test 115 of 289 and never reaches that file, so I take the pass on trust. `unit.log`
  (2877 passed), `lint.log` and `tsc.log` all end at exit 0.
- Screen-reader behaviour is judged from the markup and the unit tests, not heard with VoiceOver or NVDA.
- Only the classic page has a focus shot. The hub tiles' focus ring and the focused Memory card are not shown.
- No shot is of a dimmed panel; the dim criterion is judged from sizes and measured ratios.
- The offline state has all-MiniLM-L6-v2 already in use, so no shot shows a fetch-on-first-use model
  (all-MiniLM, BGE Small) offered on an offline pond. Judged from code.
- The `offer` fixture carries no measured block; the `live-*` shots do.
- Not scored as artefacts: the hub Memory screen's sample memories and its "Could not reach the server" banner
  (the mocks serve no memories); the "Failed to fetch" line under the Context tabs on Lineage (present in
  `before/` too); a hover tint on "Other Memory models" in some hub shots (the pointer left over from the nav
  click). The first artefact has a side effect I note under 3.6: because the switch's state is read in the same
  `Promise.all` as the memory list (`hub/views/settings/Memory.tsx:248-265`), the off-state shot
  `hub-memory-search-by-meaning-off-800x480-light.png` shows the switch on beside "Search by meaning is off".
- `DESIGN.md` section 2 gives `--color-bg` as `#FFF9F9` and the accent as `#8C4BFF`. The shipped token is
  `#FAF3E1` (`styles/design-tokens.css:90`) and the shots' accent is the household's `#7C3AED`. The shipped
  values win under section 1, so I measured against them. `DESIGN.md` owes that correction.

### Scores

| # | Criterion | Score | Evidence |
|---|---|---|---|
| 3.1 | State is elevation, not hue | 4 | Every state is a word with an icon: "In use", "Checking", "Loading", "Downloading", "Chosen, not loaded", one `HolderBadge` (`components/models/MemoryModels.tsx:40-48`) on the row, the classic Jobs card and the hub tile (`classic-models-jobs-failed-light.png`, `hub-models-top-failed-800x480-light.png`). The warning colour appears only on a fault and always with the triangle and words. The chosen Memory row still lies flat where the rule says a chosen thing rises; the deviation is argued in `styles/memory-models.css:3-4`. |
| 3.2 | Spend the offset about twice | 5 | The six commits add no offset. The tiles that became buttons stay on a hairline (`hub/hub.css:710-714`); the new badges and the Checking state are flat in every shot. |
| 3.3 | The edge marks content, never chrome | 5 | The tiles are navigation and keep the 1px line (`hub/hub.css:710`, comment says why); the Jobs cards and the Memory group keep the 2px edge as content; the status box stays a hairline (`styles/memory-models.css:150`). |
| 3.4 | Press is physical | 4 | Nothing new is raised, so nothing is pressed wrong. But the screen's one question in the off state ("Turn on") and on the recommendation ("Download and use") is a plain `mm-btn`, not the existing `mm-btn--ask` (`components/models/MemoryModels.tsx:179-189,257`). The new tile buttons have a hover tint only (`hub/hub.css:712`), so on the touch panel a press shows nothing until the scroll starts. |
| 3.5 | Let form carry data | 4 | Download bar is bytes, 117 of 295 MB (`classic-models-jobs-downloading-light.png`); re-read bar is the fraction; the numbers on Lineage now add up, 40 of 95 above Memory 40 of 45 and Summary 0 of 50 (`classic-context-lineage-reindexing-light.png`). The slate re-read bar still sits on top of purple coverage bars measuring the same thing (`styles/memory-models.css:196-200`, `styles/context.css:371`). |
| 3.6 | Never invent meaning | 4 | The row no longer says "In use" before the pond answers (`classic-models-jobs-first-read-light.png` reads "Checking"; `sections/models/memoryEmbedder.ts:220`). Two leftovers: the hub Memory screen's switch starts on before settings are read (`hub/views/settings/Memory.tsx:233`) and stays on if the memory list fails (artefact path above); and "Recommended" stays on a model the same screen says does not fit (`classic-models-jobs-failed-light.png`), because the pond's recommendation is not weighed against free memory. |
| 3.7 | Every colour carries its measurement | 4 | New colours carry their ratios in the comment: the switch fill (`styles/settings-catalogue.css:606-607`, `hub/hub.css:686-688`) and the hub cost line (`hub/hub.css:723-725`). I re-measured them: switch 5.70:1 light and 3.90:1 dark from the pixels (`#7C3AED`, `#9058F0`), knob 4.34:1, cost line 18.06:1 / 15.12:1. Still unmeasured in a comment: the re-read fill (`styles/memory-models.css:186-200`; I compute 6.22:1 light, 4.80:1 dark on its track) and the dark hover mix (`styles/settings-catalogue.css:616`, 4.69:1). |
| 3.8 | Touch the chrome, not the data | 4 | Each hub tile is a whole-tile button the e2e test holds at 44px or more (`tests/e2e/memory-embedder.spec.ts`, "the Memory tile takes a press", line 393), and every button on the hub Memory card in the offer and failed states (lines 353-354, 370-371). Classic Memory rows keep full 44px pills rather than a compact look with a hit extension, as in the first round (`classic-models-jobs-offer-light.png`). |
| 3.9 | Say what happened | 4 | The Jobs card and tile now say what the row says (`classic-models-jobs-failed-*`, `classic-models-jobs-downloading-light.png` "Downloading", `hub-models-top-first-read-800x480-*` "Checking"); offline rests with its reason. Leftovers: a first health read that fails flips "Checking" to "In use" (`hooks/useEmbedderHealth.ts:68`, `finally`), so a pond that could not be asked is described as running; a failed Turn on and a failed hub switch save show raw exception text (`hooks/useMemoryJob.ts:62`; `hub/views/settings/Memory.tsx:326`, "Settings update failed: Error: ..."). |
| 5.N | Network: works with `network_mode = offline` | 4 | Keyword search keeps Memory working in every state, and a press that would download rests as "Needs a connection to download" in both UIs (`classic-models-jobs-offline-*`, `hub-models-memory-offline-800x480-*`; `sections/models/memoryEmbedder.ts:268-278`). Not covered: models the pond fetches on first use (`acquire: "external"`, all-MiniLM 23 MB, BGE Small 33 MB) keep an enabled Use offline, because only `acquire === "download"` is blocked (`memoryEmbedder.ts:272`); and the out-of-memory sentence sends people to all-MiniLM (`memoryEmbedder.ts:141-143`), which the live catalogue lists as `downloaded: false` (`live/models.json`). |
| 5.D | Device: the Orin is the low-end device | 3 | Polling stays bounded (3 s, 15 s after 40 looks, paused while hidden; `hooks/useEmbedderHealth.ts:9-12`). Still no fit or memory figure on a Memory row (`components/models/MemoryModels.tsx:149-171`), although the live pond sends `ram_estimate_mb` for every Memory model (EmbeddingGemma 2 368 MB, all-MiniLM 50 MB) and `memory-status` reports 309 MB free beside the language model. `fitReading` skips embeddings on purpose (`sections/models/modelsView.ts:95-96`). An out-of-memory load is still learned after choosing (`classic-models-jobs-failed-light.png`), and on the hub the lighter model the error names is folded away (`hub-models-memory-failed-800x480-light.png`). |
| 5.Da | Data: say the number before they spend it | 4 | Every row gives its size before any press; "Download and use" carries the size in its name (`components/models/MemoryModels.tsx:185`); a download shows 117 MB of 295 MB with Pause and Stop. Leftovers: "Turn on" in the off state can start the chosen model's download without naming its size (`MemoryModels.tsx:256-259`; the size is said only when the model was chosen, `memoryEmbedder.ts:200-203`); fetch-on-first-use rows say "Use" for a 23 or 33 MB fetch (the size is on the line). |
| 5.S | Screen: 1024x600 and 800x480 | 4 | The tiles now take a press to the card and move focus there (`hub-models-top-failed-800x480-light.png` chevrons; `hub/views/settings/Models.tsx:105-118,251-264`); the cost line reads at body size (`hub-models-memory-offer-800x480-light.png`); nothing overflows sideways at 800x480 in any shot. Leftovers: the hub Memory screen's "Choose a model" still lands at the top of Models (`hub/views/settings/Memory.tsx:455`, `go("models")`), and the failure's named way out sits behind "Other Memory models (2)". |
| 6.C | Contrast | 3 | The change's own words pass, computed from tokens: text 18.06 / 15.12; slate 6.22 / 5.45 on the card and 5.62 / 5.64 on the status box; warning 7.09 / 10.15 and 6.41 / 11.21; focus ring 5.70:1 light and 4.50:1 dark from the pixels (`classic-models-memory-focus-*`). Three parts of these surfaces fail, all inherited: the classic "Search memories by meaning" switch when off, track 1.19:1 light and 1.14:1 dark on the card (pixels, `classic-settings-search-by-meaning-row-off-*`), so off reads as no switch; the hub toggle when off, `#D8D5E0` at 1.45:1 on white (`hub/hub.css:685`); the hub Memory screen's confirmation after the switch, `#16a34a` on `#f0fdf4` at 3.15:1 and 13px, with no dark variant (`hub/views/settings/Memory.tsx:381-390`). |
| 6.K | Keyboard and visible focus | 4 | A 2px ring shows on Download and use in both themes (`classic-models-memory-focus-*`). Hub tiles are buttons; the card they open takes `tabIndex={-1}` and focus (`hub/views/settings/controls.tsx:107`, `Models.tsx:118`) and gets the `[tabindex]:focus-visible` ring (`styles/base.css:250-255`). No shot shows hub focus. The classic Jobs card's Change scrolls to the Memory list but leaves focus on the button (`sections/Models.tsx:86-92,269-275`), where the hub now moves it. |
| 6.R | Screen readers | 4 | The live region holds a sentence that stays put and the count moves to the bar's `aria-valuetext` (`components/models/MemoryModels.tsx:87-104`, `sections/models/memoryEmbedder.ts:161-163`, `sections/context/Lineage.test.tsx`). The tile's name carries its state ("Memory: EmbeddingGemma 2, Chosen, not loaded. Change"; `hub/views/settings/Models.tsx:256-260`); the hub toggle has `aria-pressed`. Short of 5 because none of it was heard with a screen reader, and whether a valuetext change inside a live region stays silent varies by reader. |
| 6.M | Reduced motion | 5 | The new tile scroll is instant under reduced motion (`hub/views/settings/Models.tsx:116-117`), as the classic jump is (`sections/Models.tsx:90-91`); the global rule stops transitions and spinners (`styles/touch.css:336-344`, `styles/model-marks.css:416,429-431`). |
| 6.L | Plain language | 3 | Still on the live pond: "18 ms on the GPU or 41 ms on the CPU", acronyms never spelled out, and "against 39% for the default" without naming the default (`live-classic-models-jobs-light.png`, `live-hub-models-memory-800x480-light.png`; the text is `crates/pond-core/src/models/domain/recommended.rs:109`). Lineage: "Anything not indexed is still found by word" (`sections/context/Lineage.tsx:201`). "Checking" does not say what is being checked. Everything else is plain. |
| 6.D | Dim 800x480 panel | 4 | The cost line is now 13px in the text colour (`hub/hub.css:725`; `hub-models-memory-offer-800x480-*`). The faintest lines now are the 12px slate measured caption and "Needs a connection to download" (6.22 / 5.45) and the measured sentence in 12px mono (`live-hub-models-memory-800x480-*`). Status box and track are hairlines at 1.36:1 light and 1.65:1 dark; the state is in the words. The light off switch would vanish on a dim panel (6.C). |
| 7.T | Tone | 4 | The household's side, no apology: "There is not enough free memory to load EmbeddingGemma 2, so Memory is searching by keyword." The hub Memory screen's subtitle calls the pond "Goose" ("things Goose remembers about you", `hub/views/settings/Memory.tsx:349`) where every other line says the pond. |
| 7.A | A control says what happens; an action keeps its name | 4 | "Download and use", "Read memories again" and "Reading memories again" hold everywhere; the tiles' names end "Change". Off state: the Jobs card says "Switched off" with "Choose", and its hint "Press Use on the model you want for memory" leads to rows whose Use does not turn search on, while the box beside says "Turn on" (`classic-models-jobs-off-light.png`; `sections/Models.tsx:269-275`). The classic row is "Search memories by meaning" under a "Search by meaning" header, the hub row "Search by meaning" (`classic-settings-search-by-meaning-offer-light.png`). |
| 7.E | Errors | 4 | Out of memory says what happened, what to do, and keeps the pond's words behind Details, in both UIs (`classic-models-jobs-failed-*`, `hub-memory-search-by-meaning-failed-800x480-*`). The way out it names has no button of its own and, on the hub, is folded; raw exception text on a failed Turn on and a failed hub switch save (3.9). |
| 7.O | Off and empty states | 4 | "Search by meaning is off. Memory searches by keyword." with Turn on, in both UIs, and the intro changes to "Choose the model Memory will use" (`classic-models-jobs-off-*`, `hub-models-memory-off-800x480-dark.png`). The Jobs card's button for an off Memory still says "Choose", not "Turn on". |
| 7.N | No emojis | 5 | No non-ASCII character in any added line of the diff; the tile's chevron is lucide `ChevronRight`. |
| 8.P | Palette and type | 4 | The classic switch now wears the household's accent, lifted in dark (`styles/settings-catalogue.css:606-617`; `classic-settings-search-by-meaning-offer-*`). Leftovers: the measured result is a whole sentence in mono (`styles/memory-models.css:100-105`; live shots); the hub Memory screen that holds the "Search by meaning" card wears a teal `#0D9488` header and Add button and hard-coded green and red flashes (`hub/views/settings/Memory.tsx:350,370,388`), inherited and off-palette. |
| 8.L | Logo and attribution | 5 | Nothing touched. |
| B | Both UIs carry it | 5 | Every fix lands in both where the surface exists: the state badge on the classic Jobs card and the hub tile (`sections/Models.tsx:331,344`; `hub/views/settings/Models.tsx:240,262-264`), Checking in both (`classic-models-jobs-first-read-*`, `hub-models-top-first-read-*`), the accent switch in both, the shared status line in Models, Lineage and both hub screens. The hub-only cost line and tile press follow section 4. The hub has no Lineage view. |

### Average, lowest, decision

- Sum 111 over 27 criteria: **average 4.11**.
- Lowest: **3**, on three criteria: Device (5.D), Contrast (6.C) and Plain language (6.L).
- Decision: **release.** Nothing is below 3 and nothing stops a household. The change's centre, saying which
  model is really running, now holds in every state in both UIs, and the first round's contradictions are gone.
  Of the three items at 3, the contrast failures are inherited switch and flash colours shared with other
  screens, and the other two need the pond's own catalogue (a memory figure, the measured sentence). They are
  the next plan, below, not a reason to hold this branch.

### The first review's "Still open" list

| Item | Status | Evidence |
|---|---|---|
| A fit reading on the Memory rows | Open | No fit or memory figure on any row (`classic-models-jobs-failed-light.png`, `live-classic-models-jobs-light.png`; `components/models/MemoryModels.tsx:149-171`). The live catalogue sends `ram_estimate_mb` and it is unused. |
| The classic settings switch is blue | Fixed | `#7C3AED` at 5.70:1 light, `#9058F0` at 3.90:1 dark, from the pixels of `classic-settings-search-by-meaning-row-offer-*`; `styles/settings-catalogue.css:606-617`. New, separate finding: its off state measures 1.19:1 / 1.14:1. |
| The Jobs role card names a model that did not load with no state | Fixed | "Chosen, not loaded", "Downloading", "Checking" on the card (`classic-models-jobs-failed-*`, `-downloading-light`, `-first-read-*`) and the hub tile (`hub-models-top-failed-800x480-*`). |
| The hub's Memory tile does not take a press to its card | Fixed | Tiles are buttons with a chevron that scroll to and focus the card (`hub/views/settings/Models.tsx:105-118,251-264`; `hub-models-top-*`). Related gap: the hub Memory screen's "Choose a model" still lands at the top of Models (`hub/views/settings/Memory.tsx:455`). |
| The slate re-read bar and the purple coverage bars stacked on Lineage | Open | Unchanged: `classic-context-lineage-reindexing-light.png` matches `before/` pixel for pixel in that region. |
| `role="status"` speaks every 3 s while reading again | Fixed | Stable spoken sentence, visible count `aria-hidden`, count on `aria-valuetext` (`components/models/MemoryModels.tsx:87-104`); `sections/context/Lineage.test.tsx` checks the spoken text across two polls. Not heard with a screen reader. |
| "GPU" and "CPU" in the measured line | Open | Still on the live pond (`live-classic-models-jobs-*`, `live-hub-models-memory-*`); the sentence lives in `crates/pond-core/src/models/domain/recommended.rs:109`, which the branch does not touch. |
| The 800x480 cost line is the faintest text on the card | Fixed | 13px in the text colour, 18.06:1 light and 15.12:1 dark (`hub/hub.css:723-725`; `hub-models-memory-offer-800x480-*`). The measured caption and offline note, at 12px slate, are now the faintest. |
| Before the first health read, a chosen model briefly shows "In use" | Fixed | "Checking" until the answer (`classic-models-jobs-first-read-*`, `hub-models-memory-card-first-read-*`; `sections/models/memoryEmbedder.ts:220`). If that first read fails, the badge falls back to "In use" (`hooks/useEmbedderHealth.ts:68`). |

### The three lowest items now, with a fix for each

**1. Device (5.D, 3): say what a Memory model needs before it is chosen, and keep the way out on screen.**
- `components/models/MemoryModels.tsx:170`: after `memoryCost(model)`, add the memory figure the pond already
  sends, from `model.ram_estimate_mb`: "295 MB to download, needs about 368 MB of memory while it runs", same
  mono slate as the cost line.
- `components/models/MemoryModels.tsx:238` (`front`): when `line?.kind === "failed"` and
  `isOutOfMemory(line.detail)`, keep `all-MiniLM-L6-v2` in front of the fold too, so the hub shows the row the
  error names with its Use button.
- Later, in the pond: weigh the Memory recommendation against free memory
  (`crates/pond-core/src/models/domain/recommended.rs`), so a pond without room does not see "Recommended" on a
  model it cannot load.

**2. Contrast (6.C, 3): make an off switch visible and the switch's confirmation readable.**
- `styles/settings-catalogue.css:610`: add `--switch-control-bg: var(--color-text-tertiary);` to
  `.scat__swWrap .switch`. Off track `#566178` measures 6.22:1 on the light card with the white knob at 6.22:1;
  `#8B8696` measures 4.80:1 on the dark card with the knob at 3.53:1. Put those numbers in the comment.
- `hub/hub.css:685`: `.htoggle` background `#D8D5E0` (1.45:1) to `var(--color-text-tertiary)`, same numbers.
- `hub/views/settings/Memory.tsx:381-390`: replace the hard-coded flash colours (`#16a34a` on `#f0fdf4`,
  3.15:1; `#dc2626` on `#fef2f2`, 4.41:1) with tokens that clear 4.5:1 in both themes, for example
  `--color-text` on `--color-bg` with a lucide Check or AlertTriangle; and say the failure in words rather than
  `String(e)`.

**3. Plain language (6.L, 3): spell the measured line out, and name the default.**
- `crates/pond-core/src/models/domain/recommended.rs:109` (and the copy in `sections/models/fixtures.ts:248`):
  "Finds a memory in 18 ms on the graphics chip (GPU), or 41 ms on the main processor (CPU). Puts the right
  Swahili or mixed-language fact in its top five 73% of the time, against 39% for all-MiniLM-L6-v2, the
  default."
- `sections/context/Lineage.tsx:200-202`: "Anything not yet read for meaning is still found by word, just less
  well."
- While there, set the measured sentence in the body face and keep mono for the numbers alone
  (`styles/memory-models.css:100-105`), which also answers 8.P.

### Addendum at 9780a768

Scored: the same worktree at `9780a768`, three commits on `ec50127b`: `f7543ab8` (off switch track in both UIs),
`ac35e1e0` (the hub Memory screen's switch waits for settings, read apart from the memory list) and `9780a768`
(the hub toggle's knob rests at the track's ends). Evidence: `git log -p ec50127b..HEAD -- pond-desktop`, the code
at the head, and the retaken shots `classic-settings-search-by-meaning-*` (16:05) and
`hub-memory-search-by-meaning-*` (16:10), with pixels sampled from both. Nothing was run or changed.

#### What the shots and code show

- **Off track, both UIs.** Fixed. The pixels read `#566178` at 6.22:1 on the light card and `#8B8696` at 4.80:1
  on the dark card, the same in classic (`classic-settings-search-by-meaning-row-off-*`) and on the hub
  (`hub-memory-search-by-meaning-off-800x480-*`). The white knob is 6.22:1 and 3.53:1 on them. The tokens are
  `styles/settings-catalogue.css:613-615` and `hub/hub.css:685-687`. The "on" fills are unchanged: 5.70:1 light
  and 3.90:1 dark. In light, the off slate is now about as dark as the on purple. The knob's side and the hue
  carry the state, which is enough now that the knob sits at the ends.
- **Hub switch waits for settings.** Fixed. `semanticOn` starts null and the row shows a placeholder until
  `getSettings` answers, read on its own (`hub/views/settings/Memory.tsx:234,247-257,441-444`). The off-state shot
  now shows the switch off beside "Search by meaning is off. Memory searches by keyword." under the same
  memory-list failure that kept it on before (`hub-memory-search-by-meaning-off-800x480-*`). Two small things
  are left:
  - The Toggle keeps its own state and is now keyed only by `switchKey` (`Memory.tsx:444`; the Toggle is
    `hub/views/settings/controls.tsx:11-12`). A setting changed in classic or on a phone, followed by "Refresh
    memories" (`Memory.tsx:355`) or a saved memory (`:302`), updates the Model line but leaves the switch
    showing the old position. The old key remounted it on every load.
  - If `getSettings` fails, the placeholder stays for good with no word to say why. It is `aria-hidden`, so a
    screen reader hears no switch at all.
- **Knob at the track's ends.** Fixed. In `hub-memory-search-by-meaning-off-800x480-dark.png` the track spans
  x 711-756 and the knob sits at 714-734. In `-failed-800x480-dark.png`, with the switch on, the knob sits at
  734-752, inside the track. I missed this in my first pass. Before the fix, the knob's position did not say
  off or on, and only the hue did. Had I caught it, it would have counted against 3.1 and 6.C; it is gone now.

#### Scores that change

| # | Criterion | Old | New | Evidence |
|---|---|---|---|---|
| 6.C | Contrast | 3 | 4 | The off switch now clears 3:1 in both UIs and themes: track 6.22:1 light and 4.80:1 dark, knob 6.22:1 and 3.53:1, measured from the pixels of `classic-settings-search-by-meaning-row-off-*` and `hub-memory-search-by-meaning-off-800x480-*`. One failure remains on these surfaces, the hub Memory screen's confirmation after the switch, `#16a34a` on `#f0fdf4` at 3.15:1 and 13px, with no dark variant (`hub/views/settings/Memory.tsx:383-388`, unchanged). |

Every other criterion keeps its score.
- **3.6 and 3.9 stay at 4.** The default-on claim is gone, but the stale switch after a refresh takes its place
  as a smaller leftover. The earlier leftovers stand: "Recommended" on a model that does not fit, a failed first
  read reading "In use", and raw exception text.
- **6.D stays at 4.** The off switch no longer vanishes, but the 12px slate captions and the hairline status box
  still leave refinements possible.
- **7.O stays at 4.** The hub switch now agrees with the status line. The classic Jobs card still offers
  "Choose", not "Turn on".

#### Sum, average, decision

- Sum 112 over 27: **average 4.15** (was 4.11).
- Lowest: **3**, now on two criteria: Device (5.D) and Plain language (6.L). Their fixes above still apply.
- Decision: **release.**
- One fix before the next change to that screen: key the hub switch on its value as well, `key={`${semanticOn}-${switchKey}`}` at `hub/views/settings/Memory.tsx:444`, or make `Toggle` follow its `on` prop.
  - The same trap leaves the Auto-compaction toggle on that screen at its default `false` once settings arrive, because it has no key (`Memory.tsx:426`). That card is outside these surfaces.
- Also before the next change: give the placeholder a visible word if `getSettings` fails.

## Re-score: Reviewer B

**Tree scored:** branch `docs/memory-embedder-gate-rescore` at `ec50127b` (main `ffe2ef23` plus six commits), worktree
`.claude/worktrees/memory-gate-rescore`. **Standard:** `DESIGN.md` section 9 in that tree. **Evidence:** the `after/`
shots (classic Jobs band in all ten states, focus, Settings, Lineage; hub top, Memory card and Memory screen at 800x480 and
1024x600; the six `live-*` replays), `before/` for comparison, `../shots.spec.ts` for fixtures, `../live/*.json`, the diff
`ffe2ef23..HEAD -- pond-desktop docs`, and the code behind each surface. Contrast is computed with the WCAG 2.1 formula from
the hex values in the CSS, and where a value is composited or set at runtime, from screenshot pixels. Paths are relative to
`pond-desktop/src/` unless they start with `crates/`.

### Caveats on the evidence

- **The hub Memory screen's switch.** In `hub-memory-search-by-meaning-off-800x480-*` the "Search by meaning" switch is on
  while the line under it says search by meaning is off. I traced it to the listed capture artefact: `MemoryDetail` reads the
  settings in the same `Promise.all` as the memory list (`hub/views/settings/Memory.tsx:250`). When the list fails, the
  setting is never applied, and the switch keeps its default of on (`:233`). I did not score the picture. I did score the code
  path, because on a real pond the switch also reads "on" until settings arrive (see 3.9).
- **Hover, not state.** "Other Memory models" is tinted in a few hub shots (`hub-models-memory-failed-800x480-*`,
  `hub-models-memory-card-downloading-800x480-dark`). That is `.mm-btn:hover` under the parked mouse
  (`styles/model-marks.css:45`), not a state. Not scored.
- **Not seen.** No shot shows focus on the hub: a tile, or the card focused after a tile press. No shot shows focus on the
  Settings switch, the loading state on the hub Memory screen at 1024x600, or a dimmed panel. I judged dimming from measured
  contrast and type size, not from a dimmed capture. I could not judge from the client whether an offline pond can fetch the
  "fetched the first time it is used" models (all-MiniLM-L6-v2, BGE Small).
- **Live numbers I could not read.** The live `memory-status.json` reports `available_for_llm_mb: 309`, against
  EmbeddingGemma 2's `ram_estimate_mb: 368`, yet the model is ready on the live pond. The budget evidently means something
  narrower than the room left for an embedder. I cite it only as the kind of number a fit reading would have to speak to.
- **Not re-run.** I ran no tests; the brief says `tests/e2e/memory-embedder.spec.ts` passes at the head. I read it: it
  asserts what it claims, including the 44px tile, focus moving to the card, and contrast of the new words in both themes.

### Scores

| # | Criterion | Score | Evidence |
|---|---|---|---|
| 1 | 3.1 State is elevation, not hue | 4 | Every new state is a word and an icon: "Checking", "Downloading", "Chosen, not loaded" on the Jobs card, tile and row (`components/models/MemoryModels.tsx:40`, `sections/Models.tsx:54`; `classic-models-jobs-first-read-light`, `hub-models-top-failed-800x480-light`). Only a model that is not running borrows the warning colour, and always with an icon. The model in use still lies flat while the page raises its chosen picks. `styles/memory-models.css:3-4` argues this, and it remains a deliberate deviation. |
| 2 | 3.2 Spend the offset about twice | 5 | The branch adds no offset. The new tile buttons stay on a hairline (`hub/hub.css:711-713`); the rows and status boxes are flat in every shot. |
| 3 | 3.3 The edge marks content, never chrome | 4 | Right where it is new: the tiles became navigation and kept a 1px line, with a hover tint only. Still in classic failed: the 2px group holds a tinted 1px status box, which holds 1px pills, so three frames are stacked (`classic-models-jobs-failed-light`). |
| 4 | 3.4 Press is physical | 4 | Nothing new is raised, so nothing needs to lift. The tile has `:hover` but no `:active` (`hub/hub.css:712`), so on a touch panel the only press feedback is the scroll itself. The screen's one question, "Download and use" on the recommendation or "Turn on" when off, is still a plain `mm-btn`, not the `mm-btn--ask` press. |
| 5 | 3.5 Let form carry data | 4 | The re-read bar is items done; the download bar is bytes with Pause and Stop (`hub-models-memory-card-downloading-800x480-dark`). Lineage still stacks the slate re-read bar over purple per-store bars that measure the same thing (`classic-context-lineage-reindexing-light`). |
| 6 | 3.6 Never invent meaning | 4 | The intro no longer promises Swahili recall; the bars count items, and 40 of 95 now agrees with 40 of 45 plus 0 of 50 on Lineage. "Every memory can be found by meaning" holds against the live corpora: `rows` counts live rows, `crates/pond-core/src/context/vector_index.rs:104`. One claim the data lacks remains, under 3.9: the hub switch says "on" before the pond has answered. |
| 7 | 3.7 Every colour carries its measurement | 4 | Each new colour rule carries its ratio, and I reproduce every one: switch fill 5.70:1 light; dark lift 3.90:1 with the knob at 4.34:1 (`styles/settings-catalogue.css:606-617`, `hub/hub.css:687-688`); hub cost line 18.06:1 and 15.12:1 (`hub/hub.css:724-725`). Unrecorded: the dark hover lift, 75% toward white (I measure 4.69:1, knob 3.61:1); the chevron `--mut` (6.22:1 and 4.80:1); and the re-read fill `--color-text-tertiary` (`styles/memory-models.css:199`; 6.22:1 and 4.80:1 against its track). |
| 8 | 3.8 Touch the chrome, not the data | 4 | The tiles are buttons of at least 44px (spec asserts it); Details is a 44px text button. The classic rows keep full 44px pills instead of a compact look with a 44px hit area: inherited, and it inflates the dense view slightly. |
| 9 | 3.9 Say what happened | 4 | Much better: the Jobs card and hub tile now say when the model is not running, and "Checking" replaces an early "In use" (`sections/models/memoryEmbedder.ts:216-244`; `classic-models-jobs-first-read-*` against `before/`). Remaining: (a) a health read that errors, rather than one that is slow, ends `loading` (`hooks/useEmbedderHealth.ts:64-69`) and falls through to "In use"; (b) the hub Memory screen's switch defaults to on until settings arrive (`hub/views/settings/Memory.tsx:233`); (c) after "Use" while search is off, no row or card shows which model is set once the toast has gone, because `resolvedMemoryName` returns null while off (`memoryEmbedder.ts:22-24`); (d) "Turn on" does not say it will start a 295 MB download when the set model is not on disk (`hooks/useMemoryJob.ts:54-60`). |
| 10 | 5 Network: works offline | 4 | Under `network_mode = "offline"` the download rests as "Needs a connection to download", with the reason tied by `aria-describedby` (`classic-models-jobs-offline-light`, `hub-models-memory-offline-800x480-dark`); keyword search keeps Memory working. Not covered: rows marked "fetched the first time it is used" stay pressable offline, with a line promising a fetch (`memoryEmbedder.ts:68-75`, `:268-279`). The out-of-memory error recommends all-MiniLM-L6-v2 whatever the network mode. |
| 11 | 5 Device: the Orin is the low-end device | 3 | Good: the measurements are dated and taken on the Orin; polling backs off and pauses while hidden; the out-of-memory error names a lighter model. Still missing: what a Memory model costs in RAM is never shown, though the catalogue carries it (`ram_estimate_mb` 368 for EmbeddingGemma 2 in `../live/models.json`), and the chat picks on the same screen say what fits (`hub-models-top-first-read-1024x600-dark`). `sections/models/modelsView.ts:95` excludes embeddings from the fit reading. The only device failure these surfaces show, out of memory, is learned after choosing. Lineage's "Read memories again" clears every vector (`sections/context/Lineage.tsx:78-79`), makes the board read everything again, asks for no confirmation, and stays pressable during a re-read (`:182`). |
| 12 | 5 Data: say the number first | 4 | "Download and use" carries the size in its accessible name (`MemoryModels.tsx` row button); "295 MB to download" sits beside every press, and the transfer shows "117 MB of 295 MB". Gap: the 295 MB can still be spent by "Turn on" (classic status box) or the Settings and hub switches, none of which names the size. |
| 13 | 5 Screen: 1024x600 and 800x480 | 4 | Fixed: the Memory tile is on the first screen at 800x480 and takes a press to the card, with focus moved there (`hub/views/settings/Models.tsx:105-120`). Remaining: the hub Memory screen's "Choose a model" still lands at the top of Models (`hub/views/settings/Memory.tsx:455`). At 800x480 the out-of-memory error's remedy, all-MiniLM-L6-v2, is folded behind "Other Memory models (2)" (`hub-models-memory-failed-800x480-light`; `MemoryModels.tsx:238` keeps only the recommendation and the holder in front). |
| 14 | 6 Contrast (WCAG 2.1 AA) | 3 | Text passes everywhere I measured. Cost line 18.06:1 and 15.12:1; slate 6.22:1 and 5.45:1; warning text 7.09:1 and 10.15:1 on the card, 6.41:1 and 11.21:1 in the status box; chevron 6.22:1 and 4.80:1. The switch, on, is 5.70:1 light and 3.90:1 dark (pixels match the CSS). Focus ring from pixels: 5.70:1 light, 4.50:1 dark (`classic-models-memory-focus-*`). Fails the 3:1 non-text floor: the off switch. In classic the track is rgb(235,235,236) on white, 1.2:1, with a white knob at 1.0:1 (`classic-settings-search-by-meaning-row-off-light`); in dark it is rgb(39,39,42) on rgb(30,27,38), 1.1:1. On the hub the off toggle is `#D8D5E0` on white, 1.45:1 (`hub/hub.css:685`; `hub-memory-search-by-meaning-off-800x480-light`). The branch restyled the on state of exactly this switch and left the off state under the floor. |
| 15 | 6 Keyboard and visible focus | 4 | Every control is a native button; the tiles move focus to a `tabIndex=-1` card (`hub/views/settings/controls.tsx:107`), and `[tabindex]:focus-visible` gives it the ring (`styles/base.css:250-254`). The classic focus ring is visible and measured. Not seen in any shot: focus on the hub. |
| 16 | 6 Screen readers | 4 | Fixed: the live region holds "Reading memories again" while the counting line is `aria-hidden`, and the count moved to the bar's `aria-valuetext` (`MemoryModels.tsx:86-104`; `memoryEmbedder.ts:161-163`). Tile names carry the state: "Memory: EmbeddingGemma 2, Chosen, not loaded. Change". Gap: focus lands on a `div` with no role and no name (`controls.tsx:107`), so a screen reader is not told it arrived at the Memory card; `role="region"` with `aria-labelledby` on the title would. The "Checking" to "Chosen, not loaded" change is heard only through the status box's `role="alert"`, which is acceptable. |
| 17 | 6 Reduced motion | 5 | The new scroll is instant under `prefers-reduced-motion` (`hub/views/settings/Models.tsx:113-119`). The "Checking" spinner uses `.mm-spin`, which stops (`styles/model-marks.css:422`), and `styles/touch.css:337-342` catches every remaining transition, the toggles included. |
| 18 | 6 Plain language | 3 | One name for re-reading, the model's name on Lineage, the raw reason behind Details: all good. Left: "GPU" and "CPU" are never spelled out in the measured line, which the live pond shows on both UIs (`live-classic-models-jobs-light`, `live-hub-models-memory-800x480-light`; text from `crates/pond-core/src/models/domain/recommended.rs:109`). "There is not enough free memory to load EmbeddingGemma 2, so Memory is searching by keyword" uses "memory" twice in two senses, RAM and the household's Memory, on a card titled Memory (`memoryEmbedder.ts:146`). The rest of the screen calls RAM "room" ("Room for one model: 5.0 GB"). Lineage still says "Anything not indexed" (`sections/context/Lineage.tsx:201`). |
| 19 | 6 Dim 800x480 panel | 4 | The cost line is now body size in the text colour (`hub-models-memory-offer-800x480-*`). Still weak on a dim panel: the off toggle at 1.45:1 in light, scored under 14; the measured line, a whole sentence in small mono with a small slate caption (`live-hub-models-memory-800x480-light`); tiles whose edge is 1.11:1 against the ground, so the chevron carries the affordance. |
| 20 | 7 Tone | 4 | Household words: "Search by meaning", "at a quiet moment", "Built-in default", "Needs a connection to download". The double "memory" in the error, scored under 18, is the one sentence that reads as if written from the system's side. |
| 21 | 7 A control says what happens; one name per action | 4 | "Download and use", "Read memories again" and "Reading memories again" agree across both UIs. Left: the error says "Choose the lighter all-MiniLM-L6-v2" over a button that says "Use"; Lineage's failure fallback calls the same action "a rebuild" (`Lineage.tsx:87`); "Turn on" does not say it can start a download. |
| 22 | 7 Errors | 4 | It says what went wrong, names a lighter model with its size, keeps a retry, and puts the raw reason behind Details (`classic-models-jobs-failed-*`, `hub-memory-search-by-meaning-failed-800x480-light`). But the remedy is not a press away: it is behind the fold on the hub, and the hub Memory screen sends you to the top of Models. Failures of "Turn on" and of a retry still surface the server's raw `e.message` (`hooks/useMemoryJob.ts:62`, `:72`). |
| 23 | 7 Off and empty states | 4 | "Search by meaning is off. Memory searches by keyword." with "Turn on" in both UIs; the Jobs card says "Switched off" (`classic-models-jobs-off-light`, `hub-models-memory-off-800x480-light`). After a "Use" while off, nothing marks the set model (3.9c), and the hub switch can read "on" until settings arrive (3.9b). |
| 24 | 7 No emojis | 5 | No non-ASCII code point in any added line of the diff; the icons are lucide (`ChevronRight`, `Loader2`, `AlertTriangle`, `Clock`, `Check`). |
| 25 | 8 Palette and type | 4 | Fixed: the classic switch now wears the household's accent, not HeroUI blue (`classic-settings-search-by-meaning-offer-*`). Quicksand for words, JetBrains Mono for sizes. Mono is still stretched to whole sentences: the measured summary, and on the hub the cost phrase "23 MB, fetched the first time it is used" is now body-size mono. |
| 26 | 8 Logo and attribution | 5 | Untouched; nothing to judge. |
| 27 | Both UIs carry it | 4 | One `HolderBadge`, one `jobState` and one `EmbedderStatus` feed the classic Jobs card, the hub tile and both Memory screens; the switch accent and its dark lift match in both. Asymmetries: the classic Jobs card's "Change" lands on its list, but the hub Memory screen's "Choose a model" does not land on the Memory card; the default-on switch exists only on the hub Memory screen. |

### Average, lowest, decision

**Average: 109 / 27 = 4.04** (105 / 26 = 4.04 without the both-UIs row). **Lowest: 3**, three criteria: Device (11),
Contrast (14), Plain language (18). Nothing is below 3.

**Decision: release.** The six commits do what they claim, and the shots and code agree. Four of the first review's open items
are fixed outright: the switch colour, the Jobs card state, the chatty live region and the 800x480 cost line. Two more are
fixed with a small residue: the early "In use" and the Memory tile. Nothing the branch adds lowers a score, and nothing scores
below 3. What keeps this from a cleaner release is older than the branch, and should be the next change rather than a reason to
hold this one:

- The off switch, in both UIs, misses the 3:1 non-text floor that `DESIGN.md` section 6 calls non-negotiable.
- No Memory row gives a RAM figure.
- Two pieces of wording remain.

If the team reads the section 6 floor as blocking for any surface this gate touches, the off-switch fix is a few lines and
should go in before the merge. I would accept that as "revise (light)" and not argue it.

### The first review's "Still open" list

| Item | Status | Evidence |
|---|---|---|
| A fit reading on the Memory rows (Reviewer A) | **Open** | `MemoryRow` shows only `memoryCost` (`MemoryModels.tsx:154-170`); `modelsView.ts:95` excludes embeddings from fit; no fit shows in any shot, the live replay included. |
| The classic settings switch is blue, not Jarida purple | **Fixed** | `styles/settings-catalogue.css:606-617`; measured from pixels, rgb(124,58,237) light at 5.70:1 and rgb(144,88,240) dark at 3.90:1 (`classic-settings-search-by-meaning-row-offer-*`). New, separate finding: its off state is 1.2:1 (criterion 14). |
| The Jobs role card names a model that did not load with no state of its own | **Fixed** | `sections/Models.tsx:54`, `:331`, `:344`; "Chosen, not loaded" in `classic-models-jobs-failed-*`, "Downloading" and "Checking" in their shots; the hub tile carries the same badge. |
| The hub's Memory tile does not take a press to its card | **Fixed**, small residue | The tiles are buttons with a chevron that scroll to their card and focus it (`hub/views/settings/Models.tsx:105-120`, `:226-270`; spec test "the Memory tile takes a press to the Memory card"). Residue: the hub Memory screen's "Choose a model" still lands at the top of Models. |
| The slate re-read bar and purple coverage bars stacked on Lineage | **Open** | No change to `Lineage.tsx`'s bars; both are still visible in `classic-context-lineage-reindexing-*`. The numbers now agree (40 of 95 = 40 of 45 + 0 of 50). |
| `role="status"` speaks every 3 s while reading again | **Fixed** | The live text is the fixed "Reading memories again"; the count line is `aria-hidden`, and the count is in `aria-valuetext` (`MemoryModels.tsx:86-104`); `sections/context/Lineage.test.tsx` checks what is heard across a re-render. |
| "GPU" and "CPU" in the measured line | **Open** | Still in `crates/pond-core/src/models/domain/recommended.rs:109`; visible in `live-classic-models-jobs-*` and `live-hub-models-memory-800x480-*`. |
| The 800x480 cost line is the faintest text on the card | **Fixed** | `hub/hub.css:724-725`: `--text-base` in `--color-text`, 18.06:1 and 15.12:1 computed; `hub-models-memory-offer-800x480-*`. |
| Before the first health read, a chosen model briefly shows "In use" | **Fixed**, small residue | `holderBadge(line, { checking })` (`memoryEmbedder.ts:216-220`); "Checking" in `classic-models-jobs-first-read-*` and `hub-models-top-first-read-*`, then "Chosen, not loaded" once the answer arrives. Residue: a first read that errors, rather than one that is slow, still ends on "In use" (`hooks/useEmbedderHealth.ts:64-69`). |

### The three lowest, with a fix for each

**1. Contrast, 3: the off switch, in both UIs.**

- In `hub/hub.css:685`, give `.htoggle` an off track that clears 3:1 in both themes. Either fill it with
  `var(--color-text-tertiary)` (6.22:1 on white with the knob at 6.22:1; 4.80:1 on the dark panel with the knob at 3.53:1),
  or keep the light fill and add `box-shadow: inset 0 0 0 2px var(--color-text-tertiary)` so off reads as hollow.
- In `styles/settings-catalogue.css`, inside the `.scat__swWrap .switch` block the branch added, set `--switch-control-bg`
  and `--switch-control-bg-hover` the same way. HeroUI exposes both variables.
- Write the ratios in the comment.
- Add an off-state `contrastOf` check beside the on-state one in `tests/e2e/memory-embedder.spec.ts`.

**2. Device, 3: say what a Memory model takes before it is chosen.**

- In `sections/models/memoryEmbedder.ts`, `memoryCost` (lines 68-75), take `ram_estimate_mb` and add the running cost in the
  screen's own word: "295 MB to download, about 370 MB of room while it runs".
- Where `ModelMemoryStatus` gives a budget, have `MemoryRow` (`components/models/MemoryModels.tsx:154`) say "Tight on this
  pond" in the existing `mm-fit--wait` word form when the estimate would not fit beside the conversation model. This needs
  the budget for an embedder defined first; `modelsView.ts:95` says the conversation budget does not apply.
- When the line is out of memory, let the fold keep the lighter model in front: extend `front` at `MemoryModels.tsx:238` so
  the remedy the error names is one press away at 800x480.
- In `sections/context/Lineage.tsx:182`, disable "Read memories again" while a re-read runs, and ask before clearing.

**3. Plain language, 3: two words a household cannot read.**

- At `sections/models/memoryEmbedder.ts:146`, replace "There is not enough free memory to load {title}" with "This pond
  does not have room to load {title} beside the conversation model". That keeps "memory" for the household's Memory and
  matches "Room for one model".
- At `crates/pond-core/src/models/domain/recommended.rs:109`, spell out the acronyms: "18 ms on the graphics processor (GPU)
  or 41 ms on the main processor (CPU)".
- At `sections/context/Lineage.tsx:201`, replace "Anything not indexed" with "Anything not read yet", which matches "Read
  memories again".
- Update the strings the unit and e2e tests pin (`memoryEmbedder.test.ts`, `memory-embedder.spec.ts`).

Also worth taking with these, from 3.9: show the set model while search is off. Do this with a "Set, waits for search by
meaning" badge from `holderBadge` when `memoryIsOff` and `roles.embedding.model` is named. Also, read the hub Memory
screen's switch from settings on its own, not inside the memory list's `Promise.all` (`hub/views/settings/Memory.tsx:250`),
and leave it unset until they arrive.

### Addendum at 9780a768

**What I checked:** `git log -p ec50127b..HEAD -- pond-desktop` (three commits: `f7543ab8`, `ac35e1e0`, `9780a768`) and the
retaken shots `classic-settings-search-by-meaning-*` (16:05, after `f7543ab8`, which is the last commit to touch classic) and
`hub-memory-search-by-meaning-*` (16:10, after `9780a768`). I measured the switches from the pixels again. Nothing else changed,
so every other score stands.

**What the pixels say**

| Switch | Off: track on card, knob on track | On: fill on card, knob on fill | Knob position (track x 711-756) |
|---|---|---|---|
| Hub, light (`hub-memory-search-by-meaning-off-800x480-light`, `-failed-800x480-light`) | rgb(86,97,120) 6.22:1, 6.22:1 | rgb(124,58,237) 5.70:1, 5.70:1 | off 715-733, on 734-752 |
| Hub, dark (`-off-800x480-dark`, `-reindexing-800x480-dark`) | rgb(139,134,150) 4.80:1, 3.53:1 | rgb(144,88,240) 3.90:1, 4.34:1 | off 714-734, on 734-752 |
| Classic, light (`classic-settings-search-by-meaning-row-off-light`) | rgb(86,97,120) 6.22:1 | unchanged, 5.70:1 | knob at the left end |
| Classic, dark (`-row-off-dark`) | rgb(139,134,150) 4.80:1 | unchanged, 3.90:1 | knob at the left end |

Every number matches the comments the commits added (`hub/hub.css:685-687`, `styles/settings-catalogue.css:613-615`). The off switch now clears the 3:1 non-text floor in both UIs and both themes.

**Something I missed in the first pass.** `9780a768` fixes a fault that was visible in shots I had already scored. Before it,
the hub toggle's knob sat mid-track when off and about 10px past the right end when on. You can see this in the earlier
`hub-memory-search-by-meaning-off-800x480-light`, where the "Compact memory nightly" knob is centred. So on the hub, a toggle's
state was carried by its fill colour alone. Had I caught it, 3.1 and Contrast would have scored lower at `ec50127b`. At
`9780a768` the knob rests inside the track at either end (table above), and the spec checks both ends, so the fault is gone
and 3.1 stays at 4.

**What `ac35e1e0` changes**

- The hub Memory screen reads settings on their own (`hub/views/settings/Memory.tsx`, `loadData`) and shows a placeholder
  until they arrive. In the retaken `-off-800x480-*` shots the memory list still fails, because the capture's banner is still
  there, but the switch now reads off beside "Search by meaning is off". That contradiction is gone.
- New, small: if the settings read itself fails, `.catch(() => undefined)` leaves `semanticOn` null, so the row keeps an
  `aria-hidden` grey placeholder for good, with no word for a sighted reader or a screen reader. A disabled switch and "Could
  not read this setting" would say what happened. This does not change a score.

**Scores that change**

| # | Criterion | Old | New | Evidence |
|---|---|---|---|---|
| 14 | 6 Contrast (WCAG 2.1 AA) | 3 | 4 | The one failure I scored, the off switch, now measures 6.22:1 light and 4.80:1 dark against the card in both UIs, with the knob at 6.22:1 and 3.53:1 (table above). `tests/e2e/memory-embedder.spec.ts` now measures the off fill against what is behind it in both themes and both UIs (`fillContrastOf`, `tests/e2e/helpers/contrast.ts`). It is not 5: the dark margins are thin (knob 3.53:1 off, fill 3.90:1 on), on a product whose own tokens warn that a boundary which only just clears 3:1 on a bright monitor is gone on a dim panel. |

**Scores that stand, with evidence brought up to date**

- **3.6 stays 4.** The claim I cited, the hub switch saying "on" before the pond answered, is fixed. What remains is a health
  read that errors and still ends on "In use" (`hooks/useEmbedderHealth.ts:64-69`). There is also "23 MB, fetched the first
  time it is used" beside a model that is already in use (`classic-models-jobs-offer-light`).
- **3.9 and Off and empty both stay 4.** Item (b) is fixed. Items (a), (c) and (d) remain, plus the silent placeholder above.
- **Dim 800x480 panel stays 4.** The off switch is now visible from across a room. But in light, off and on are now the same
  lightness: slate against purple measures 1.09:1, and 1.23:1 in dark. Both read as a filled dark pill, so knob position and
  hue carry the state. That is enough now that the knob sits at the ends. A hollow off would read as empty at a glance: keep
  a light fill and draw a 2px `--color-text-tertiary` ring. The measured line in small mono and the 1.11:1 tile edges are
  unchanged.
- **Both UIs stays 4.** The hub-only default-on switch is gone, and the off track is shared. The hub Memory screen's "Choose
  a model" still does not land on the Memory card.

**New sum and average:** 110 / 27 = **4.07** (106 / 26 = 4.08 without the both-UIs row). **Lowest: 3**, two criteria: Device
(11) and Plain language (18). Nothing is below 3.

**Decision: release.** Nothing is left at the accessibility floor, so I withdraw the caveat I attached to the decision above.
The plan after this change is the two items still at 3: the RAM figure and fit on the Memory rows, and the wording ("free
memory", GPU and CPU, "not indexed"). The fixes for both are as written in "The three lowest, with a fix for each", items 2
and 3.
