# Notifications

How a notice from the Pond reaches a household member's phone. The routes themselves are in
[api.md](../api.md#devices-matter-and-notifications).

A notification is `Notification {id, target, category, title, body, timestamp, data}`
(`crates/pond-core/src/mcp/ports/notification.rs`). `category` is `alert`, `info` or
`action_required`; `target` is a device id or `"broadcast"`; `data` is an optional JSON object
that may carry an `open` route hint (see [Route hints](#route-hints-2026-11-04)) or an `action`
payload (see [rides and maps](rides-and-maps.md)). Every producer goes through the `NotificationSender`
port, implemented by `BroadcastNotificationSender` (`crates/pond-infra/src/broadcast_notification_sender.rs`),
which caps the title at 200 characters and the body at 2000.

## Route hints (2026-11-04)

What a tap on a notice opens. The Pond writes `data.open` only through `OpenScreen`
(`crates/pond-core/src/mcp/ports/notification.rs`), so the set of screens is closed:

| `open` | Extra keys | Opens |
|---|---|---|
| `schedule` | `schedule_id`, and `run_id` for a run's result | the schedules list at that schedule (`ScheduleDetail`, without `run_id`, is the missed-run question) |
| `chat` | `session_id` when the notice is about a conversation | that conversation; the chat screen when there is no `session_id` |
| `devices`, `models`, `extensions`, `remote_access` | none | that screen |
| `my_pond` (2026-11-08) | none | the phone's My Pond screen; the "My Pond unlocked" notice carries it. The dashboard has no such screen and offers no action |
| `goose_activity` (2026-11-12) | none | My Goose's list of what Goose did by itself; the notice after a heartbeat that set a reminder, paused a watch or looked something up carries it |

**The id rides beside the screen, never instead of it.** A client that does not read an id still
opens the right screen, and a client that cannot open the id opens the screen alone. Each client
validates both: `open` against its own allow-list (an unknown value opens nothing, and the app
stays where it was), and an id against the shape the Pond mints (`[A-Za-z0-9_-]{1,128}`); a
malformed id is dropped and the screen kept. A conversation that is gone, or is another member's,
is answered as missing by `GET /sessions/{id}/messages`; the client then opens the chat screen
alone. Nothing about a hint can crash a client or open anything outside its allow-list.

**Which notices name a conversation.** Before 2026-11-04 every `chat` hint was bare, so "Open
chat" on "Task done: ..." landed on the chat list, not the conversation the task reported to.

| Notice | `session_id` is | When there is none |
|---|---|---|
| long task done or stopped (`long-task-{id}`) | the task's `origin_session_id`, the conversation it was started from and reports back to | the conversation was deleted (the column is cleared), or the task was started outside one |
| an agent's run (`agent-run-{id}`) | the same, for the task the agent ran | as above |
| proposal (`proposal-{id}`) from a reminder | the conversation the reminder was heard in (the trigger's `source_id`, `reminder_session_id` in `user_data/services/reminder_proposal.rs`) | the proposal came from the proactive reviewer (a sensor, a camera, a device): no conversation is behind it, and the trigger's `source_id` (a device id) is never sent as one |

Schedule results keep `open: "schedule"`. A scheduled prompt runs in a `sched-{id}-{time}`
session the scheduler creates, but its turn is not written to `session_messages`, so that
conversation would open empty; the run's own result is on the schedules screen.

The phone keeps its own route name for a conversation, `conversation` (its notice that a reply
finished); it turns a Pond `chat` hint with a `session_id` into that route on arrival. The Pond
never sends `conversation`, so a phone from before 2026-11-04 opens its chat tab for these notices,
exactly as it did.

## Delivery classes

Each producer picks how far its notice goes, through one `NotificationSender` method:

| Method | Reaches | Queued and pushed to phones | In the household's history |
|---|---|---|---|
| `publish_live` | screens connected right now | never: transient status (the model warming up) | never; marked `data.transient = true` |
| `broadcast` | the household: connected screens, and every paired phone's own copy | yes | every device |
| `broadcast_except(device)` | every paired phone but `device`, which caused it | yes; no live broadcast copy, since the stream would hand that to `device` too | every device but `device` |
| `send` | one device | yes | that device |
| `MemberNotifier::notify_member` | one member's own devices, never a broadcast | yes | that member's devices at the time |

Before 2026-10-27 the warm-up status ("Pond is getting ready", "Pond is ready") went through
`broadcast`, so once broadcasts became durable every restart queued and pushed both to every paired
phone. It is `publish_live` now; a test pins that a warm-up is never queued for a phone.

## How a notice reaches a phone (2026-10-20)

A **targeted** notice (one device, or a member's devices through `MemberNotifier`) is queued in
`notifications` for each device, relayed through the push relay, and published live.

A **household** notice goes through `NotificationSender::broadcast()`. Before 2026-10-20 that only
published to the stream subscribers connected at that moment, so a phone that was asleep never saw
it, and nothing was queued or relayed. Now `broadcast()` also gives every **paired phone**
(`device_type = "gotg"` with an unrevoked session token, `NotificationRecipients`) its own copy,
queued and relayed exactly like a targeted notice. That copy's id is
`"{notification_id}:{device_id}"` (`per_device_notification_id`). Desktops and other clients get the
live broadcast copy only.

**One notice, one id per phone.** The stream skips a broadcast when the device it serves has its
own queued copy (the copy arrives on its own, and stays undelivered until the phone acks it). The
push wake's `notification_id`, the stream and `/pending` therefore all carry the same per-device id,
and a client that dedupes by `id` shows the notice once however it arrived. A copy that fails to
queue has no row, so the stream still sends that phone the broadcast copy.

## Who marks a notice delivered (2026-10-20)

For a **phone** (`device_type = "gotg"`) only `POST /notifications/{id}/ack` does; the stream never
does, on connect or live. A write into a connection the phone abandoned without closing (Wi-Fi and
data turned off) succeeds on the Pond's side for as long as the kernel buffers it, so before this
change such a notice was stamped delivered while the phone was offline, and the push wake's
`/pending` then came back empty. Each new stream connection therefore re-sends every copy the phone
has not acked; the app dedupes by `id` and acks each item after presenting it. The keep-alive does
not close that gap: it too is a write that succeeds into a half-open connection, and only TCP
retransmission timeouts end one. **Any other device** (a desktop, a speaker) has no ack path, so the
stream marks what it sends delivered, as before. `notifications_stream_flushed` logs each connect's
re-send count and whether the device acks.

An app build from before the ack route never acks, so its queued copies stay undelivered: each
reconnect and wake offers them again (it shows each id once per run, by its own dedupe), until the
pruning pass below removes them at 30 days or beyond the newest 500 for that phone. The table stays
bounded either way.

## Retention

The pruning pass (every 6 h) deletes delivered rows after 7 days, every row after 30 days, and
anything beyond a device's newest 500 (`NOTIFICATIONS_*` in `crates/pond-infra/src/pruning.rs`).

The household's history (below) keeps the same 30-day and 500-row caps, counted over the whole
household rather than per device.

## The household's history and the dashboard (2026-10-28)

Before 2026-10-28 the dashboard's notification centre listed only schedule runs: it subscribed to
`/schedules/events` and nothing else, so every notice in the catalogue below reached the phones and
never the dashboard. The queue could not serve it either: it holds per-phone copies only, none at
all for a household without a paired phone, and none for a member's notice to a member without one.

**The history.** `BroadcastNotificationSender::with_history` records every durable notice once,
under its household id, in `notification_history` (migration `0079`), through the
`NotificationHistory` port (`crates/pond-core/src/mcp/ports/notification_history.rs`, SQLite adapter
`crates/pond-infra/src/sqlite_notification_history.rs`). Who may read a row is its
`HistoryAudience` (`crates/pond-core/src/mcp/domain/notification_history.rs`), decided when the
notice is raised and following the queue's own scoping: a household notice is every device's
except the one that caused it (`broadcast_except`), and a member's notice (a proposal, a ride link)
belongs to the devices it was queued for at the time, in `notification_history_recipients`, so a
member's notice never reaches another member's device or an unattributed shared screen. A device
that changes member, or whose member is deleted, drops those rows (migration `0080`). Read state
is per device (`notification_history_reads`), so marking a notice read on the dashboard leaves it
unread on a phone. A failed history write is logged (`notification_history_record_failed`) and the
notice is still delivered. The `chat` process's member notifier (rides) records too.

**Live-only status is not history.** A `publish_live` notice (the warm-up) is never recorded, and
carries `data.transient = true` on the stream so a centre can leave it out. It is not listed even
transiently: the dashboard's warm-up banner (`/warmup`) already shows that state, and a list entry
saying "Pond is ready" would only be stale by the time anyone read it.

**The dashboard.** `AppContext` runs two fetch-based streams (`state/liveFeeds.ts`; an
`EventSource` cannot send the session token, and the old one on `/schedules/events` was refused by
every Pond without the loopback bypass) that reconnect with a growing pause (2 s up to 60 s):

1. On every (re)connect, `GET /notifications/history`, which also names the dashboard's own device
   id; then `GET /notifications/stream?device_id=` for that id.
2. Every notice is merged by household id (`state/householdNotices.ts`): a copy addressed to this
   device carries `"{id}:{device_id}"` and is mapped back, so the live copy and the history row are
   one entry. A notice read on either side stays read.
3. A schedule-result notice (`open: "schedule"`) whose run already has a card in the centre is
   folded into that card, which keeps its "View on Canvas"; one whose run has no card is listed.
4. Each notice is styled by its category (`info`, `alert`, `action_required`), and its `open` hint
   opens the dashboard's section: `schedule` the schedules, `devices`, `chat`, `models`,
   `extensions` their own, `remote_access` the Pairing page that holds remote access. An unknown
   hint offers no action. A `chat` hint with a `session_id` (2026-11-04: a long task, an agent's
   run, a suggestion from a reminder) opens that conversation: the chat screen asks the Pond for
   it first, and stays on its wall when the Pond has no such conversation for this dashboard
   (`openSessionId` in `pond-desktop/src/state/householdNotices.ts`, `requestedConversation` in
   the reducer).
5. "Mark all read", or opening one notice, marks it read at once and on the Pond with
   `POST /notifications/history/read`; a failure says so on the page. Schedule-run cards keep their
   in-memory read state. The bell counts unread runs plus unread listed notices.

A history read that fails (a dashboard with no paired device, such as one on loopback under
`POND_DEV_ALLOW_LOOPBACK`, gets `403`) shows "could not be loaded" on the page and keeps retrying.

## Setting up push

Without a key the Pond still queues every notice, and a phone receives it the next time it opens
the app, but nothing wakes a sleeping phone. To wake Android phones through Firebase Cloud
Messaging:

1. In the Firebase console for the app's project, create a service-account key (Project settings,
   Service accounts, Generate new private key). It downloads as a JSON file.
2. Put it on the Pond at `<data_dir>/secrets/fcm-service-account.json`, or point
   `POND_FCM_KEY_PATH` at it, and make it readable by the Pond's user only:
   `chmod 600 <data_dir>/secrets/fcm-service-account.json`.
3. Restart the Pond. Startup logs `kind="push_relay_fcm"` when the key is in use, and a WARN
   `kind="push_relay_stubbed"` with the `expected_path` when there is no key or it cannot be read.
   Only the path is logged, never the key.

The wake is data-only (`notification_id`, `category`, `wake: "1"`): no title or body passes through
Google. The phone reads the content from `/pending` or the stream.

To wake iPhones through the Apple Push Notification service:

1. In the Apple Developer account, create a key with Apple Push Notifications service (APNs)
   enabled (Certificates, Identifiers & Profiles, Keys). It downloads once, as a `.p8` file, and
   the page shows its Key ID.
2. Put a JSON file on the Pond at `<data_dir>/secrets/apns-key.json`, or point
   `POND_APNS_KEY_PATH` at it, readable by the Pond's user only:

   ```json
   {
     "team_id": "the 10-character Team ID",
     "key_id": "the key's 10-character Key ID",
     "bundle_id": "io.jarida.gooseonthego",
     "private_key": "-----BEGIN PRIVATE KEY-----\n...the .p8 file's lines...\n-----END PRIVATE KEY-----\n",
     "sandbox": false
   }
   ```

   `sandbox` is `true` for an app built for development, whose tokens belong to Apple's sandbox.
3. Restart the Pond. Startup logs `kind="push_relay_apns"` when the key is in use, and a WARN
   `kind="push_relay_stubbed"` with `reason="no_apple_key"` or `"apple_key_unusable"` otherwise.

iOS throttles silent pushes and drops them for an app the person swiped away, so the iPhone wake is
a visible alert. It says one fixed line, "Something new from your Pond.", and carries the same
`notification_id`, `category` and `wake: "1"`: no notice's title or body passes through Apple.
Repeat wakes for one notice share an `apns-collapse-id`, so they replace each other on the phone.

A Pond with one key wakes that kind of phone only. `pond-server reliability` says which keys are
present, and how many notices were woken, fetched and never fetched.

## What became of a notice

Each queued copy records more than whether it was acknowledged (migration 0102):

| Column | Stamped when |
|---|---|
| `wakes`, `woken_at` | a push wake really left the Pond for this copy, each time |
| `fetched_at` | the phone asked `/pending` for it, or acknowledged it |
| `delivered_at` | the phone acknowledged it (`POST /notifications/{id}/ack`) |
| `shown_at` | the acknowledgement said `{"shown": true}`: the phone put it in front of the person |
| `opened_at` | the phone said the person opened it (`POST /notifications/{id}/opened`) |
| `spoken_at` | the Pond said it in the room because no phone fetched it |

A copy no phone has fetched is woken again after 2 minutes, then at doubling gaps up to an hour,
six wakes in all and for a day at most. A due reminder (`data.speak_if_unfetched`) nobody fetched
within five minutes is said once in the room, outside quiet hours, when
`speak_unfetched_reminders` is on.

## Quiet hours

While the household's quiet hours last and `notices_wait_in_quiet_hours` is on, a copy that is
neither an `alert` nor marked `data.quiet_pass` is held (`held = 1`): no wake, nothing on an open
stream, not listed by `/pending`. When quiet hours end, each phone's held copies become one notice,
"N notices from quiet hours", naming up to six of them. A due reminder and an answer to something
the person just asked for (a form to confirm, a ride offer, a link sent to the phone) are marked to
pass. Unreadable quiet-hour bounds hold nothing.

## Catalogue (2026-10-27)

Every notice the Pond raises on its own. Titles and bodies are English, like every other producer's:
the Pond has no i18n framework yet. `{x}` is filled in. Categories: `info`, `alert`,
`action_required`. "Open" is the `data.open` route hint. System notices also carry `data.kind`, the
name in the first column.

### Household and member notices that existed before

| Kind | Trigger | Title / body | Category | Open | Audience | De-dup |
|---|---|---|---|---|---|---|
| schedule result | a schedule or rule run finishes (`ScheduleResultEvent`) | `Schedule: {label}` / the run's result or error | `info`, `alert` when it failed | `schedule` + ids | household | none per run; skipped when `schedule_result_notify` is off, and when a rule with a `Notify` action completed (that action already spoke); a failure is always announced |
| rule `Notify` action | a sensor, camera or device rule fires | the rule's own title / body | `alert` | none | household | the rule's `cooldown_secs` |
| `send_notification` tool | the model calls it | the tool's title / body | the tool's | none | household | none |
| proposal | the proactive reviewer makes a proposal (a reminder from conversation did from 2026-10-27; it is offered as a to-do instead from 2026-11-09) | `A suggestion` / the proposal's summary | `action_required` | `chat`, with the reminder's conversation (2026-11-04) | that member's devices only | one per proposal (`proposal-{id}`); the shared daily proposal cap |
| long task | a member's long task (deep research included) finishes or stops | `Task done: {goal}` / `Task stopped: {goal}`, the answer or the reason | `info` / `alert` | `chat`, with the conversation it was started from (2026-11-04) | the task's owner's devices only | one per task (`long-task-{id}`) |
| agent run | one of a member's agents finished a run with something to say | `From {agent}` / its summary | `info` | `chat`, with the task's conversation (2026-11-04) | the task's owner's devices only | one per task (`agent-run-{id}`) |
| pairing | a device pairs, or a pairing verification fails | `New device paired` / `Failed pairing attempt` | `info` / `alert` | `devices` | household | failures: one per 10 minutes |
| Matter | device added, device left, pairing failed, controller unreachable / back, first-run set-up started / finished | see `pond-adapters-matter/src/notify.rs` | `info` / `alert` | `devices` | household | alerts one per 10 minutes per kind; "back" only after an announced outage; a removal the Pond asked for is silent |
| ride links and offers | `giap-travel` | see [rides and maps](rides-and-maps.md) | | `action` payload | the member who asked | |
| web watch | a watch finds a match (`A match on {watch}`), cannot check its page three times in a row, or the daily digest at 18:00 | the change lines | `info` | `watch` (with `watch_id`; none for a digest across several) | the watch's owner's devices only | one per check with a match; one digest a day. A match is also said aloud to an owner who opted in ([web watches](web-watch-and-exploration.md)) |
| POST form | `read_web_page` is asked for a POST form | `Send this form to {site}?` / its fields and values | `action_required` | `action: form_confirm` with `id` | the member who asked | one per ask; it lapses unsent after 15 minutes |
| to-do reminder | a hook of a reminder (or of a to-do Goose need not ask about) fires: a time, a repeat, a place, a Pond event or a connector event (`pond-server/src/hooks_runner.rs`) | `Reminder: {text}` / the notes, else the hook's sentence (`When you get home`) | `info` | `todos` with `todo_id` | the to-do's owner's devices only | one per to-do per pass; an event hook stays quiet 10 minutes after firing |
| to-do check-in | a hook of a to-do with `ask` fires, or its follow-up after a not-yet comes (at the snooze, else 09:00 member time) | Goose's question (`Did you call the garage about the Fielder?`) / how to answer | `action_required` | `action: todo_checkin` with `todo_id`, `text`, `label`, and `todos` | the to-do's owner's devices only | at most three asks per to-do, then it stays open unasked |
| to-do offer | memory extraction lifted a dated mention from a member's chat (`reminders`) | `Add '{text}' to your to-dos for {day}?` / what they said and the time Goose worked out | `action_required` | `action: todo_offer` with `reminder_id`, `text`, `when_said`, `due_at`, `label`, and `todos` | that member's devices only | one per mention (`todo-offer-{reminder_id}`); counts toward the shared daily proposal cap |

### System notices (2026-10-27)

Raised through `SystemNotices` (`crates/pond-core/src/mcp/services/system_notices.rs`); the wording is
`SystemEvent::notification` (`crates/pond-core/src/mcp/domain/system_notice.rs`), and where each is
noticed is `crates/pond-server/src/system_notices.rs` unless the table says otherwise.

| Kind | Trigger | Title / body | Category | Open | De-dup |
|---|---|---|---|---|---|
| `model_downloaded` | a model or picture add-on download finishes (`model_acquisition::run`, pond-api) | `Model downloaded` / `"{model}" finished downloading and is ready to use.` (add-on: `Picture support downloaded` / `"{model}" can now read pictures.`) | `info` | `models` | one per transfer; two requests for one file share one transfer |
| `model_download_failed` | a download ends in error | `Model download failed` / `{what} did not finish downloading: {reason}. Try again from the Models screen.` | `alert` | `models` | one per transfer; a pause or a cancel is the household's own doing and says nothing |
| `startup_problems` | `serve` started, but speech recognition, spoken replies, or a saved extension did not | `The Pond started with problems` / one sentence per failed part | `alert` | `extensions` when an extension failed, else `models` | once per distinct set of failed parts a day, across restarts (`<data_dir>/.startup_notice.json`); a clean start forgets it. An extension the household adds later and that fails is shown to them where they added it, not pushed |
| `disk_low` | free space on the data directory's disk below the larger of 2 GB and 5 % of the disk, for two readings 5 minutes apart | `The Pond is running out of space` / `Only {free} of {total} is free on the Pond's disk. ...` | `alert` | `models` | recovers only 1 GB above the mark; a new low within 6 hours of the last announcement waits out the 6 hours |
| `disk_recovered` | free space back above the mark plus 1 GB, after an announced `disk_low` | `The Pond has space again` / `{free} is free on the Pond's disk again.` | `info` | `models` | only after an announced outage |
| `remote_access_down` | remote access on and registered, and the helper not `Running` for 5 minutes | `Remote access stopped` / `This Pond cannot be reached from outside the home. ...` | `alert` | `remote_access` | one per outage; a restart blip under 5 minutes is silent; a new outage within an hour of the last announcement waits out the hour |
| `remote_access_sign_in` | as above, with the helper in `NeedsLogin` or `NeedsMachineAuth` | `Remote access needs signing in` / `... Sign it in again ...` | `action_required` | `remote_access` | as above |
| `remote_access_restored` | `Running` again after an announced outage | `Remote access is working again` | `info` | `remote_access` | only after an announced outage; switching remote access off during one forgets it silently |
| `device_removed` | `DELETE /devices/{id}` removed a device that held a session (pond-api `unregister_device`) | `Device removed` / `"{name}" was removed from this Pond and can no longer reach it. ...` | `alert` | `devices` | one per removal; every paired phone but the one that removed it (`broadcast_except`); the removed device's access is revoked first; a device with no session is not news |
| `sign_in_expired` | an account provider refused the Pond's saved refresh token, and the Pond forgot it (OAuth refresh tick) | `Sign in to {provider} again` / `{provider} refused this Pond's saved sign-in ...` | `action_required` | none | once: the forgotten token is never refreshed again |

Notes on the remote-access notices: a phone away from home reaches the Pond only through remote
access, so while it is down the push wake arrives but `/pending` cannot be fetched; the notice is
read once the phone is home or remote access is back. Members at home see it at once.

### Switches

- `schedule_result_notify` (dashboard: "Tell me when a task finishes"): off, no schedule or rule
  run reaches the phones. Before 2026-10-27 nothing read it.
- `system_notices_enabled` (dashboard: "Tell me when something needs attention"): off, no system
  notice is sent except `device_removed`, which is a security notice and always sent. A failed
  settings read sends the notice.

### Not notified, by design

- Camera and vision events: a motion or face event is for a rule to act on. A household that wants
  one on the phone writes a vision automation with a `Notify` action. A default notice would push
  every passer-by.
- Suggestions composed from memories: questions served when someone opens the app, not news.
- Sensor readings and device state changes: through rules only.
- A device signing itself out: the device did it.
- Updates: the Pond has no update channel.

### Open product decision

`device_removed` goes to every paired phone in the household (except the actor), the conservative
default. Whether only the removed device's owner, or only household admins, should hear it is open.

### The app

The phone validates `open` against its own allow-list (`services/notification-link.ts` in
goose-on-the-go) and ignores a value it does not know, opening where it was. A `chat` hint with a
`session_id` opens that conversation from goose-on-the-go `fix/notification-popup-and-open` on. `models`, `extensions`
and `remote_access` open the app's Models, Extensions and Remote access screens from
goose-on-the-go `feature/notification-open-system-screens` on; an older app ignores them.
`my_pond` opens My Pond from goose-on-the-go `fix/unlock-notice-opens-my-pond` on; before it the
unlock notice opened Devices.
