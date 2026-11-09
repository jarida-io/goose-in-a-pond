# Web watches and exploration: build plan

The running plan for building `web-watch-and-exploration.md`. It is updated as each phase lands.
Last update: 2026-11-09: phases 1, 2, 4, 5, 6, 8 and 9 merged on the fork.

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
| 3 | Phone (GOTG): watch list and detail, saved collections, report and chat with chips and pictures, pictures on conversation cards | PR open | goose-on-the-go #272, org #855 and #856 |
| 4 | Exploration: 30-minute budget on the card, one site per step, compare step writes options | Merged | fork #5 |
| 5 | Saved items: typed cards, collections, share switch, "Watch price", search tool | Merged | fork #6 |
| 6 | Images: Wikimedia Commons and preview images, the pond's copy, the credit | Merged | fork #7 |
| 7 | Lightpanda: aarch64 build against glibc 2.35, nano measurement, on-demand process | Needs a nano session | |
| 8 | POST forms confirmed on the phone; spoken alerts opt-in per member with quiet hours | Merged | fork #10 |
| 9 | Desktop screens: watches, saved collections, report pictures, options and source chips | Merged | fork #9 |
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
- The desktop doesn't yet know the `watch` screen, so it opens its default view (done in phase 9).

## Phase 3: phone screens, what landed

| Step | Status |
|---|---|
| Watches list and one watch (what changed, what is on the page, edit, pause, delete); a `watch` notice opens it | Done, on the phone |
| Saved cards by collection, one card whole, share switch, Watch price, delete | Done, on the phone |
| Report: pictures in their own shape with credit, option cards with Save and Watch price, citations as numbered chips | Done, on the phone |
| Chat answers carry the same chips and pictures (`POST /report-images/for-pages`, org #855) | Merged and deployed; checked on the phone |
| A source chip opens its page's card in place: title, site, address, picture, Open page, Close. One source opens in one tap | Done, checked on the phone |
| Pictures on conversation cards (across the head of a wall card, a thumbnail on a list row) from the first page a conversation cited or read (org #856) | Code and tests; needs #856 on the nano to show |

Not done in phase 3:
- The `form_confirm` sheet and the spoken-matches switch (phase 8's routes).
- A wall card's picture shows no credit; the credit is under the picture in the conversation.
- Asking for a watch should start it without asking for filters first; the model asked for price, mileage and interval in the live test.

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
- Saving an option card straight from a report: the phone and desktop send it with `POST /saved-items` (phase 3; the desktop's is done in phase 9).
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
- Drawing the pictures. The route needs the client's token, so a screen fetches each picture with it (a blob on the desktop, headers on the phone) instead of a bare `<img>`. Phone in phase 3; the desktop's is done in phase 9.
- The numbered source chips in the text. The report still cites `[title](link)`; the chips phase switches the composer to ledger numbers. The desktop draws chips from the links since phase 9.

## Phase 8: POST forms and spoken alerts, what landed

| Step | Kind | Status |
|---|---|---|
| A read page's POST forms are read like its GET forms. A form asking for a password, card, email, phone or file, or sending a file, is still left out | Code | Done |
| Table `web_form_submissions` (migration 0085), port in pond-core, SQLite in pond-infra. It lives in `pond_system.db`, so a form asked for in the voice child is answered through `serve`, and a restart keeps what waits | Code | Done |
| Asking `read_web_page` for a POST form's address with its fields sends nothing. The pond keeps the form and pushes `Send this form to {site}?` (`action: form_confirm`) to the speaker's own phones, as `book_ride` does | Code | Done |
| `GET /web-forms/{id}`, `POST /web-forms/{id}/confirm` and `/decline`, from the member's own paired phone only. On a yes the pond reads the form's page again for its fresh hidden values and cookies, posts it once through the tracked client, and keeps the answer as a page one click past the form's. Both requests are filed as `giap-web-forms` under the conversation that asked, never the turn in flight | Code | Done |
| A no, no answer within 15 minutes, a form gone from its page, or a lost answer: the next read of the form's address says which, and whether the site may have the form. A form is kept a day | Code | Done |
| A watch notice has a kind: match, trouble or digest. A match is also said aloud through the announcer when all of these hold: the member opted in (`web_watch_spoken_alerts` in their preferences, off by default); it is outside the household's quiet hours and the member's own (22:00 to 07:00 by default); the household's `unprompted_speech_enabled` is on; presence says they are home; no turn holds the model | Code | Done |
| `GET/PUT /web-watches/spoken-alerts`: the member's choice and quiet hours, beside the household's switch | Code | Done |
| A page's reply lists a POST form as "POST: sent only after the person says yes on their phone", without its hidden fields. The tool block is unchanged | Model | Done |
| Tests: pond-core 2,647, pond-mcp-server 393 (a stub site takes a confirmed form: fresh hidden value, the page's cookie, the answer kept), pond-infra 617, pond-api 343, pond-audio-out 38; route tests for the phone's confirm and the spoken-alerts switch. `cargo check -p pond-server --all-targets` passes on this Mac, with the target in a short path (`/private/tmp/...`) | Tests | Done |

Not done in phase 8:
- The phone. GOTG needs a sheet for the `form_confirm` notice (the site, the fields, Yes and No, on the three routes) and a switch for spoken matches with their quiet hours (phase 3). The desktop has no switch either: phase 9 merged before these routes.
- Presence counts a conversation from the member's phone, and a phone may be away from home. Until presence knows where a phone is, a match can be spoken while its member chats from elsewhere.
- A field the conversation left unset is not sent. A site that expects a select's default value may refuse such a form, and the outcome says so.
- Nothing has run on the nano or against live sites.
- One pond-api test fails on main, before this phase: `a_long_report_is_joined_from_its_sections_and_exports_three_ways` (`agent_suggestions_routes.rs`). Its drafts all share the Result's step and summary, so `report_parts` has dropped them as copies of the answer since `ee34a8ec`. The test or the rule needs a look of its own.

## Phase 9: desktop screens, what landed

All of it is in `pond-desktop`, against the routes phases 2, 5 and 6 added. No pond-server code changed.

| Step | Kind | Status |
|---|---|---|
| Client: watches (`GET /web-watches`, `GET/PATCH/DELETE /web-watches/{id}`), saved cards (`GET/POST /saved-items`, collections, `PATCH/DELETE /saved-items/{id}`, `POST /saved-items/{id}/watch`), report pictures as blobs. The report's `options` and `images` are read defensively, and a picture path must be `/api/v1/report-images/{hex key}` | Code | Done |
| Watches screen: the list (what it looks for, how often, item and match counts, failing checks); one watch with what changed (matches marked), what is on the page, and its words, price, year, mileage and interval to edit, pause and delete | Code | Done |
| Saved screen: collection chips (All, each one, Not in a collection); a face per kind (a recipe's size and counts, a listing's price first, a place's address and hours); a recipe's ingredients and numbered steps; Watch price on a listing; rename, move (or a new collection), the share switch and delete for the owner; another member's shared card is read-only | Code | Done |
| Report sheet: pictures fetched with the token and drawn from object URLs, credit and licence link under each; option cards with Save (as a listing) and Watch price (saves first); citations as numbered chips after their words, a tap opens title, site and link; the pond's Sources list folded into one collapsed "N sources" row | Code | Done |
| A `watch` notice opens the Watches screen at its `watch_id` (`householdNotices.ts`, `REQUEST_WATCH`) | Code | Done |
| Both shells: Saved and Watches in the drawer's Pond group and the classic section list; hub routes `saved` and `watches` | Code | Done |
| Tests: desktop 3,079 across 187 files; typecheck, lint and `vite build` clean | Tests | Done |

Not done in phase 9:
- Nothing has been looked at on screen or run against a live pond: only typecheck and component tests so far. The first look comes with the nano deploy, which also carries these screens to browsers and phones through the embedded web bundle (`npm run build` before the pond-server build).
- A listing's `photos` are not drawn. They are remote addresses, and loading them would tell the site who is looking. They need pond-kept copies, as report pictures have.
- A watch's items are not marked one by one as matching; the screen shows the pond's counts. Marking them would copy pond-core's filter rules into the desktop, or need the pond to send a flag per item.
- A report forgets that an option was saved once its sheet closes, so a second Save makes a second card.
- A `Saved { item_id }` notice screen: the pond does not send one yet.
- The chips follow the model's `[title](link)` words. Ledger numbers, with the line used and the date read in the chip's card, come with the chips phase.

## What waits on you

| What | Why |
|---|---|
| **Merge and deploy org #856, merge goose-on-the-go #272** | #856 gives each conversation its picture; the phone shows it once the nano runs it. Main through #855 is merged and deployed |
| Phase 7, Lightpanda on the nano | Building and measuring it runs on the nano beside the live pond, so it waits for your OK |
| Defaults: 30 days of change history, 20 watches and 500 cards per member, digest at 18:00, a turn's pause counted against the 30 minutes for now | Proposed in the design; building with them unless you say otherwise |
