# Goose In A Pond: Comprehensive Agent Handoff

**Date:** November 12, 2026  
**Target Audience:** Incoming AI Coding Agents & System Engineers  
**Target Environment:** NVIDIA Jetson Orin Nano (8 GB Unified LPDDR5, ~7,620 MB usable ceiling) & Goose On The Go (Mobile/iOS)  
**Repository:** `goose-in-a-pond` (Branch: `main`)

---

## 0. Where things stand after 12 November 2026

This section is newer than the rest of the document and wins where they disagree. In
particular: the scratchpad template is named `scratchpad`, not `memory`; the soul, the
heartbeat and the scratchpad are now read and run, not only stored; and ideas are short cards.

### What landed, all on the fork's `main` and deployed to the nano

| Part | What is true now | Where to read |
|---|---|---|
| Tools in plain words | Schedules, reminders and timers take "every weekday at 7", "tomorrow at 6 pm"; a schedule is found by describing it; `world_clock` takes city names. Reminders never worked before: `schedule.rs` composed its to-do router without `#[tool_handler(router = self.tool_router)]`, so those tools were never listed | `crates/pond-mcp-server/src/schedule.rs`, `todo_tools.rs`, `tests/composed_routers_are_served.rs` |
| Memories around the person | A closed list of 22 words a fact is filed under, one write policy (`member_profile::admit`), one-at-a-time words that replace, a re-sort of older rows that files or archives and never deletes | [member-memory.md](member-memory.md) |
| Soul, heartbeat, scratchpad | The soul leads every prompt preset; the heartbeat runs hourly with its checklist and may add a reminder, write a note, pause a web watch that keeps failing or push a notice, within caps; Goose keeps lessons and follow-ups with `note_to_self` | [agent-documents.md](agent-documents.md) |
| One interview | Eight fixed sections asked in code, taps first, one follow-up at most, one model call only for typed words, nothing kept until approved | [my-goose-hub.md](my-goose-hub.md) section 0c |
| Ideas | A short card (kind, title, one line) with its own page (facts, steps, sources, Goose's notes). One everyday idea a run (recipe, outing, date or solo date, something to do, holiday) written against the profile; an idea naming an allergen is dropped in code | my-goose-hub.md section 6 |
| Cards sent to chat | `"card": {"kind","id"}` on `/chat/stream` for an idea, story, digest, to-do, watched link or web watch; the Pond reads the card as the speaker and the turn carries its content and Goose's notes | my-goose-hub.md section 6a |
| Screenshots, opt-in | Off by default. A screenshot waits in memory, is read once for one line, and is dropped; money, logins, private conversations and documents are skipped | my-goose-hub.md section 6b |
| Web watches | One rule read off the whole page: words appear or go away, a number goes below or above a value. Clock times and view counts are not updates | [web-watch-build-plan.md](web-watch-build-plan.md) phase 11 |
| A browser for script-built pages | A page that arrives as an empty shell is rendered once by a headless Chromium and read as usual, when a browser is found and memory allows | web-watch-build-plan.md phase 7 |

### Checked on the nano with Gemma 4 E4B

- `scripts/jetson/tools-live/tools_live.py`: 6 of 6. `scripts/jetson/memory-live/memory_live.py`: 6 of 6.
- The heartbeat ran with its checklist; nothing needed doing, and one impulse was refused by the daily cap.
- Ideas with the new prompts: three written in 85 seconds.
- `scripts/jetson/shots-live/shots_live.py`: the trail page became one line in 6 seconds, the bank screen was skipped; the vision tower (354 MB) loads on demand.

### Not done, and why

- **No browser is installed on the nano**, so script-built pages are not read there yet. A render was measured beside the loaded model: up to 307 MB and under 16 seconds a page, and the gate is 600 MB ([web-watch-build-plan.md](web-watch-build-plan.md) phase 7). `POST /api/v1/browser/install` downloads one when the household asks.
- No click-and-type browser agent, no feeds or replaced-file watches, no model judging whether a watch's change matters, no robots.txt.
- Screenshots are chosen by hand on the phone; syncing them automatically needs a native build.
- A guest's turn whose personal tools are withheld still ends as "I could not produce a response".
- The heartbeat pauses a web watch only when it has failed three checks in a row; it never starts or resumes one by itself.

### Traps found this week

- A tool that is not offered looks exactly like a model that will not call it. Read `GET /api/v1/agent/tools` before tuning a prompt.
- E4B picks a tool by its name, says it saved something it did not, and names a schedule by describing it.
- A phone paired by the test handshake is bound to nobody, so it is a guest; in a household of one it acts as that member and what it saves is shared with the household.
- Any change to a tool's words moves the pinned sizes in `prefix_oracle.rs` (now 20,890 and 21,692 bytes).
- The model wrote `Source: Page 1` as an idea's fact until the prompt forbade naming the pages and code dropped such facts.

## 1. Executive Summary & Work Delivered

Over the recent workstreams, we implemented, verified, and deployed four major architectural enhancements:

1. **Agent Soul, Heartbeat & Scratchpad Architecture**:
   - Introduced user-customizable, autonomous agent instruction templates:
     - `soul.md`: Defines the agent's domestic persona, sovereignty, concise voice, and local hardware awareness.
     - `heartbeat.md`: Defines autonomous periodic check-ins, routine inspections, and notification triggers to keep the human in the loop.
     - `memory.md`: Agent's internal scratchpad notes and self-directed agenda (distinct from user memories).
   - Fully wired into backend prompt endpoints (`/api/v1/prompts/{name}`) and verified in Goose On The Go mobile UI.

2. **Ideas State Machine & Completion Auto-Resolution**:
   - Diagnosed why ideas never finished in the app: `IdeaState::Done` existed in the domain model but was never set by any code in the repo.
   - Implemented automatic lifecycle resolution across `sqlite_ideas.rs`, `sqlite_long_task.rs`, and `crates/pond-api/src/ideas.rs`.

3. **Pond-Native Grounding for Long Tasks & Subagents**:
   - Fixed subagents (`Point Gander`, `Forager Gosling`, `Nestmaker Goose`) searching the web for external Python libraries named "Goose".
   - Grounded subagent recipes and idea generators in the real domestic MCP tools: `giap-memory`, `giap-weather`, `giap-schedule`, `giap-device`, `giap-sensors`, and `giap-vision`.

4. **Autonomous On-Device Model Self-Improvement Architecture (QLoRA & SFT Mining)**:
   - Designed an autonomous edge-training pipeline for the Jetson Orin Nano that trains small LoRA adapters ($r=8$, ~25 MB) during quiet hours (02:00–04:00) using ~4.5 GB peak VRAM.
   - Documented in detail in [`docs/architecture/on-device-lora-self-improvement.md`](on-device-lora-self-improvement.md).

5. **Live Jetson Orin Nano Deployment**:
   - Compiled native release binary with CUDA `sm_87` flags (`pond-adapters-local-inference/cuda`, `pond-adapters-whisper/cuda`).
   - Deployed, restarted systemd service, and confirmed healthy HTTP 200 on `http://127.0.0.1:8080/api/v1/health`.

---

## 2. Git Commits & Modified Files

### Recent Commit History
- `6db4570f`: `docs(architecture): add plan for on-device qlora and domestic sft mining`
- `ce1ae621`: `fix(api): resolve completed ideas and ground research in pond capabilities`
- `79ceb65c`: `feat(core): add soul, heartbeat, and scratchpad agent prompt presets`
- `2b28f9cd`: `feat(server, core, mcp): adversarial memory consolidation and structured memory presentation`
- `43d09bff`: `fix(inference): enrich background agents, expand output budgets, and unload idle whisper`

### Key Files & What They Do

| File Path | Purpose & Changes Made |
| :--- | :--- |
| `crates/pond-infra/src/sqlite_ideas.rs` | Added `sync_finished(&self, profile_id: &str)` to automatically resolve `working` ideas to `done` on read if their associated `long_tasks` or `todos` are complete. Added unit test. |
| `crates/pond-infra/src/sqlite_long_task.rs` | In `set_task_state`, automatically update `ideas SET state = 'done'` whenever a task reaches `Completed`, `Failed`, or `Cancelled`. |
| `crates/pond-api/src/ideas.rs` | In `goose()` handler, immediate actions (`GooseKind::Schedule`, `GooseKind::Watch`) now set `IdeaState::Done` upon creation. Enriched research prompt to anchor findings to Goose In A Pond. |
| `crates/pond-core/src/shared/services/long_task_roles.rs` | Rewrote system prompts for `Point Gander` (planner), `Forager Gosling` (researcher), and `Nestmaker Goose` (synthesizer) to clarify domestic identity and MCP capabilities. |
| `crates/pond-core/src/user_data/services/idea_generation.rs` | Grounded `IDEA_SYSTEM` prompt in `goose_can` capabilities (Matter devices, sensors, weather, calendar, memories). |
| `docs/architecture/on-device-lora-self-improvement.md` | Full architectural blueprint for domestic SFT data mining, prompt masking, and quiet-hours QLoRA training on the Orin Nano. |

---

## 3. Architecture Deep-Dive

### A. Ideas Lifecycle State Machine
Before our fix, ideas stayed `working` indefinitely. The lifecycle is now strictly synchronized:

```mermaid
stateDiagram-v2
    [*] --> Discovered: Idea Generator (Inference Lane)
    Discovered --> Dismissed: User dismisses idea
    Discovered --> Working: User taps action button
    
    state Working {
        [*] --> ActionBranch
        ActionBranch --> ScheduleDone: GooseKind::Schedule (Sets Done immediately)
        ActionBranch --> WatchDone: GooseKind::Watch (Sets Done immediately)
        ActionBranch --> ActiveTask: GooseKind::Research (Spawns LongTask)
        ActionBranch --> OpenTodos: GooseKind::Todo (Stages todo items)
        
        ActiveTask --> TaskDone: LongTask completes/fails/cancels
        OpenTodos --> TodosDone: User checks off all staged todos
    }
    
    ScheduleDone --> Done
    WatchDone --> Done
    TaskDone --> Done: sqlite_long_task hook / sync_finished()
    TodosDone --> Done: sync_finished() on read
```

### B. Agent Soul, Heartbeat & Scratchpad (The "Holy Trinity")
Stored in `prompt_templates` table in SQLite, seeded with defaults, and exposed via REST API:
- `GET /api/v1/prompts` — Lists all prompt templates.
- `GET /api/v1/prompts/{name}` — Reads active prompt.
- `PUT /api/v1/prompts/{name}` — Saves user custom prompt.
- `POST /api/v1/prompts/{name}/reset` — Reverts to factory builtin default.
- UI location in mobile app: **Settings / You -> Agent Instructions** tab.

### C. On-Device QLoRA Self-Improvement Architecture
```
Daytime Interactions:
  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
  │ Human Correction │  │ Tool Call Traces │  │ Overnight Soul   │
  │ (Barge-in / No)  │  │ (Matter/Sensors) │  │ Critique Pass    │
  └────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘
           │                     │                     │
           └─────────────────────┼─────────────────────┘
                                 ▼
           ┌───────────────────────────────────────────┐
           │ Dataset Miner: Redaction + Prompt Masking │
           │   (Target = Assistant Tokens, Loss = 1.0) │
           │   (Context = System/User, Loss = 0.0)     │
           └─────────────────────┬─────────────────────┘
                                 ▼
           ┌───────────────────────────────────────────┐
           │ 02:00 AM Inference Lane: llama.cpp LoRA   │
           │   (Batch=1, MaxTokens=1024, VRAM < 4.5GB) │
           └─────────────────────┬─────────────────────┘
                                 ▼
           ┌───────────────────────────────────────────┐
           │ Validation Gate -> Mount adapter_vX.gguf  │
           └───────────────────────────────────────────┘
```

---

## 4. Operational Invariants & Known Traps

> [!CAUTION]
> **Jetson SQLite Database Ownership (`agamemnon:agamemnon`)**:
> On the Jetson Orin Nano, the production database is at `/var/lib/goose-in-a-pond/pond_system.db`. It is owned by system user `agamemnon`.
> Running `sqlite3` as default SSH user `nano` will result in `sqlite3.OperationalError: attempt to write a readonly database`.
> **Always run queries with:**
> ```bash
> ssh nano "sudo -u agamemnon sqlite3 /var/lib/goose-in-a-pond/pond_system.db '<SQL>;'"
> ```

> [!IMPORTANT]
> **Hardware Ceiling vs Inference Lane Rules**:
> The Orin Nano has **7,620 MB usable unified memory**.
> - Never run heavy inference or background jobs concurrently with room voice or user turns.
> - Background chores (compaction, consolidation, embedding re-index, and fine-tuning) must strictly yield via the `inference_lane` mutex.
> - Do not enable MLX or PyTorch directly on the Jetson; inference is strictly CUDA-accelerated GGUF via `llama-cpp-2` / `goose-local-inference`.

> [!NOTE]
> **Deployment Commands**:
> Deploying to the Nano from the development Mac:
> ```bash
> bash scripts/jetson.sh deploy
> ```
> This script builds the web UI locally, rsyncs `dist/`, fetches `origin/main` on the Nano, runs an incremental CUDA release build (`sm_87`), restarts the systemd service, and verifies HTTP 200 health.

---

## 5. Suggested Next Horizons for the Incoming Agent

1. **Implement `DatasetMinerService` (Phase 1 of LoRA Plan)**:
   - Add interaction logging hooks in `ChatService` to tag turns with implicit positive/negative feedback signals.
   - Wire `redaction_chokepoints` to scrub PAKE tokens and Wi-Fi passwords prior to saving SFT pairs.
2. **Autonomous Heartbeat Notification Dispatcher**:
   - Currently, `heartbeat.md` is editable in the UI. Next, implement the autonomous scheduler runner in `inference_lane_runner.rs` that executes the heartbeat instructions on a timer and dispatches push notifications to the user when actionable home anomalies are detected.
3. **Idea Action Card UI Polish**:
   - In Goose On The Go, ensure the action button transitions smoothly from the active loading spinner to a clean "Done" / "View Schedule" badge when the idea state changes.
