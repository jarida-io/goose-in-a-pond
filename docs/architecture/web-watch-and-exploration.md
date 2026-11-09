# Web watches, exploration, saved items and source chips

Design decided with Jerry on 2026-11-09. Nothing here is built yet.

The member asks for something on the web, for example "find me a Subaru Forester under 1.5M". The
pond explores several sites, prepares options, and keeps watching the pages that matter. It tells
the member when something they care about changes. They can keep what it found as saved items,
and read its reports with images and with sources as chips.

## What exists today

| Piece | Where | State |
|---|---|---|
| Page reading | `read_web_page` (`crates/pond-mcp-server/src/web_page.rs`) | Fetches a page and returns Markdown: the lines that answer the question (1,800 bytes) or one 3,200-byte part. Caches 8 pages for 10 minutes. Checks neither robots.txt nor llms.txt |
| Search | `search_web` (`web_search.rs`) | Brave. Its terms (3(b)(i)) allow search results only transient storage |
| Provenance rule | `read_web_page` | Reads only addresses the person wrote, the task names, or a search returned. Follows no links |
| Agents and hooks | `role_block.rs`, `agent_activations.rs` | Schedule and event activations; after-hooks `NotifyOwner`, `SaveArtifact`, `StartTask`, `HandTo` |
| Notifications | `mcp/ports/notification.rs`, `docs/architecture/notifications.md` | Phone push through the companion app (GOTG), live on desktops. A tap opens one screen from the closed set `OpenScreen` |
| Long tasks | `long_task_runner.rs`, `long_task_roles.rs` | Worker gets 8 turns a step, with one or two page reads a step. Artifacts carry work between steps |
| Report sources | `research_report.rs` `sources()`, `pond-desktop/src/longtasks/ReportSheet.tsx` | Parsed from the `[title](url)` links the model writes, listed at the end |
| Remote images | `pond-desktop/src/components/MarkdownText.tsx` | Dropped on purpose, so a page can't learn who reads it (`MarkdownText.test.tsx`) |

## Decisions

| Area | Decision |
|---|---|
| Fetch tool | `read_web_page` grows; it is still the one reading tool. Watching is a new tool, `web_watch` |
| Site rules | A page the member named, or one the agent reads for them, is read as their own agent would read it. robots.txt and llms.txt don't apply. Data is kept only for the member's own watch or saved item |
| Script-built pages | Structured data first (JSON-LD, Next.js `__NEXT_DATA__` and App Router flight data, Nuxt `__NUXT_DATA__`), then Lightpanda. No other browser |
| Links | Same site, up to two clicks from an allowed page |
| Forms | GET forms are submitted freely. A POST form waits for a yes on the member's phone. A password, card or payment field is refused |
| Sign-in | Allowed later, once PAI-2 P9 (Pond Keychain) lands |
| Budget | Long tasks get 30 minutes, which the member can extend from the card. A chat turn reads up to 3 pages, then offers "explore more" as a long task |
| What changes | Items, not text: new, gone, price changed. Ads, dates and layout are ignored |
| Check interval | Every 6 hours by default; the member sets it per watch |
| Watch shape | A watch is its own record, checked in code. The model runs only on a match, and then the existing after-hooks run |
| When to tell | A match is pushed right away. Other changes go into one daily digest |
| Channels | Phone push, the in-app inbox, and live on desktops. Speaking aloud is opt-in per member, only when they are home and no conversation is running, never during quiet hours (22:00 to 07:00 by default, editable) |
| Saved items | Typed cards (recipe, listing, place, report) with fields and a Markdown body, in collections. They belong to the member who saved them, with a switch to share them with the household. A saved listing offers "Watch price" |
| Images | The pond fetches each image, keeps a size-capped copy and serves it itself, with its credit underneath. Images come from Wikimedia Commons (with licence and author) and from the preview images of pages read. They are picked by caption and alt text; the model doesn't look at them |
| Sources | A numbered chip after each sentence it supports. A tap opens a card: title, site icon, the line used, the date read and an Open link. A collapsed "N sources" row ends the report. The pond builds the chips from pages actually read, and drops any number that names a page never read |
| Surfaces | Phone first. The desktop keeps today's report sheet until its turn |

## Design

### Reading

- `web_page.rs` looks for structured data before it writes Markdown. When it finds items (a schema.org `ItemList`, `Car`, `Vehicle`, `Product` or `Offer`, or the page data of a Next.js or Nuxt app), it returns them as rows: title, price, mileage, link, image. A watch compares the same rows.
- A page with no readable text and no structured data goes to Lightpanda, which runs as its own process only for that page and then closes. It starts only when `MemAvailable` leaves the same 300 MB margin the embedder residency check uses. Otherwise the read says the page needs script and is put off until there is room.
- Every read is recorded in a per-task source ledger: address, title, site, the lines used, and when it was read. Chips are built from this ledger, and the link-following rule checks it.
- A Brave result set is never stored or watched. A watch points at a site's own page or filter address.

### Watches

- A new table holds each watch:
  - owner;
  - address;
  - interval;
  - filters, which the model writes at set-up and the member edits;
  - the last item set;
  - the changes it reported.

  Deleting a watch deletes all of it.
- The scheduler runs the checks. A check is a fetch and a comparison in code. It never claims the inference bus.
- A match wakes the model in the `Chore` lane to write one line for the notice, then runs the watch's after-hooks: notify the owner; save an item when asked; or start a long task, for example "compare the 3 new listings".
- Self-heal: on a 429 or 403 the pond waits longer between checks, twice as long each time, and tells the member after three failed checks in a row. A page whose items disappear entirely is reported as possibly moved, not as "all gone".
- New `OpenScreen` values, `Watch { watch_id }` and `Saved { item_id }`, so a tap opens the right screen. The phone learns them first.

### Exploration

- The planner plans one site per step and ends with a compare step that writes the options: one card per option, its fields, why it fits, and its sources.
- The card shows the time used out of 30 minutes, with an extend button. Steps still pause for a person's turn on the inference bus.
- POST forms reuse the confirm-on-your-phone pattern of `book_ride`.

### Saved items

- Kinds and their fields:

  | Kind | Fields |
  |---|---|
  | Recipe | servings, time, ingredients, steps |
  | Listing | price, currency, mileage, year, location, link, photos |
  | Place | address, hours, link |
  | Report | the report, its sources and images |
- Each item has a Markdown body and its sources, and sits in member-named collections.
- The assistant can search saved items through a tool when asked. They are not fed into memory.

### Reports

- The composer cites by ledger number, `[n]`. `research_report.rs` turns each number into a chip from the ledger, instead of parsing the model's links.
- Images are listed in the report's data, not as Markdown images, so the renderers keep dropping remote images. They are served from a pond route, and each copy keeps its credit and licence.

## Phases (one PR each)

1. **Reading:** structured data in `web_page.rs`, the source ledger, same-site links two clicks deep, GET filter addresses, and the chat cap of 3 reads with the "explore more" offer.
2. **Watches:** table and migration, the `web_watch` tool, scheduler checks, item comparison, filters, push on a match plus the daily digest, the new `OpenScreen` values, and back-off.
3. **Phone (GOTG):** the watch list and detail, the saved collections, and the report with chips and images.
4. **Exploration:** the time budget on the card, one site per step, and the compare step that writes options.
5. **Saved items:** kinds, collections, the share switch, "Watch price", and the search tool.
6. **Images:** Wikimedia Commons and preview images, the pond copy, and the credit.
7. **Lightpanda:** build for aarch64 against glibc 2.35, measure it on the nano, the on-demand process and its memory margin, and the licence notice.
8. **POST forms** confirmed on the phone, and **spoken alerts** as a per-member opt-in with quiet hours.
9. **Desktop:** the same screens in `pond-desktop`, and the web bundle from its build.
10. **Sign-in to sites**, after PAI-2 P9.

## Measurements owed, on the nano

- Lightpanda's memory and time per page. The only figure so far is about 49 MB for one static page, measured on x86_64.
- Time per read with structured data, against Markdown.
- How large a helper's transcript gets by its last turn. Needs the per-call helper trace line, which isn't built yet.

## Open, with proposed defaults

- How long a watch keeps its change history: proposed 30 days.
- How many watches each member may have: proposed 20.
- When the daily digest goes out: proposed 18:00, editable.
- Whether a pause for a person's turn counts against the 30 minutes: proposed no. Today a task's
  deadline is wall-clock time and keeps counting through a pause.
- Lightpanda is AGPL-3.0. The plan runs it unmodified as a separate program and links to its source. Any patch to it would have to be published.
