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

The fixes have not been re-scored.

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
