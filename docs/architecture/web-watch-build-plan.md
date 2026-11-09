# Web watches and exploration: build plan

The running plan for building `web-watch-and-exploration.md`. It is updated as each phase lands.
Last update: 2026-11-09: phases 1, 2, 4, 5 and 6 merged on the fork.

## How the work runs

- Each phase is one branch, merged into **the fork's main** (`JERRYFROMKENYA/goose-in-a-pond`).
  The org PRs (jarida-io #849, #851) stay open for you to merge there.
- Within a phase, the parts that don't touch the model go first:
  1. storage;
  2. code-only checks;
  3. routes.

  Then the parts the model sees: tool text and prompts. Then one test pass, at the end.
- Deploying to the nano always waits for your OK. Nothing below has been run on the nano or
  against live sites yet.

## Phases

| # | Phase | Status | Where |
|---|---|---|---|
| 1 | Reading: structured items, same-site links two clicks deep, GET forms, source ledger, 3 reads per chat turn | Merged | fork #3 (org #851) |
| 2 | Watches: store, code-only checks, push on a match, daily digest, `web_watch` tool, routes | Merged | fork #4 |
| 3 | Phone (GOTG): watch list and detail, saved collections, report with chips and images | Ready: its routes exist | goose-on-the-go repo |
| 4 | Exploration: 30-minute budget on the card, one site per step, compare step writes options | Merged | fork #5 |
| 5 | Saved items: typed cards, collections, share switch, "Watch price", search tool | Merged | fork #6 |
| 6 | Images: Wikimedia Commons and preview images, the pond's copy, the credit | Merged | fork #7 |
| 7 | Lightpanda: aarch64 build against glibc 2.35, nano measurement, on-demand process | Needs a nano session | |
| 8 | POST forms confirmed on the phone; spoken alerts opt-in per member with quiet hours | Proposed as its own session | |
| 9 | Desktop screens: watches, saved collections, report pictures, options and source chips | Proposed as its own session | |
| 10 | Site sign-in, after PAI-2 P9 (Pond Keychain) | Waits for P9 | |

## Phase 2: watches, what landed

| Step | Kind | Status |
|---|---|---|
| Tables `web_watches` and `web_watch_changes` (migration 0083; 0082 is left to the context-space branch), port in pond-core, SQLite in pond-infra | Code | Done |
| Item comparison: new, gone and price changed, keyed by link, else by title; a page that suddenly lists nothing counts as a failed check, not as everything gone | Code | Done |
| Filters: words, max price, min year, max mileage. A new match or a price drop on a match is pushed; everything else goes to the digest | Code | Done |
| Checker loop: due watches every 5 minutes; back-off doubles up to a day; the owner is told at the 3rd failed check in a row | Code | Done |
| Push to the owner's own devices, opening `watch` (`OpenScreen::Watch`); daily digest at 18:00, household time; changes kept 30 days | Code | Done |
| REST: `GET /web-watches`, `GET/PATCH/DELETE /web-watches/{id}` (detail lists items and the 50 newest changes) | Code | Done |
| `web_watch` tool (start, list, stop) for the member in the conversation; at most 20 watches each | Model | Done |
| Tool text and knowledge instructions. The tool adds 790 bytes to every turn's tool block, trimmed from 1,278 | Model | Done |
| Tests: pond-core 2,616, pond-mcp-server 380, pond-infra 610, pond-api 341 | Tests | Done |

Not done in phase 2:
- Creating a watch over REST. It comes with "Watch price" in phase 5; until then a watch starts from a conversation.
- Compiling pond-server on this Mac (llama, whisper and espeak don't fit in the free disk). The two lines in `main.rs` were checked by reading; the nano build will compile them.
- The desktop doesn't yet know the `watch` screen, so it opens its default view (phase 9).

## Phase 4: exploration, what landed

| Step | Kind | Status |
|---|---|---|
| Tasks started from a conversation and deep research get 30 minutes (`deadline`); activation tasks keep their 2 hours | Code | Done |
| Past the deadline a task no longer fails: steps that have not run are skipped, and the last step answers from everything saved. It gives up only 20 minutes past the deadline | Code | Done |
| `POST /long-tasks/{id}/extend` adds 15 minutes (or `minutes`, up to 60), counted from now once the time is up, at most 3 hours from the start | Code | Done |
| The task view carries `deadline`; the desktop card shows "12 of 30 min" or "Time's up: answering with what it found", with a "+15 min" button | Code | Done |
| The report JSON carries `options` (title, price, link, why, other fields, sources), read from `## Option N:` sections | Code | Done |
| Planner: told how many steps its time fits (one to three minutes each); for choices, one site per step, then a compare step that writes `## Option N:` sections | Model | Done |
| The answering step after time runs out is told so and given every saved artifact | Model | Done |
| Tests: pond-core 2,622, pond-mcp-server 380, pond-infra 611, pond-api 341, desktop long tasks 186 | Tests | Done |

Not done in phase 4:
- A pause for a person's turn still counts against the 30 minutes, as before. Not counting it needs the bus to record pause time per task.
- The phone card (phase 3) and saving an option (phase 5).
- One line in `main.rs` (handling `SkipForTime`) is compiled only by the nano build.

## Phase 5: saved items, what landed

| Step | Kind | Status |
|---|---|---|
| Typed cards (recipe, listing, place, report, note) with a Markdown body, sources and a collection; migration 0084, port, SQLite | Code | Done |
| A member sees their own cards and shared ones, and changes only their own; collections join by name without case ("cars" joins "Cars"); at most 500 cards each | Code | Done |
| REST: `GET/POST /saved-items`, `GET /saved-items/collections`, `GET/PATCH/DELETE /saved-items/{id}` (`shared`, `collection`, `null` to take it out) | Code | Done |
| "Watch price": `POST /saved-items/{id}/watch` starts a watch on a listing's or place's link; its first check keeps the baseline | Code | Done |
| `saved_items` tool (save, find, list). Saving takes the conversation's last answer, so a recipe is never typed out twice; a recipe's ingredients and steps are read from its lists | Model | Done |
| The tool adds 674 bytes to every turn's tool block | Model | Done |
| Tests: pond-core 2,626, pond-mcp-server 383, pond-infra 613, pond-api 341 | Tests | Done |

Not done in phase 5:
- Saving an option card straight from a report: the phone and desktop send it with `POST /saved-items` (phase 3 and phase 9).
- One flaky pond-core test (`device_budget`, "the qat row is its own entry") failed once and passed on two reruns. It is unrelated to this work and needs its own look.

## Phase 6: report images, what landed

| Step | Kind | Status |
|---|---|---|
| A cited Wikipedia article gives its lead image, with licence, author, caption and file page; a non-free (fair use) image is never taken | Code | Done |
| Any other cited page gives the preview image it names for itself (`og:image`), credited "Image from {site}" | Code | Done |
| The pond fetches each picture through its tracked, public-only client, keeps JPEG, PNG, WebP or GIF only (no SVG), at most 2 MB each and 200 MB in all (oldest go first), and serves it at `GET /report-images/{key}` | Code | Done |
| The report JSON carries `images` (up to 3): `src`, `credit`, `licence`, `caption`, `page`, `file_page` | Code | Done |
| Tests: pond-core 2,629 (egress guard included), pond-mcp-server 386, pond-api 341 | Tests | Done |

Not done in phase 6:
- Drawing the pictures. The route needs the client's token, so a screen fetches each picture with it (a blob on the desktop, headers on the phone) instead of a bare `<img>`. Phone in phase 3, desktop in phase 9.
- The numbered source chips in the text. The report still cites `[title](link)`; the chips phase switches the composer to ledger numbers and the screens to chips.

## What waits on you

| What | Why |
|---|---|
| **A nano deploy of fork main (`7ea54c09`)** | pond-server has not been compiled with these phases: the Mac lacks the disk for its native crates. Its new lines are four short calls in `main.rs`, typed against compiled signatures. The deploy is also the first live check of reads, watches, pushes, time budgets, saved cards and pictures |
| Phase 3, the phone screens | They live in goose-on-the-go: watches, saved collections, a report's pictures, options and source chips, and the `watch` notice screen (`services/notification-link.ts`). Say where to start it |
| Phase 7, Lightpanda on the nano | Building and measuring it runs on the nano beside the live pond, so it waits for your OK |
| Merging org PRs #849 and #851 | They are merged on the fork only; later phases have no org PRs |
| Defaults: 30 days of change history, 20 watches and 500 cards per member, digest at 18:00, a turn's pause counted against the 30 minutes for now | Proposed in the design; building with them unless you say otherwise |
