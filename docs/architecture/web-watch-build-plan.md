# Web watches and exploration: build plan

The running plan for building `web-watch-and-exploration.md`. It is updated as each phase lands.
Last update: 2026-11-09, phase 2 merged.

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
| 3 | Phone (GOTG): watch list and detail, saved collections, report with chips and images | Waiting on 2 and 5 | goose-on-the-go repo |
| 4 | Exploration: 30-minute budget on the card, one site per step, compare step writes options | Next | |
| 5 | Saved items: typed cards, collections, share switch, "Watch price", search tool | | |
| 6 | Images: Wikimedia Commons and preview images, the pond's copy, the credit | | |
| 7 | Lightpanda: aarch64 build against glibc 2.35, nano measurement, on-demand process | Needs a nano session | |
| 8 | POST forms confirmed on the phone; spoken alerts opt-in per member with quiet hours | | |
| 9 | Desktop screens | | |
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

## What waits on you

| What | Why |
|---|---|
| A nano deploy, now that phase 2 has landed | First live check of reads, watches and pushes, and the first compile of pond-server with them |
| Merging org PRs #849 and #851 | They are merged on the fork only |
| Defaults: 30 days of change history, 20 watches per member, digest at 18:00, a turn's pause not counted against 30 minutes | Proposed in the design; I'm building with them unless you say otherwise |
