# Scheduling System

Runs agent prompts and webhooks on a recurring cron or once at a given instant, and fires sensor
rules when matching events cross the event bus.

## Architecture

```
  giap-schedule MCP tools (5)      REST /api/v1/schedules, /api/v1/rules      desktop Schedules
                 │                               │                                   │
                 └───────────────────────────────┼───────────────────────────────────┘
                                                 ▼
                                SchedulerPort (pond-core, ports/scheduler.rs)
                                                 │
                     CronSchedulerAdapter (pond-infra-scheduler, cron_scheduler.rs)
                       schedules.json            task definitions, rebuilt on start
                       schedule_runs.json        JsonRunHistory
                       RecurringJob / one-shot   timers on tokio-cron-scheduler
                     run_rules_engine (rules_engine.rs): EventBus event ──> run_now(rule)
                                                 │ fire
                                ScheduleExecutor port (pond-core)
                       DeferredExecutor ──> AgentScheduleExecutor (pond-server, schedule_executors.rs)
                         Semaphore(schedule_max_concurrent)
                         AgentPrompt   agent.chat() in session sched-{id}-{unix_ts}
                         Webhook       HTTP POST through the egress gate
                         SensorTrigger each of the rule's actions
                         MemberReminder agent.chat() as the member, reply to their own phones
                                                 │
                       ScheduleResultEvent broadcast ──> SSE /api/v1/schedules/events
                                                    └──> push notifications
```

## Domain types (`pond-core/src/user_data/domain/schedule.rs`)

- **`TaskKind`**: `AgentPrompt { prompt }`, `Webhook { webhook_url }`,
  `SensorTrigger(SensorTriggerSpec)`, or `MemberReminder { profile_id, prompt }`.
  `TaskKind::is_event_triggered()` is true for a sensor rule, which gets no timer; the rules engine
  fires it through `run_now`. `TaskKind::member()` names a private reminder's member, and
  `ScheduleResultEvent::for_member` carries it on every run event.
- **`SensorTriggerSpec`**: `source` (`TriggerSource`: kind `Sensor` / `Camera` / `Device`, optional
  `device_id` and `signal`; unset fields match anything), `condition` (`TriggerCondition`: an
  optional numeric comparison `op` + `value`, and an optional `after` / `before` local-time window
  that wraps midnight; a malformed bound fails closed), `actions`, and `cooldown_secs` (default
  60). `validate()` refuses a rule that could never work (`RuleRejection`: no actions, a malformed
  time bound, half a comparison, an empty filter or action).
- **`TriggerAction`**: `AgentPrompt { prompt }`, `DevicePower { device_id, on }`,
  `Notify { title, body }`.
- **`Schedule`**: id, label, cron, fire_at, timezone (IANA), kind, paused, currently_running,
  last_run, next_run, created_at. `cron` holds the sentinel `"@once"` (`CRON_ONCE`) for a one-shot
  and `"@event"` for a sensor rule; neither is ever parsed.
- **`ScheduleRun`**: id, schedule_id, status (`Running` / `Completed` / `Failed`), result, error,
  started_at, finished_at, duration_ms.
- **`ScheduleResultEvent`**: broadcast when a run starts and when it finishes.
- **`OneShotTimePassed`**: the error for resuming a paused one-shot whose instant has passed.

## Cron format

Six fields, seconds required: `<sec> <min> <hour> <day-of-month> <month> <day-of-week>`. When both
day fields are set, both must match (day-of-month AND day-of-week).

- `0 0 8 * * *`: daily at 08:00
- `0 30 * * * *`: every hour at :30
- `0 0 9 * * 1`: Mondays at 09:00

## Time zones

A schedule's cron is wall-clock time in its IANA `timezone`. A recurring task is a chain of
one-shot jobs (`RecurringJob` in `cron_scheduler.rs`), each armed for its next occurrence by
`next_occurrence`, which applies the offset chrono-tz has in force on that date. The same function
computes `next_run`, so the fire time and the card cannot disagree. `Job::new_async` would read
the cron in UTC, and `Job::new_async_tz` would freeze the zone's offset at build time and drift an
hour after each DST change, which is why neither is used.

A fire arms the next occurrence before it runs, so a run that overruns its interval or fails does
not end the chain. Occurrences missed while the machine slept collapse into one late fire.
Changing only a schedule's timezone re-registers it. An unrecognised zone runs on UTC, with a
warning at registration.

Occurrences are wall-clock times, so clock changes show. When clocks go back, the repeated hour
runs once: an hourly `0 30 * * * *` runs at the first 01:30 and then at 02:30. When they go
forward, a time inside the skipped hour runs when the clocks land (02:00 for a 01:30 schedule in
London) and on its own time again the next day.

REST creates default to `UTC` when no timezone is given; MCP creates default to the household's
`timezone` setting.

## One-shots

A one-shot stores `fire_at` (an instant) and the `"@once"` cron. It fires once, then is deleted, so
spent one-shots do not clutter `list_schedules`. Ways to make one:

- `set_timer` (MCP): a natural-language delay ("10 minutes", "1h30m").
- `POST /api/v1/schedules` with `fire_at` (RFC 3339; `cron` may be omitted), or with `once: true`
  and a `cron`, which becomes the cron's next occurrence in the schedule's `timezone` (what the
  phone app sends). `fire_at` wins if both are given. A `fire_at` already past is a **400** with
  `"code": "fire_at_in_past"`, logged as `kind="schedule_create_refused"`.
- `PUT /api/v1/schedules/{id}` with `fire_at` or `once`. A real `cron` alone makes the schedule
  recurring again and clears `fire_at`.

At startup an armed one-shot whose instant passed while the pond was down fires late: nobody asked
it to wait. Resuming a **paused** one-shot whose instant passed is different, because a pause is
the household saying "not at that time": `resume_task` refuses with `OneShotTimePassed`, logged as
`kind="schedule_resume_refused"`, and `POST /api/v1/schedules/{id}/resume` answers **409** with
`"code": "one_shot_time_passed"`. The schedule stays paused and listed; a `PUT` with a new
`fire_at`, then a resume, works. Resume and rehydration both test `fire_at` before `cron`, since no
cron parser reads `"@once"`. A successful resume logs `kind="schedule_resumed"`; a recurring
schedule resumes at its next occurrence.

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
chat path is unchanged: there the user still sees goose's text. A run that speaks its reply
(`voice_mode`, added on main after this change) streams it into the audio bus, and that path
does not see the provider failure yet: such a run is still recorded `completed`, and the error
text is spoken.

## Execution flow

1. The task's job fires (or `run_now` is called) and marks the task `currently_running`.
2. `stamp_fire` sets `last_run` to the fire time. It is written to disk at once only for a rule
   with a cooldown (`durable_fire_stamp`), which reads it back after a restart; other stamps ride
   along on the next save.
3. `JsonRunHistory::record_start()` creates a run record and a `Running` event is broadcast.
4. `ScheduleExecutor::execute()` runs the kind, holding a permit from a semaphore of
   `schedule_max_concurrent` so scheduled work cannot starve interactive chat:
   - **AgentPrompt**: creates session `sched-{id}-{unix_ts}` and calls `agent.chat()` with
     `ProfileScope::Household` and no profile context (schedules have no owner).
   - **Webhook**: `POST` to the URL, checked and recorded by the egress gate
     (`EgressPurpose::Schedules`); a non-2xx status fails the run.
   - **SensorTrigger**: runs every action; the run fails only when no action succeeded.
   - **MemberReminder**: see [A member's reminder](#a-members-reminder).
5. `JsonRunHistory::record_finish()` records the result or error and duration, and a
   `Completed` / `Failed` event is broadcast.
6. The task is marked not running.

`sched-` sessions are pond-authored (`POND_AUTHORED_SESSION_PREFIXES`): they are never mined for
memories and never count as user activity for the background jobs.

### Speaking a result

An `AgentPrompt` result is spoken aloud when the pond has voice output,
`unprompted_speech_enabled` is on, and the current time, in the household's `timezone` setting,
is outside `quiet_hours_start` to `quiet_hours_end` (`AgentScheduleExecutor::speakable_now`).
There is no per-member presence check.

### A member's reminder

A reminder set with `private` (on `set_timer` or `create_schedule`) is a
`MemberReminder` for the person speaking, resolved from the call's session as the travel tools
resolve it. A speaker who can't be told apart gets nothing set and is asked whether it may be a
household reminder. When it fires, `AgentScheduleExecutor` runs the prompt with
`ProfileScope::Owner(member)` and without voice, sends the reply to that member's own devices
through `MemberNotifier` (category `action_required`), and, under the same gate as above, says
only "Grace, you have a reminder. I've sent it to your phone." Nothing that reaches the household
carries what it says:

- the run's result, which the run history, the SSE stream and the dashboard show, is "Sent to
  Grace's phone", and an undelivered reminder is a failed run that does not quote it;
- the household's "task finished" notice is skipped (`ResultSilence::MembersOwn`);
- `GET /schedules`, `GET /schedules/upcoming`, the `PUT` answer, `list_schedules` and the upcoming
  block in the system prompt show its label and time but not its prompt.

Editing its prompt keeps it the member's (`TaskKind::with_prompt`). `POST /schedules` refuses a
`member_reminder` kind, because that route can't tell who is asking.

### A turn with no reply

`agent.chat()` drains the same stream as interactive chat and keeps only its text
(`collect_reply` in `pond-adapters-goose/src/goose_agent.rs`): reasoning is not an answer, and
tool calls go into the response metadata. A scheduled prompt has no one watching it, so two
recoveries in the stream decide whether it gets an answer:

- Goose hides the model's reply to its completeness check (see
  [goose-patch-management.md](../goose-patch-management.md)). When an attempt armed the check and
  streamed no text of its own, the adapter reads that reply back from goose's session and shows it
  (`kind="completeness_reply_shown"`).
- When an attempt reasoned but wrote no text (for example a tool call followed by a round of
  reasoning alone, which goose drops as empty), `FinalReply` asks once for the reply from what the
  turn already has, naming anything it could not do. For that call the completeness check is off
  and, on the `local` / `gguf` engines, reasoning is switched off, then restored. It is logged as
  `kind="final_reply_asked"` and `kind="final_reply_result"`. The user's Thinking setting is not
  changed.

A turn that still has no text fails the run with an error naming the tools it called, logged as
`kind="turn_without_reply"`.

## Deferred executor

The scheduler is built before the agent, but the executor needs the agent, so `pond-server`
passes the scheduler a `DeferredExecutor` (an empty `OnceCell`):

```
deferred = DeferredExecutor::new()
scheduler = CronSchedulerAdapter::with_options(schedules.json, schedule_runs.json, deferred, ...)
agent = (built with the scheduler)
deferred.init(AgentScheduleExecutor::new(agent, ...))
```

A task that fires before `init` fails with "schedule executor not yet initialized".

## Sensor rules

`run_rules_engine` (`pond-infra-scheduler/src/rules_engine.rs`) subscribes to the event bus. For
each event with a trigger view (sensor, camera and device events; clock ticks and session changes
match nothing) it fires every unpaused rule whose source and condition match, through `run_now`,
so rule fires get the same run records and broadcasts as cron fires. A rule is debounced by
`cooldown_secs` against the later of its in-memory fire time and the persisted `last_run`. The rule
list is cached for 5 s. Time windows are judged on the server's local wall clock, not the
household `timezone` setting.

Rules are managed over REST (`/api/v1/rules`), which validates the spec and never reaches a cron
schedule by id. `POST /api/v1/schedules` with a `SensorTrigger` kind is validated the same way.
There is no MCP tool for rules.

## MCP tools (`giap-schedule`, `pond-mcp-server/src/schedule.rs`)

| Tool | Description |
|------|-------------|
| `list_schedules` | Every schedule with cron, timezone, kind and status; rules are summarised |
| `set_timer` | One-shot after a natural-language delay; deletes itself after firing. `private` makes it the speaker's own reminder |
| `create_schedule` | Create a recurring `AgentPrompt` schedule, or with `private` the speaker's own reminder; with an existing `id`, update it instead (omitted fields keep their value) |
| `schedule_action` | `delete`, `pause`, `resume` or `run_now` by id |
| `world_clock` | Current time in one or more IANA zones; defaults to the household's |

`create_schedule` resolves the cron in this order: parameters generated by the optional tool-model
specialist (only when the `tool_model` setting names one, in builds with `local-inference`); the
model's `cron` argument, if it is six fields of digits and `* / - ,`; then
`parse_cron_from_message` over the last user message ("every morning at 9" becomes
`0 0 9 * * *`). If none yields a cron, the tool asks the model to retry with the field order
rather than inventing a time. On update, a `cron` argument is first read as natural language,
then as a literal.

Upcoming schedules are not injected into turn context; the model calls `list_schedules`.

## REST API

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/schedules` | List all schedules (rules included) |
| POST | `/api/v1/schedules` | Create: `name` (or `label`), `cron`, `prompt` or `kind` (or legacy `payload`), `timezone`, `fire_at`, `once`; 201 |
| GET | `/api/v1/schedules/upcoming` | Active schedules by next fire (`limit`, default 10) |
| PUT | `/api/v1/schedules/{id}` | Update `name`, `cron`, `timezone`, `prompt`, `fire_at`, `once` |
| DELETE | `/api/v1/schedules/{id}` | Delete |
| POST | `/api/v1/schedules/{id}/pause` | Pause |
| POST | `/api/v1/schedules/{id}/resume` | Resume; 409 `one_shot_time_passed` for a one-shot whose instant passed while paused |
| POST | `/api/v1/schedules/{id}/run-now` | Fire now; 202 |
| GET | `/api/v1/schedules/{id}/runs` | Run history (`limit`, default 10) |
| GET | `/api/v1/schedules/events` | SSE stream of `ScheduleResultEvent`s |
| GET, POST | `/api/v1/rules` | List or create sensor rules |
| GET, PUT, DELETE | `/api/v1/rules/{id}` | Read, replace or delete a rule |
| POST | `/api/v1/rules/{id}/pause`, `/api/v1/rules/{id}/resume` | Pause or resume a rule |

When the scheduler failed to start, every route above except the event stream answers 503.

## Persistence

- **Definitions**: `<data_dir>/schedules.json`, rebuilt into timers at start. Each write goes
  through its own scratch file and a rename, in content order (`SnapshotWriter`), so a slow writer
  cannot publish older state over newer. An unreadable file is moved aside as
  `schedules.json.unreadable-*` and the scheduler starts empty rather than failing every route. A
  stored schedule that cannot be armed stays listed for editing but does not fire. Legacy records
  without `kind` are migrated from `payload`; a missing timezone reads as `UTC`.
- **Run history**: `<data_dir>/schedule_runs.json`, at most `schedule_max_runs_per_task` runs per
  schedule.

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `schedule_max_concurrent` | 2 | Parallel scheduled executions (read at startup) |
| `schedule_max_runs_per_task` | 50 | Run records kept per schedule (read at startup) |
| `schedule_result_notify` | true | Whether a finished run is announced to the household's phones; the dashboard's live feed gets it either way |
| `ext_schedule_enabled` | true | Register the `giap-schedule` MCP server |
| `timezone` | `UTC` | Default zone for MCP-created schedules and the zone quiet hours are judged in |
| `unprompted_speech_enabled` | false | Whether a result may be spoken at all |
| `quiet_hours_start`, `quiet_hours_end` | `22:00`, `07:00` | When a result is never spoken |

## Desktop

The Schedules section (`pond-desktop/src/sections/Schedules.tsx`) has list and calendar
(`ScheduleCalendar.tsx`) views; a create and edit form with repeat patterns, a time-zone picker
and prompt presets; cards with pause and resume, run now, edit and delete; and each card's last
five runs. `state/AppContext.tsx` subscribes to `/api/v1/schedules/events` for completion notices.
`pond-server` also turns each finished run into a broadcast notification
(`system_notices::schedule_result_notification`: category `info` when completed, `alert` when
failed, a tap opens the schedule), so results reach the phone. It is skipped when
`schedule_result_notify` is off, and when a rule with a `Notify` action completed, since that action
already told the household; a rule that failed is always announced. See
[notifications.md](notifications.md).
