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
| 7 | A browser for pages that scripts build: Chromium's headless shell, run once a page, on demand (Lightpanda set aside: its arm64 binary needs glibc 2.38) | Merged; the nano measurement waits for you | fork #19 |
| 8 | POST forms confirmed on the phone; spoken alerts opt-in per member with quiet hours | Merged | fork #10 |
| 9 | Desktop screens: watches, saved collections, report pictures, options and source chips | Merged | fork #9 |
| 10 | Site sign-in, after PAI-2 P9 (Pond Keychain) | Waits for P9 | |
| 11 | General watches: tell me when words appear or go away, or a number goes below or above a value; passing changes (clock times, "3 hours ago", view counts) no longer count as updates; a watch as a card sent to chat | Merged | fork #18 |

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
| Pictures on conversation cards (across the head of a wall card with three lines of preview, a thumbnail on a list row): the first picture sent in the conversation, else the first page it cited or read (org #856) | Code and tests; previewed on the phone by hand, needs #856 on the nano to show for real |

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

## Phase 7: reading pages that need a browser, what landed

Some pages arrive as an empty shell that scripts fill in. Every read (the page tool, watches,
the news, ideas, research) goes through one fetch; that fetch now hands such a shell to a
headless browser once and reads what the browser built with the usual reader.

| Step | Kind | Status |
|---|---|---|
| A shell is a page with under 200 characters to read, scripts, and a mark that the scripts build it (`id="root"`, `__next`, "enable JavaScript", ...). A page with words is never sent to a browser | Code | Done |
| One render: `--headless --dump-dom` with 8 s of virtual time for the page's scripts, 30 s at most, pictures and sound off, a fresh profile deleted afterwards, incognito, the browser's own sandbox on. One at a time; a second page is told to try again | Code | Done |
| The browser is the pond's own download if there is one, else `chrome-headless-shell`, Chromium, Chrome or Edge found on the machine. No setup: it is looked for when a page needs it | Code | Done |
| Memory gate: on a board that reports free memory, a browser starts only with 600 MB available, set from the measurement below. The refusal says how much was free | Code | Done |
| `POST /browser/install` downloads the stable headless shell for the machine from Chrome for Testing (about 130 MB) into `<data_dir>/browser`, unpacks it and checks that it runs; `GET /browser` shows the browser in use, whether a page could be rendered now and why not, and the download's progress; `DELETE /browser` removes the pond's own | Code | Done |
| Household setting `browser_rendering_enabled` (on by default), in the desktop's Extensions & Tools; applied at once | Code | Done |
| A shell that was not rendered is a failed read whose reason the model is given in words: turned off, no browser yet, too little memory free, busy, or the browser could not read it | Code | Done |
| A real page built by a script is rendered by the installed Chrome in the tests on a Mac (under a second); the test is skipped where no browser is installed | Tests | Done |
| The headless shell for `linux-arm64` (157.0.8103.78) starts on the nano's glibc 2.35 with no library missing | Measured on the nano | Done |

### What a render costs on the nano

Measured on the Orin Nano on 12 November 2026 with `scripts/jetson/render-cost/render_cost.sh`,
beside the running pond with the chat model loaded (6.4 GB in use, about 900 MB available),
using the pond's own flags and Chrome for Testing's headless shell 157.0.8103.78. Each render
ran in its own memory-capped scope; memory was sampled ten times a second.

| Page | Cap | Seconds | Peak memory | Lowest available memory |
|---|---|---|---|---|
| example.com | 700 MB | 2.8 | 307 MB | 830 MB |
| react.dev/learn | 700 MB | 6.3 | 233 MB | 705 MB |
| jumia.co.ke | 700 MB | 12.9 | 235 MB | 679 MB |
| en.wikipedia.org/wiki/Kenya | 700 MB | 4.2 | 161 MB | 740 MB |
| youtube.com | 700 MB | 9.4 | 266 MB | 651 MB |
| youtube.com | 300 MB | 8.1 | 280 MB | 658 MB |
| jumia.co.ke | 300 MB | 15.4 | 246 MB | 656 MB |
| youtube.com | 200 MB | stopped at 30 s, nothing read | 199 MB | 690 MB |

Available memory stood between 900 and 916 MB before each batch, so a render took at most 265 MB
of it.

- A render fits in about 300 MB, and still completes when held to 300 MB. Held to 200 MB the
  browser does not finish within the 30 seconds the pond allows.
- The pond stayed healthy throughout and nothing was killed.
- The gate is therefore 600 MB: about the most a render took (307 MB), with 300 MB left for the pond. With
  the picture encoder also loaded the board had 498 MB available, so the pond still refuses
  then, and renders when the encoder is not held.
- The peak is a sample every tenth of a second, so a shorter spike could be higher.

### On the nano

The household asked for the download on 12 November 2026. `POST /browser/install` fetched the
headless shell (128 MB) into `/var/lib/goose-in-a-pond/browser`, unpacked it, checked that it
runs, and `GET /browser` answered `ready` with 685 MB free against the 600 MB it needs.

`scripts/jetson/render-live/render_live.py` then asked the pond, in a conversation with
Gemma 4 E4B, to read a page that arrives as an empty shell (Hacker News search). The pond's
log shows the one render: `web_page_rendered ok=true bytes=42524 ms=9876`. The turn took 42
seconds, the pond stayed healthy, and 715 MB was available afterwards. The answer was drawn
from the rendered page but was thin: asked for the first three story titles, the model quoted
one passage and said it could find no more. What the reader picks out of a rendered list for
a question is the next thing to look at; the browser did its part.

Not done in phase 7:
- No browser is downloaded on its own: it is 130 MB, so the household starts it. The phone has no button for it yet; the route is there. A newer build of the browser is not fetched either; `DELETE /browser` then `POST /browser/install` replaces it.
- Clicking, typing and signing in with a browser are not built. The pond already follows same-site links, sends search forms, and sends other forms after a yes on the phone; a click-driven browser for a small model needs the memory answer first, and its tools would sit in every turn's prompt unless they are a group that is off by default.
- The browser does its own networking, outside `pond-net`. It is only ever given an address the pond's own fetch already read from the public web, but scripts on that page make their own requests.
- robots.txt is not read, and fetches are not spaced per site beyond each job's own limits.

## Phase 11: general watches, what landed

A watch was a listing's: new items, updates and price drops among the items a page lists. It
now also takes one rule read off the whole page, in code, with no model in a check.

| Step | Kind | Status |
|---|---|---|
| `TellWhen` on a watch's filters: `new_or_changed` (default), `appears {phrase}`, `disappears {phrase}`, `below {phrase, value}`, `above {phrase, value}`. Stored in the filters' JSON, so no migration and older watches read as the default | Code | Done |
| A rule reports only itself, once, when it comes true between two checks: the phrase was absent and is there, was there and is gone, or the number crossed the value. Nothing else on that page is kept for the digest | Code | Done |
| The number is the one written within 60 characters after the phrase; with no phrase, the page's first price | Code | Done |
| A section whose words did not change is not an update: clock times, "3 hours ago", "just now" and counts of views, likes and comments are set aside before comparing. A version number still counts | Code | Done |
| `web_watch` tool: `when`, `phrase`, `value`. Setting a rule up says how the page stands now ("USD/KES is 129.4 now"), and refuses a number rule whose number is not on the page | Code | Done |
| Routes: a watch's view carries `filters.tell_when`, the rule in words (`filters_words`) and `standing`; `PATCH /web-watches/{id}` takes the rule in `filters` | Code | Done |
| A watch is a card that can be sent to chat (`"card": {"kind": "watch", "id"}`), with what it tells about, how the page stood and its recent changes as Goose's notes | Code | Done |
| The tool block is 226 bytes larger (20,664 to 20,890) for the three fields | Measured | Done |

Not done in phase 11:
- Feeds (RSS, Atom) are not read as entries; a blog's own page is watched through its sections.
- A file that is replaced (a PDF) is not watched: only pages the reader turns into text.
- No model judges whether a change matters. Every check is code, so "tell me when the timetable changes in a way that affects mornings" is not possible yet; the rules above are.
- A change is not confirmed by a second read, so a page that shows different sections on every visit can report updates. Its owner can switch that watch to a rule.
- Checks run no more often than hourly, as before.
- The phone and desktop screens do not offer the rule yet; it is set from a conversation.

## What waits on you

| What | Why |
|---|---|
| **Merge and deploy org #856, merge goose-on-the-go #272** | #856 gives each conversation its picture; the phone shows it once the nano runs it. Main through #855 is merged and deployed |
| Phase 7, Lightpanda on the nano | Building and measuring it runs on the nano beside the live pond, so it waits for your OK |
| Defaults: 30 days of change history, 20 watches and 500 cards per member, digest at 18:00, a turn's pause counted against the 30 minutes for now | Proposed in the design; building with them unless you say otherwise |
