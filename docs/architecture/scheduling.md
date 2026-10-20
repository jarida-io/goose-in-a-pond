# Scheduling System

Automation engine that executes LLM prompts or webhooks on recurring cron schedules, and fires sensor rules on matching EventBus traffic.

## Architecture

```
                ┌─────────────────────┐
                │  MCP Tools (12)     │ create, list, delete,
                │  REST API (8 routes)│ pause, resume, run-now,
                └────────┬────────────┘ get-runs, update, rules, clock
                         │
              ┌──────────▼──────────┐
              │   SchedulerPort     │  pond-core port trait
              │   (trait object)    │
              └──────────┬──────────┘
                         │
              ┌──────────▼──────────┐
              │ CronSchedulerAdapter│  pond-infra-scheduler
              │  tokio-cron-sched   │  JSON file persistence
              │  JsonRunHistory     │  run execution logs
              └──────────┬──────────┘
                         │ on cron fire
              ┌──────────▼──────────┐
              │  ScheduleExecutor   │  pond-core port trait
              │  (DeferredExecutor) │
              └──────────┬──────────┘
                         │
              ┌──────────▼──────────┐
              │AgentScheduleExecutor│  pond-server
              │  agent.chat()       │  ephemeral session
              │  Semaphore(N)       │  configurable concurrency
              └─────────────────────┘
```

## Domain Types (`pond-core/src/user_data/domain/schedule.rs`)

- **`TaskKind`** — `AgentPrompt { prompt }` (sends to LLM), `Webhook { webhook_url }` (HTTP POST), or `SensorTrigger(SensorTriggerSpec)` (#92 — fires on matching EventBus traffic rather than a cron cadence; `TaskKind::is_event_triggered()` tells the scheduler to skip cron registration, and the rules engine invokes it through `run_now`)
- **`SensorTriggerSpec`** — `source`, `condition` (numeric comparison plus a local-time window that wraps midnight and fails closed on malformed input), `actions`, `cooldown_secs` (default 60). Actions are `AgentPrompt`, `DevicePower { device_id, on }` and `Notify { title, body }`
- **`Schedule`** — id, label, cron (6-field), timezone (IANA), kind, paused, currently_running, last_run, next_run, created_at
- **`ScheduleRun`** — id, schedule_id, status (Running/Completed/Failed), result, error, started_at, finished_at, duration_ms
- **`ScheduleResultEvent`** — broadcast event for SSE delivery to desktop

## Cron Format

6-field: `<sec> <min> <hour> <day-of-month> <month> <day-of-week>`

Examples:
- `0 0 8 * * *` — daily at 8:00 AM
- `0 30 * * * *` — every hour at :30
- `0 0 9 * * 1` — every Monday at 9:00 AM

## Timezones (2026-10-13)

A schedule's cron is wall-clock time in its IANA `timezone`. Until 2026-10-13 only the displayed
`next_run` honoured it: the job was registered with `Job::new_async`, which reads the expression in
UTC, so the household's "Daily Brief" (`0 0 8 * * *`, Africa/Nairobi) promised 08:00 and ran at
11:00 Nairobi time every day. `Job::new_async_tz` would not have been enough: it freezes the zone's
UTC offset when the job is built, so a zone with DST would fire an hour off after each change.

A recurring task is now a chain of one-shot jobs (`RecurringJob` in
`pond-infra-scheduler/src/cron_scheduler.rs`). Each occurrence is resolved by `next_occurrence`,
with chrono-tz applying the offset in force on that date, and the same function computes
`next_run`, so the fire time and the card cannot disagree. A fire arms the next occurrence before
it runs, so a run that overruns the interval or fails does not end the chain. The expression is
parsed as `tokio-cron-scheduler` parsed it (six fields, seconds required; day-of-month AND
day-of-week), so stored schedules keep their meaning. Changing only a schedule's timezone
re-registers it. An unrecognised timezone runs on UTC, with a warning at registration.

Occurrences are wall-clock times, so the clock changes show. When clocks go back, the repeated
hour runs once, not twice: an hourly `0 30 * * * *` runs at the first 01:30 and then at 02:30.
When they go forward, a time inside the skipped hour runs when the clocks land (02:00 for a
01:30 schedule in London) and on its own time again the next day. A schedule that
speaks judges quiet hours in the household's `timezone` setting, the zone schedules default to,
not in the machine's.

## Resuming a paused one-shot (2026-10-19)

A one-shot stores the `"@once"` sentinel as its `cron` and its instant in `fire_at`. Until
2026-10-19 `resume_task` re-registered every paused schedule from its `cron`, so resuming a paused
one-shot asked the cron parser to read `"@once"`, failed ("invalid cron expression '@once'"), and
`POST /api/v1/schedules/{id}/resume` turned that into a 404 for a schedule that plainly existed.
Resume now tests `fire_at` before `cron`, as rehydration already did, and re-arms a one-shot at its
own instant.

If that instant passed while the schedule was paused, resume refuses with
`OneShotTimePassed` (`pond-core/src/user_data/domain/schedule.rs`), logged as
`kind="schedule_resume_refused"`, and the route answers **409** with
`"code": "one_shot_time_passed"` and an error that says to edit its time first. The schedule stays
paused and listed; a `PUT` with a new `fire_at` followed by a resume works. This is deliberately
different from startup, where an armed one-shot the pond slept through fires late: nobody asked
that one to wait, whereas a pause is the household saying "not at that time", and firing it hours
late on resume is not what was paused. A successful resume of either kind logs
`kind="schedule_resumed"`. Recurring schedules resume exactly as before, at their next occurrence.

### Creating a one-off over REST (2026-10-19)

`POST /api/v1/schedules` takes the same one-off fields `PUT` does. `fire_at` (an RFC 3339 instant)
makes a one-shot: the stored `cron` is `"@once"` and `cron` may be omitted from the body. `once: true`
with a `cron` makes a one-shot at that cron's next occurrence in the schedule's `timezone`, which is
what the phone app sends (a cron pinned to one date and time). `fire_at` wins if both are given. A
`fire_at` that has already passed is a **400** with `"code": "fire_at_in_past"`, logged as
`kind="schedule_create_refused"`, rather than a schedule that fires the moment it is created. Until
2026-10-19 the route dropped `fire_at`, so the only REST way to an exact instant was a `PUT` after
the create.

## Execution Flow

1. The task's one-shot job fires at its next local occurrence and arms the one after it
2. Job closure marks the task as `currently_running`
3. `JsonRunHistory::record_start()` creates a run record
4. `ScheduleExecutor::execute()` dispatches based on `TaskKind`:
   - **AgentPrompt**: creates ephemeral session (`sched-{id}-{timestamp}`), calls `agent.chat()`
   - **Webhook**: HTTP POST to the configured URL
5. `JsonRunHistory::record_finish()` records result/error + duration
6. `ScheduleResultEvent` broadcast via `tokio::sync::broadcast` channel
7. Task marked as not-running, `last_run` updated

### What an `AgentPrompt` run records (2026-10-13)

`agent.chat()` drains the same stream as interactive chat (`GooseAdapter::chat_stream`) and keeps
only its `Text` events: reasoning is not an answer, and tool calls go into the response metadata.
Each run gets its own engine session: the GIAP id is `sched-{id}-{unix_ts}`, and goose names the
session it mints for it after that id (`branch="created"` in the log). The `session_resumed`
prefix-cache invalidation that follows is logged for every new engine session, not a sign of reuse.

Goose hides the model's prose reply to its completeness check (row (b) of "the completeness check
must not reach the user" in `docs/goose-patch-management.md`). When a turn's earlier rounds were
all tool calls or reasoning alone, that hidden reply is the only answer the turn has. The
household's "Daily Brief" failed every morning in the week to 2026-10-12 with "Received empty
response from Goose agent" for this reason, on gemma-4-E2B: the weather call, a reasoning-only
round goose dropped, then the briefing as the hidden reply. The adapter now reads that reply back
from goose's session history when an attempt armed the check and streamed no text of its own, and
shows it (`kind="completeness_reply_shown"`). A turn that still has no text (tool calls only, as on
2026-10-10) fails with an error naming the tools it called, logged as `kind="turn_without_reply"`.

**A failed model call is a failed run (2026-10-20).** When the engine itself fails (llama.cpp's
"null result", LiteRT-LM's `engine_create failed`), goose does not return an error: it writes
"Ran into this error: ... Please retry if you think this is a transient or recoverable error." as
the turn's reply. The executor took that as the answer, so the Jetson recorded such runs
`completed` and notified the household with goose's prose. The provider shim now records the
typed `ProviderError` of each session's latest provider call (`SessionControls::provider_failure`,
cleared when a later call starts, as after a compaction and retry), and the adapter's `chat()`
turns a reply whose turn ended on one into an `Err` ("The model could not run: <error>"), logged
as `kind="turn_engine_failed"`. The scheduler records the run `failed` with that error, so its
notification says it failed; the executor logs `kind="schedule_prompt_failed"`. The streaming
chat path is unchanged: there the user still sees goose's text.

## Deferred Executor Pattern

The scheduler needs `Arc<dyn Agent>` but is created before the agent (circular dependency). Solution:

```
DeferredExecutor (empty OnceCell)
    → CronSchedulerAdapter::new(deferred_executor)
    → agent = build_goose_backend(scheduler.clone())
    → deferred_executor.init(AgentScheduleExecutor::new(agent))
```

## Natural Language Scheduling

The tool classifier routes "schedule to get weather at 10am every morning" → `create_schedule` tool → `parse_cron_from_message()` extracts:
- Time: "at 10 am" → hour=10
- Frequency: "every morning" → daily
- Prompt: "get weather" (stripped of scheduling words)
- Result: cron=`0 0 10 * * *`, prompt="Get weather"

## MCP Tools

| Tool | Description |
|------|-------------|
| `list_schedules` | List all with timezone, kind, status |
| `create_schedule` | Create from name + cron + prompt + timezone |
| `delete_schedule` | Delete by ID |
| `pause_schedule` | Pause (stops firing) |
| `resume_schedule` | Resume paused schedule |
| `run_schedule_now` | Trigger immediate execution |
| `get_schedule_runs` | Execution history with status + result |
| `update_schedule` | Modify an existing schedule in place |
| `create_sensor_rule` | Create an event-triggered rule (source, condition, actions, cooldown) |
| `list_sensor_rules` | List sensor rules |
| `delete_sensor_rule` | Delete a sensor rule by ID |
| `world_clock` | Current time in a given timezone |

Sensor rules have no dedicated REST surface today — they are created by POSTing a `SensorTrigger` kind to `/api/v1/schedules`, so the MCP tools above are the practical interface.

## REST API

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/schedules` | List all schedules |
| POST | `/api/v1/schedules` | Create (accepts name, cron, prompt, timezone, kind; a one-off with `fire_at` or `once`, see below) |
| GET | `/api/v1/schedules/upcoming` | Active schedules sorted by next fire |
| DELETE | `/api/v1/schedules/{id}` | Delete |
| POST | `/api/v1/schedules/{id}/pause` | Pause |
| POST | `/api/v1/schedules/{id}/resume` | Resume; 409 `one_shot_time_passed` for a one-shot whose time passed while paused |
| POST | `/api/v1/schedules/{id}/run-now` | Fire immediately (202 Accepted) |
| GET | `/api/v1/schedules/{id}/runs` | Execution history |
| GET | `/api/v1/schedules/events` | SSE stream of completion events |

## Persistence

- **Task definitions**: `$DATA_DIR/schedules.json` — survives restarts via rehydration
- **Run history**: `$DATA_DIR/schedule_runs.json` — bounded (configurable `schedule_max_runs_per_task`, default 50)

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `schedule_result_notify` | true | Broadcast completion events via SSE |
| `schedule_max_concurrent` | 2 | Max parallel scheduled task executions |
| `schedule_max_runs_per_task` | 50 | Max retained run history entries per schedule |

## Desktop UI

The Schedules section (`pond-desktop/src/sections/Schedules.tsx`) provides:
- Create modal with frequency presets, cron input, timezone selector, recipe presets
- Schedule cards with enable/disable toggle, run-now button, delete
- Expandable run history per card with status badges (completed/failed/running)
- Live SSE notifications when scheduled tasks complete

## Turns that reason but never reply (2026-10-14)

A scheduled prompt has no one watching it, so a turn that ends without text fails the run. On
2026-10-13 at 22:54 Nairobi time, "Run now" on the household's "Daily Brief" ("Give me a morning
briefing: weather, calendar, and top news.") failed this way on gemma-4-E2B with reasoning on
(`Re-stamped engine thinking flag ... enable_thinking=true` in the log). The engine session shows
the sequence: one round of reasoning (804 output tokens) ending in the weather call, a round of
reasoning alone (213 tokens), then goose's completeness check and a 15-token round with nothing in
it. Goose drops a round with no text and no tool call as empty (`empty_response` in the fork's
`agent.rs`), so nothing after the tool result was stored at all. The same run with Thinking set to
Off replied.

GIAP's empty-turn re-engagement did not run because it only covers attempts with no visible output,
and a tool call is visible output. The adapter now asks for a **final reply** instead
(`FinalReply` in `crates/pond-adapters-goose/src/goose_agent.rs`): when an attempt reasoned but
wrote no text, it sends one more message asking for the reply from what the turn already has, and
to name what it could not do rather than guess. For that one call the completeness check is off
and, on the `local`/`gguf` engines, `enable_thinking` is lifted to `false`, then restored before
the attempt is judged. It happens at most once a turn; a final reply that also writes nothing ends
the turn with the exhausted-retries message instead of re-asking. The user's Thinking setting is
not changed. It is logged as `kind="final_reply_asked"` and `kind="final_reply_result"`. While
reasoning is lifted, the adapter's record of the stamped flag is cleared, so a stream dropped
mid-reply leaves the next turn to re-stamp it.

What the "Daily Brief" can actually fetch, as of 2026-10-14:

- **Weather**: yes, `giap-weather`.
- **Calendar**: only through `giap-context`, and only when a member has connected a CalDAV
  account (`account_sync.rs` mirrors events into `context_items`). The Jetson has no context
  source connected, and `giap-context`'s group description in `tool_group.rs` does not mention
  calendars, so relevant-tool selection never offers it for a calendar question. `giap-schedule`
  is the pond's own schedules, not a calendar.
- **News**: no tool exists. `discovery.rs :: search_web` is deliberately unregistered, and
  `giap-knowledge` is reference lookup, not news.

The thinking-off run that did reply called only `get_current_time` and `get_current_weather`, yet
said the calendar "is currently empty as no events have been scheduled". That was never checked:
no calendar tool ran, and none could have. Until a calendar is connected and a news source exists,
the briefing prompt should ask only for what the pond can fetch.
