# Web watching and on-device browsing: research report (2026-11-12)

No files touched. Search queries were generic. Intervals, budgets and RAM targets marked "rec." are my recommendations, not sourced facts.

## Part 1 - General web watching

**What established tools support**
- changedetection.io (Apache-2.0): CSS / XPath 1.0 / JSONPath / jq filters, trigger / ignore / extract text, single-product "restock & price" with threshold and % alerts, browser steps, conditions, day/time scheduler, PDF text, and plain-English AI rules judged against "every detected diff" (local models supported). README use cases: prices, restocks, PDFs, government notices, releases and security advisories, festivals, real-estate listings, jobs, JSON APIs, legal documents ([README](https://github.com/dgtlmoon/changedetection.io)).
- Distill: monitor types Web page, PDF, JSON, Word, XML, Feed, Uptime, SiteMap; named uses are tickets, visa appointment dates, restocks ([docs](https://distill.io/docs/web-monitor/what-is-distill/)). Conditions test Text / Added / Previous / Deleted text with contains, regex, number less/more than, increased/decreased by amount or %, length, all/any ([docs](https://distill.io/docs/web-monitor/using-conditions-to-get-alert-on-important-changes/)).
- Visualping personal uses: back in stock, price drops, jobs, house hunting, grades, restaurant reservations, scholarships, travel, tickets; AI flags whether a change is important ([site](https://visualping.io/)). Wachete: whole page or part, login pages, 12-month history ([site](https://www.wachete.com/)).
- urlwatch filter chain: css, xpath, jq, pdf2text, ocr, ical2text, grep, sort, re.sub ([filters](https://urlwatch.readthedocs.io/en/latest/filters.html)). Huginn (MIT): weather, travel/shopping deals, peak detection, digests ([README](https://github.com/huginn/huginn)). RSS-Bridge: feeds for sites without one, ~447 bridges ([README](https://github.com/RSS-Bridge/rss-bridge)). Google Alerts: email when new search results match a topic ([help](https://support.google.com/alerts/answer/4815696)).

### a. Taxonomy (intervals rec.)

| Kind | Extract | Trigger | Interval | "Tell me when ..." |
|---|---|---|---|---|
| Price | price (structured data, else selector) | below X; drops N% | 30-60 min | "the kettle drops under 4,000" |
| Availability | stock state | out -> in | 5-15 min | "the bike is back in stock" |
| New list item | item keys (link + title) | key never seen | 15-60 min | "a new two-bedroom rental is listed" |
| New post | feed entry IDs | new ID | 30-60 min | "the school blog posts" |
| Keyword | page or section text | appears / disappears | 1-6 h | "'rationing' shows up on the county water page" |
| Section text | selector or main text | change after normalising | 6-24 h | "the bus timetable changes" |
| Number | one numeric field | <, >, change N% | 15-60 min | "the dollar passes 130" |
| Slot / date | list of dates | new or earlier date | 5-15 min | "an earlier appointment opens" |
| Status | status string | changes / equals X | 15-60 min | "my parcel is out for delivery" |
| File | ETag / Last-Modified / hash, PDF text | hash changes | 6-24 h | "the fee schedule PDF is replaced" |
| Release | release feed / version string | new version | 6-24 h | "new router firmware is out" |
| Deadline | date field | date changes; N days before | daily | "the bursary deadline moves, and 3 days before it" |

### b. Minimal data model
`watch = url + fetch_tier + extractor + condition + schedule + noise + notify`
- Extractors without an LLM: main text, CSS/XPath selector, feed entries, JSONPath, structured data, PDF text, HTTP validators/hash. With an LLM: a named field where no stable selector exists; relevance judgement of a diff.
- Conditions: any change; contains / not contains / regex; number <, >, delta, delta %; equals; new items as a set difference against all history, as changedetection.io's "Only trigger when unique lines appear" ([tutorial](https://changedetection.io/tutorial/how-monitor-website-new-content)).
- Operands, as Distill: current, added, removed, previous.

### c. Noise and politeness
- Watch only the stable part; deselect timestamps, counters, ads, widgets; wait for load; verify with two immediate checks that raise nothing ([Distill](https://distill.io/docs/web-monitor/eliminate-false-alerts/)).
- Normalise: strip, sort, dedupe, regex-remove volatile lines ([urlwatch](https://urlwatch.readthedocs.io/en/latest/filters.html)); alert on added lines only; "net added text is not empty" and a minimum added length against reordering and trivial edits (Distill, same page).
- Persist-for-two-checks: I found no tool naming this. Nearest: urlwatch's `max_tries` reports errors only after N consecutive failures ([jobs](https://urlwatch.readthedocs.io/en/latest/jobs.html)). Rec. for stock, status and number watches.
- Conditional requests: urlwatch uses ETag/Last-Modified unless `ignore_cached` (same page); a 304 avoids re-sending the resource ([MDN](https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/Conditional_requests)).
- Back off on 429/5xx, as crawlers do ([Google](https://developers.google.com/search/docs/crawling-indexing/http-network-errors)). Rec.: one request per host at a time, 10 s minimum spacing, jitter, exponential backoff.
- robots.txt: RFC 9309 addresses crawlers, says the rules "are not a form of access authorization", and caps caching at 24 h ([RFC](https://www.rfc-editor.org/rfc/rfc9309.html)); changedetection.io leaves compliance to the user. Rec.: a URL the user named is fetched as their agent; pages the assistant chooses itself honour robots.txt.
- A flat five minutes is aggressive (Visualping says it crawls "at a low frequency"). Use per-kind defaults.
- Alert fatigue: conditions, an importance flag (Visualping), digests (Huginn), one alert per state transition; rec. offer to pause a watch whose alerts go ignored.

### d. Spoken request to watch
- The LLM fills a closed schema (kind, URL or search query, extractor enum, condition, interval); code enforces floors, probes deterministic extractors (feed autodiscovery, structured data, main text), fetches twice to find volatile lines, then reads back "I'll watch X; it currently says Y" for confirmation.
- Diff deterministically on every fetch; call the LLM only on a normalised diff, with the user's intent, returning `{relevant, summary}` and holding no tools. That is changedetection.io's pattern. LLM-per-fetch is worth it only for LLM-extracted fields, and skipped when the main-text hash is unchanged.

## Part 2 - Autonomous browsing on the Orin Nano

| Option | Licence / runtime | aarch64, Ubuntu 22.04 | Memory | Notes |
|---|---|---|---|---|
| Chromium headless shell | BSD / none | Playwright supports Ubuntu 22.04 arm64 ([docs](https://playwright.dev/docs/intro)) and installs a separate headless shell with `--only-shell` ([docs](https://playwright.dev/docs/browsers)); Chrome for Testing publishes `linux-arm64` since v153.0.8001.0 ([repo](https://github.com/GoogleChromeLabs/chrome-for-testing)); Chrome arm64 .deb announced 2026-03-12, shipped by August ([announcement](https://blog.google/chromium/bringing-chrome-to-arm64-linux-devices/), [report](https://linuxiac.com/google-chrome-quietly-arrives-on-arm64-linux/)). apt `chromium-browser` is only a snap shim ([Ubuntu](https://packages.ubuntu.com/jammy/chromium-browser)); snap browsers broke on Jetson with snapd 2.70 ([NVIDIA forum](https://forums.developer.nvidia.com/t/chromium-other-browsers-not-working-after-flashing-or-updating-heres-why-and-quick-fix/338891)) | Idle with CDP port 382-388 MB PSS, 169-266 ms to CDP port (Chrome 156, x86_64 VM, Oct 2026) ([benchmark](https://computingforgeeks.com/install-lightpanda-headless-browser-linux/)) | Full JS and CSS |
| Lightpanda | AGPL-3.0 / none | arm64 binary needs glibc 2.38 ([README](https://github.com/lightpanda-io/browser)); Ubuntu 22.04 has 2.35 ([Ubuntu](https://packages.ubuntu.com/jammy/libc6)), so Docker arm64 image or source build | 17 MB idle PSS; 27-48 ms start (same benchmark) | No layout, paint or screenshots; CDP plus built-in MCP (`goto`, `markdown`, `links`, `fill`, `press`, `evaluate`) ([docs](https://lightpanda.io/docs/usage/mcp)); `--obey-robots`; telemetry on by default; a bot-protected site returned 403 |
| Servo | MPL-2.0 (from memory) / Rust crate | `servo` 0.1.0 on crates.io 2026-04-13 with LTS ([blog](https://servo.org/blog/2026/04/13/servo-0.1.0-release/)); Linux AArch64 nightlies ([book](https://book.servo.org/trying/getting-servoshell.html)) | not found | Not ready to rely on |
| Playwright MCP | Apache-2.0 / Node 18+ | via Playwright | Chromium + Node | Snapshot refs, `browser_find`, `--isolated`; states it "is not a security boundary" ([repo](https://github.com/microsoft/playwright-mcp)) |
| Chrome DevTools MCP | Apache-2.0 / Node LTS, current Chrome | - | Chromium + Node | `--slim`, `--headless`, `--isolated` ([repo](https://github.com/ChromeDevTools/chrome-devtools-mcp)) |
| browser-use 1.0 | MIT / Python 3.11+ | - | Chromium + Python | Raw CDP; `max_steps` default 100 ([docs](https://docs.browser-use.com/open-source/customize/agent/all-parameters.md)); `allowed_domains` is "a guard for an aligned model, not a sandbox" ([repo](https://github.com/browser-use/browser-use)) |
| Stagehand | MIT / TS, Python, Go | - | Chrome + runtime | `act`, `observe`, `extract` ([repo](https://github.com/browserbase/stagehand)) |

**Rust drivers**
- `chromiumoxide` 0.9.1 (2026-02-25): async, tokio only, MIT/Apache; README says the API "still lacks some functionality" with raw `Page::execute` as the escape hatch ([crates.io](https://crates.io/crates/chromiumoxide), [repo](https://github.com/mattsse/chromiumoxide)). Fork `chromey` releases often ([docs.rs](https://docs.rs/chromey)).
- `headless_chrome` 1.0.22 (2026-06-11): synchronous threads, MIT, 142 open issues, not Puppeteer-complete ([repo](https://github.com/rust-headless-chrome/rust-headless-chrome)).
- `fantoccini` 0.22.1 (2026-02-28): WebDriver, so a chromedriver process as well ([crates.io](https://crates.io/crates/fantoccini)).

**Observation format for small models**
- "Compact observations (accessibility trees) are preferable for lower-capability models"; long HTML raises hallucination; diff-based history saves tokens ([arXiv 2604.01535](https://arxiv.org/abs/2604.01535)).
- 42% of raw DOM snapshots exceed 128k tokens; a downsampled DOM matched a screenshot baseline (73% vs 67%, not significant) ([arXiv 2508.04412](https://arxiv.org/abs/2508.04412)).
- Playwright MCP: snapshots are text, need no vision model, and are narrowed with `target`, `depth`, `browser_find` ([docs](https://playwright.dev/mcp/snapshots)).
- Rec.: numbered interactive elements plus trimmed main text, about 1.5-2k tokens per observation, no screenshots, no raw HTML.

**Safety**
- Page instructions can override the user's; precautions: allowlist of domains, no login data, human confirmation for consequential actions including cookie consent, payments and terms ([Anthropic docs](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)). Frontier models with safeguards still showed 1.4-2.4% attack success; "no browser agent is immune" ([Anthropic, 2025-11-24](https://www.anthropic.com/research/prompt-injection-defenses)).
- Keep user instructions and page content separate ([Brave, 2025-10-21](https://brave.com/blog/unseeable-prompt-injections/)); never combine private data, untrusted content and outbound communication ([lethal trifecta](https://simonwillison.net/2025/Jun/16/the-lethal-trifecta/)); least privilege and human approval ([OWASP LLM01](https://genai.owasp.org/llmrisk/llm01-prompt-injection/)).
- Ladder precedent: urlwatch calls browser jobs resource-intensive, for use only when a plain URL job fails.

### Recommendation (rec.)
1. **Tier 0**: existing HTTP fetch, with conditional requests.
2. **Tier 1, render only**: Chromium headless shell (Playwright's arm64 build or Chrome for Testing, never the snap) driven in-process by `chromiumoxide`: open, wait, serialise DOM into the existing extractor. No LLM. Triggered when Tier 0 text is empty or script-gated.
3. **Tier 2, interactive**: same browser; tools `open`, `snapshot`, `click(ref)`, `type(ref, text)`, `select`, `scroll`, `back`, `extract`, `done`. Cap 12-15 steps, 120 s, one tab.
- Launch on demand, fresh profile, images/fonts/media blocked, sandbox kept on; kill after the task or 60 s idle; never beside a chat turn. Budget 400-600 MB until measured on the board, which sits at the top of the stated allowance.
- Skip Node and Python stacks; borrow their designs. Lightpanda is a later Tier 1 fast path once glibc, AGPL and telemetry are settled.
- Guards: no credentials or payments; every submit, consent click and download needs in-app confirmation; text types only with a size cap; domain scope is the start site plus user-approved hosts.
- **Autonomous fetching** for background jobs means read-only Tiers 0-1 inside budgets (pages per job, per-host rate, daily cap, bytes), robots.txt honoured for self-chosen pages, every fetch in the activity log, and fetched text summarised by a call that holds no tools. Tier 2 runs only for a task the user asked for, or as recorded steps a user approved for a watch.

## Could not verify
- Chromium memory and cold start on aarch64 or the Orin Nano (only x86_64 figures found); per-tab cost.
- Whether Chrome for Testing arm64 or the Chrome .deb runs on Ubuntu 22.04 / glibc 2.35 under JetPack 6; whether snap Chromium is fixed on current JetPack 6.x (forum reports conflict).
- changedetection.io: latest version, default interval, ETag support, whether AI rules run strictly on change.
- Lightpanda Web API coverage and real-site success; its memory on aarch64.
- Servo headless/WebDriver readiness and licence (not re-checked); no Servo-based automation product found.
- Stagehand's underlying driver; browser-use 1.0's default observation format; first-party per-snapshot token counts for Playwright MCP.
- Visualping, Distill, Wachete and Browse AI interval tiers; Browse AI monitors (search snippets only); Google Alerts frequency labels; Huginn agent names.
- Published step-cap guidance for small local models: none found.
