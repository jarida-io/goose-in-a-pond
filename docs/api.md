# GIAP REST API Reference

Every route the pond serves, as `pond-server serve` composes it. The router is
`api_routes` in `crates/pond-api/src/routes.rs`, nested under `/api/v1` by `build_transport_router`
in `crates/pond-api/src/lib.rs`; the remote-access routes come from
`crates/pond-server/src/embedded_network.rs`, and `crates/pond-server/src/listeners.rs` decides which
listener serves what. When this page and those files disagree, the code is right.

Paths below are relative to `/api/v1` unless they start with `/api/v1` or are listed under
[Non-API paths](#non-api-paths).

## Contents

- [Listeners](#listeners)
- [Authentication](#authentication)
  - [Credentials](#credentials)
  - [Auth classes](#auth-classes)
  - [The onboarding gate](#the-onboarding-gate)
  - [Checks inside handlers](#checks-inside-handlers)
- [Limits, CORS and errors](#limits-cors-and-errors)
- [Route summary](#route-summary)
- [System and diagnostics](#system-and-diagnostics)
- [Pairing and tokens](#pairing-and-tokens)
- [Onboarding](#onboarding)
- [Connections and setup card](#connections-and-setup-card)
- [Settings](#settings)
- [Chat and runs](#chat-and-runs)
- [Sessions](#sessions)
- [Voice and audio](#voice-and-audio)
- [Models](#models)
- [Profiles and session identity](#profiles-and-session-identity)
- [Devices, Matter and notifications](#devices-matter-and-notifications)
- [Sensors, camera and rules](#sensors-camera-and-rules)
- [Schedules](#schedules)
- [Memory, reminders, suggestions and proposals](#memory-reminders-suggestions-and-proposals)
- [Personal context](#personal-context)
- [Activity, logs and telemetry](#activity-logs-and-telemetry)
- [Extensions, marketplace, secrets and sign-in](#extensions-marketplace-secrets-and-sign-in)
- [Agent, tools, prompts, skills and recipes](#agent-tools-prompts-skills-and-recipes)
- [Music player](#music-player)
- [Rides and Uber accounts](#rides-and-uber-accounts)
- [My Pond (administration)](#my-pond-administration)
- [Faces](#faces)
- [Mesh](#mesh)
- [Remote access](#remote-access)
- [Non-API paths](#non-api-paths)

---

## Listeners

| Listener | Bind | Default port | Serves |
|---|---|---|---|
| Dashboard | `127.0.0.1` | 4000, then 4001..4009 (`--port`) | `/api/v1/*`, the web UI, `/dev/test`, `/dev/face`, remote-access management |
| Companion | `0.0.0.0`, HTTPS | 4443, then the next free (`--https-port`) | `/api/v1/*`, remote-access configuration, enrollment and recovery; no web UI |
| Tailnet socket | a private Unix socket (Unix only) | none | the companion API, for requests the embedded network node forwards; the peer address comes from the node, and a tailnet request never counts as the phone being home |
| Development plaintext | `0.0.0.0`, HTTP | 4080 (`POND_DEV_INSECURE_LAN_PORT`) | the companion API without remote-access enrollment. Debug builds only, and only with `POND_DEV_INSECURE_LAN=1`; release builds ignore the variable. See [auth-network-posture.md](auth-network-posture.md) |

The bound ports are written to `<data_dir>/.runtime_api_port` and `<data_dir>/.runtime_https_port`;
read them rather than assuming the defaults.

The companion listener uses a TLS identity generated per pond (`crates/pond-server/src/tls_identity.rs`).
Phones pin its public key; the pin is `tls_spki_sha256` (`sha256/<base64>` of the DER
SubjectPublicKeyInfo), returned as part of the pairing material.

The dashboard listener is wrapped in `host_guard::loopback_only`: a request whose `Host` is not
`127.0.0.1`, `localhost` or `[::1]` gets `421 {"error":"loopback_host_required"}`, and one whose
`Origin` is neither a CORS origin nor `http://` on the same loopback host and port gets
`403 {"error":"origin_not_allowed"}`. This blocks DNS rebinding and cross-site requests from pages
in the host's own browser.

## Authentication

### Credentials

| Credential | How it is sent | Where it comes from |
|---|---|---|
| Session token | `Authorization: Bearer <token>` | Pairing (`/handshake/pake/verify`, `/handshake/verify`, `/handshake`), renewed with `/handshake/refresh`. Stored hashed; valid while unrevoked and unexpired (`Handshake::validate_token`, `crates/pond-infra/src/sqlite_handshake.rs`) |
| Host credential | `X-Pond-Host-Credential: <value>` | 43 random characters the server writes at every start to `<data_dir>/.runtime_host_credential` (mode 0600). Proves the caller can read the pond's data directory. Only the dashboard listener holds it: on the companion listener a route that needs it answers `503 {"error":"host_credential_unavailable"}` |
| Internal extension token | `Authorization: Bearer <token>` | A per-process value handed to extension subprocesses in `GIAP_INTERNAL_TOKEN` (`oauth_callback::internal_extension_token`). Accepted only by the routes that say so |
| Session token as a query | `?token=<percent-encoded token>` | The session token again, for an `<img>` tag or MJPEG viewer that cannot set a header. Accepted only on `GET` camera snapshot, stream and event-snapshot routes (`QUERY_TOKEN_ROUTES`, `crates/pond-api/src/middleware/mod.rs`); every other route answers `401`. Any `token` parameter is written to the request log as `token=<redacted>` |

`POND_DEV_ALLOW_LOOPBACK=1` is a development bypass: any loopback request (except on the
development plaintext listener) skips the bearer check, and an empty `X-Pond-Host-Credential` is
accepted. It is off unless set.

The device behind a token comes only from the token (`Handshake::caller_for_token`), never from a
request field. Handlers read it as a `Principal`/`ProvenDevice`.

### Auth classes

`auth_middleware` (`crates/pond-api/src/middleware/mod.rs`) classifies each request by
`PUBLIC_ROUTES`. A route not in that table needs a valid bearer token.

| Class | Without a token | With a token |
|---|---|---|
| `open` | Answered | Answered |
| `host` | Answered only from a loopback peer, and never on the development plaintext listener. Every non-`/api/` path is in this class | Answered |
| `wizard` | Answered only from loopback, and only while the pond is neither claimed (no device has paired) nor onboarded | Answered |
| `reset` | `POST /onboard/reset` only: from loopback while the pond is unclaimed, or after onboarding (the handler then asks for the host credential) | Answered |
| `token` | `401` | Answered |

`+onb` marks a route on the protected router (`require_onboarding_complete`,
`crates/pond-api/src/middleware/onboarding_guard.rs`): it answers
`403 {"error":"onboarding_required","message":"Complete onboarding before using this feature"}`
until onboarding reaches `Completed`, whatever credential the caller holds.

A missing, malformed or invalid token answers `401` with
`{"error": "Missing Authorization header" | "Invalid Authorization header format. Use: Authorization: Bearer <token>" | "Invalid or expired token", "status": 401}`.

### The onboarding gate

The claim and the onboarding state are read live on every `wizard`/`reset` request. A failed read
of either counts as the narrower state (claimed, onboarded). A pond with no setup record reads as
unclaimed.

| Tokenless caller | Unclaimed, not onboarded | Claimed, not onboarded | Onboarded |
|---|---|---|---|
| `wizard` routes | loopback only | nobody | nobody |
| `POST /onboard/reset` | loopback only | nobody | loopback with the host credential |

A paired device's token is admitted to every `wizard` route in every state, so a phone that claims
a pond runs owner setup as itself. Routes a phone needs during setup but that must never be
tokenless are `token` on the public router (no onboarding gate): `GET /settings`,
`POST /voice/sample`, `GET /network/status`, the connections routes, `GET /setup/status`, the five
model routes that list, fetch and pick the chat model `POST /onboard/complete` requires, and the
caller's own device routes. The rest of the API waits for onboarding.

### Checks inside handlers

The route summary's "Also" column names these.

| Check | Rule | Refusal |
|---|---|---|
| LAN | Loopback, or a peer on a directly attached LAN interface; tailnet and unclassifiable peers are refused (`network::require_lan`) | `403 pairing_requires_lan` |
| loopback | The socket peer is loopback | `403` (message varies) |
| cred | The host credential, from every caller (`host_guard::require_credential`) | `403 host_credential_required`, `503 host_credential_unavailable` |
| cred if tokenless | A caller admitted without a token must present the host credential (`bearer_or_host_credential`) | as above |
| own device | The device named in the path or query is the caller's own (`require_own_device`) | `403` |
| member | The caller's paired device is attributed to a household member | `403`; another member's record reads as `404` |
| unlocked | A member's device that unlocked My Pond with the admin card in the last 15 minutes ([pond administrator](architecture/pond-administrator.md)) | `403 admin_members_only`, `403 admin_locked` |
| changer | The host credential, the device that claimed the pond, or a member's device (`connection_changes`) | `403 not_allowed` |
| run owner | Only the device that started the run; unknown and not-yours are the same `404` | `404 unknown run` |
| internal | The internal extension token as the bearer | `401` |
| scoped | Results are scoped to the member the caller resolves to (`session_id` query plus the caller's device) | narrower results, or `403` for a write |
| consent | The stored `cameras_enabled` setting is on, read on every request (`camera::require_camera_consent`) | `403 {"reason": "cameras_disabled"}` |
| `?token=` | The session token may also come as a percent-encoded `token` query parameter (`GET` only) | `401` when it does not validate |

## Limits, CORS and errors

- **Rate limits** (`build_transport_router`): non-loopback clients get 600 requests per 60 s per
  IP; `/handshake` and `/handshake/*` have their own 30 per 60 s; `/handshake/pake/verify` and
  `/handshake/verify` add 10 per 60 s per IP (`verify_limiter`). The remote-access companion
  routes have 20 per 60 s. Loopback is never limited. A limit answers
  `429 {"error":"Rate limit exceeded","status":429}` with `Retry-After`.
- **Body size**: Axum's 2 MiB default, except `/chat/stream` and `/agent/chat/stream`
  (`MAX_CHAT_BODY_BYTES`, about 22 MiB, room for 4 images of 4 MiB as base64), the CPace routes
  (4 KiB), `/voice/sample` (1 KiB), `/connections/wifi/join` (4 KiB) and the remote-access
  companion routes (4 KiB).
- **Streams**: chat-like SSE routes share `sse_semaphore`; a full semaphore answers
  `503 {"error":"Too many concurrent streams"}` before the stream opens. `/notifications/stream`
  has its own semaphore. A resumable chat turn also needs a detached-run slot:
  `503 {"error":"Too many agent runs in flight ...","kind":"run_cap"}`.
- **CORS** (`cors_layer`): origins `app://giap` (the packaged desktop renderer), the Vite dev server
  at `http://localhost:1420` and `http://127.0.0.1:1420` in debug builds, the origin in
  `GIAP_DEV_SERVER`, and any listed in `POND_CORS_ALLOWED_ORIGINS` (comma-separated). Methods
  GET, POST, PUT, PATCH, DELETE, OPTIONS; headers `Authorization`, `Content-Type`,
  `X-Pond-Host-Credential`. Native clients send no `Origin` and are unaffected.
- **Errors** are JSON with at least `error`. Many routes add a stable `code`; pairing and
  connections routes put the stable code in `error` itself. Status codes in use: 400 bad body or
  parameter, 401 no valid token, 403 refused, 404 not found, 409 conflict or no model, 410 gone,
  413/415 image too large or unreadable, 421 dashboard reached by a non-loopback name, 422 valid
  JSON with invalid values, 428 missing secrets, 429 rate limited, 501 optional repository not
  wired, 502 an upstream refused, 503 a subsystem is not running, 504 timed out.

---

## Route summary

`Auth` uses the [auth classes](#auth-classes); `Also` the [handler checks](#checks-inside-handlers).

### System and diagnostics

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /health | open | | Liveness; `version` only to a loopback peer |
| GET | /system/info | open | | HTTPS port and tailnet address; the full record with a valid token |
| GET | /warmup | wizard | | Prefix warm-up state for the boot banner |
| GET | /time/zones | open | | Every IANA zone with today's offset |
| POST | /location/detect | wizard | | Where this pond is, cheapest source first |
| GET | /test | host | | Probe whisper, llamafile, Ollama and the chat model |
| POST | /test/speak | host | cred | Speak text on the host's speaker |
| GET | /dev/goose | token +onb | | Goose agent status and loaded extensions |
| GET | /weather | token +onb | | Weather widget data |
| GET | /usage/summary | token +onb | | Token usage across all sessions |
| GET | /lane | token +onb | | Background jobs and what each waits for |
| POST | /lane/jobs/{job}/run | token +onb | | Wake one background job |

### Pairing and tokens

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| POST | /handshake/pake/init | open | LAN | Phone pairing (CPace), phase 1 |
| POST | /handshake/pake/verify | open | LAN | Phone pairing (CPace), phase 2: tokens |
| POST | /handshake/init | open | LAN, loopback | Dashboard pairing (HMAC), phase 1 |
| POST | /handshake/verify | open | LAN, loopback | Dashboard pairing (HMAC), phase 2: tokens |
| POST | /handshake | open | LAN, loopback | Legacy single-step pairing |
| POST | /handshake/refresh | open | | Swap a refresh token for a new pair |
| POST | /handshake/revoke | token | | Sign this device out and unregister it |
| GET | /handshake/pairing-code | open | loopback, cred | The current pairing code and pairing material |
| POST | /handshake/pairing-code | open | loopback, cred | Issue a fresh pairing code |
| POST | /handshake/spoken-code | open | LAN | Read a fresh pairing code aloud; never returns it |

### Onboarding

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /onboard/status | open | | Onboarding progress |
| POST | /onboard | wizard | | Start onboarding, or report where it is |
| POST | /onboard/step/{name} | wizard | | Record reaching a step |
| POST | /onboard/complete | wizard | | Finish onboarding; attributes the claiming device |
| POST | /onboard/reset | reset | cred if tokenless | Back to `Welcome`; reopens the wizard routes |

### Connections and setup card

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /network/status | token | | The BLE setup channel and Wi-Fi station, and why each part is off |
| GET | /connections | token | | Wi-Fi, Ethernet and Bluetooth, and whether this caller may change them |
| GET | /connections/wifi/networks | token | | Scan now: the ranked Wi-Fi list |
| POST | /connections/wifi/join | token | changer | Join a Wi-Fi network |
| DELETE | /connections/wifi/networks/{ssid} | token | changer | Forget a saved Wi-Fi network |
| GET | /setup/status | token | | The setup card's state, never its code |

### Settings

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /settings | token | | The whole `Settings` record |
| PUT | /settings | wizard | | Partial update; answers with the merged record |

### Chat and runs

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| POST | /chat | token +onb | | One turn, answered whole |
| POST | /chat/stream | token +onb | | One turn as SSE; resumable on request |
| GET | /chat/runs/{run_id}/events | token +onb | run owner | Reattach to a run (SSE) |
| POST | /chat/runs/{run_id}/cancel | token +onb | run owner | Stop a run |
| GET | /sessions/{session_id}/active-run | token +onb | run owner | The run in flight for a session |
| DELETE | /sessions/{session_id}/active-run | token +onb | run owner | Stop the run in flight for a session |
| POST | /agent/chat/stream | token +onb | | One turn through the agent loop as SSE (not resumable) |

### Sessions

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /sessions | token +onb | | All sessions, most recently updated first |
| PATCH | /sessions/{session_id} | token +onb | | Rename |
| DELETE | /sessions/{session_id} | token +onb | | Delete with its messages |
| GET | /sessions/{session_id}/messages | token +onb | | Messages, paged |
| DELETE | /sessions/{session_id}/messages/{message_id} | token +onb | | Delete a message and every later one |
| PUT | /sessions/{session_id}/messages/{message_id}/feedback | token +onb | | Like, dislike or clear |
| GET | /sessions/{session_id}/attachments/{attachment_id} | token +onb | | Raw bytes of one chat image |
| POST | /sessions/{session_id}/compact | token +onb | | Compact this session now |
| POST | /sessions/retitle | token +onb | | Run the titling job's next pass now |
| POST | /sessions/{session_id}/retitle | token +onb | | Retitle one session |

### Voice and audio

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| POST | /tts | host | cred if tokenless | Synthesise WAV |
| POST | /voice/tts/apply | wizard | | Apply voice settings to the live engine, downloading what is missing |
| POST | /voice/sample | token | | Hear an installed voice on the pond's speaker |
| GET | /voice/tts/status | token +onb | | Whether the speech engine is available and loaded |
| POST | /voice/tts/prewarm | token +onb | | Load the speech engine |
| POST | /voice/tts/unload | token +onb | | Unload the speech engine |
| POST | /voice/calibrate | wizard | | Add a wake-phrase calibration sample |
| DELETE | /voice/calibrate | wizard | | Clear wake-phrase calibration |
| POST | /transcribe | token +onb | | Transcribe multipart audio |
| GET | /audio/devices | token +onb | | Which microphone and speaker are in use, and why |
| POST | /audio/devices/test | token +onb | | Re-test the audio devices, optionally with a chime |

### Models

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /models | token | | The catalog, grouped by category |
| GET | /models/download/progress | token | | Downloads in flight and recently finished |
| POST | /models/download/control | token | | Pause, resume or cancel a download |
| POST | /models/{category}/{name}/download | token | | Download a catalog model |
| POST | /models/{category}/{name}/activate | token | | Assign a model to a role |
| POST | /models/download/url | token +onb | | Download a model file from an HTTPS URL |
| POST | /models/{category}/{name}/companions/pictures | token +onb | | Add picture support to a model |
| POST | /models/{category}/{name}/revoke | token +onb | | Revoke a model's role |
| DELETE | /models/{category}/{name} | token +onb | | Delete the file, keep the catalog row |
| GET | /models/capabilities | token +onb | | The active model's capabilities |
| GET | /models/vision-status | token +onb | | Picture support for the active chat model |
| GET | /models/memory-status | token +onb | | The LLM memory budget |
| GET | /models/active-roles | token +onb | | Which model holds each role |
| POST | /models/registry/refresh | token +onb | | Refresh the catalog from upstream |
| POST | /models/scan | token +onb | | Scan the models directory for new files |
| POST | /models/cleanup | token +onb | | Sweep unreferenced Hugging Face cache blobs |
| GET | /models/disk-usage | token +onb | | Bytes per category and in the cache |
| GET | /models/ollama | token +onb | | Ollama's local models |
| POST | /models/ollama/pull | token +onb | | `ollama pull` a model |
| GET | /models/search/gguf | token +onb | | Search Hugging Face for GGUF models |
| GET | /models/search/gguf/files | token +onb | | The `.gguf` files in a Hugging Face repo |
| GET | /models/search/llamafile | token +onb | | llamafile releases on GitHub |

### Profiles and session identity

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| POST | /profiles | wizard | | Create a household member |
| PATCH | /profiles/{id} | wizard | | Merge preference keys |
| GET | /profiles | token +onb | | All members |
| GET | /profiles/{id} | token +onb | | One member |
| DELETE | /profiles/{id} | token +onb | | Remove a member, report what went with them |
| GET | /sessions/{session_id}/user | token +onb | | Who a session belongs to, and on what evidence |
| PUT | /sessions/{session_id}/user | token +onb | policy | Identify the session as a member |
| DELETE | /sessions/{session_id}/user | token +onb | | Release the binding |
| POST | /sessions/{session_id}/identify-user | token +onb | | Wake-on-face: identify and bind |

### Devices, Matter and notifications

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /devices/self | token | paired device | The caller's device and its member |
| POST | /devices/{id}/push-token | token | own device | Set the device's push token |
| DELETE | /devices/{id}/push-token | token | own device | Remove it |
| GET | /notifications/stream | token | own device | Notifications for one device (SSE) |
| GET | /notifications/pending | token | paired device | The caller's unacknowledged notifications |
| POST | /notifications/{id}/ack | token | paired device | Mark one of the caller's notifications delivered |
| GET | /devices | token +onb | | All registered devices |
| POST | /devices | token +onb | | Register a device |
| PUT | /devices/{id} | token +onb | own row or cred | Rename, set hostname or room |
| DELETE | /devices/{id} | token +onb | | Unregister; revokes remote access first |
| POST | /devices/{id}/heartbeat | token +onb | | Refresh `last_seen` |
| POST | /devices/{id}/offline | token +onb | | Mark a device off |
| POST | /devices/commission | token +onb | | Commission a Matter device |
| GET | /matter/status | token +onb | | The Matter controller's state |

### Sensors, camera and rules

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| POST | /sensors | token +onb | | Record a reading |
| GET | /sensors/{device_id} | token +onb | | Recent readings, optionally aggregated |
| POST | /camera/events | token +onb | | Record a camera event |
| GET | /camera/events | token +onb | | List camera events |
| PATCH | /camera/events/{id}/acknowledge | token +onb | | Dismiss a camera alert |
| GET | /camera/events/{id}/snapshot | token +onb | `?token=`, consent | One event's JPEG |
| GET | /cameras | token +onb | | Known cameras and whether each is live |
| GET | /camera/snapshot | token +onb | `?token=`, consent | The configured camera's latest JPEG |
| GET | /camera/{camera_id}/snapshot | token +onb | `?token=`, consent | A camera's latest JPEG |
| GET | /camera/stream | token +onb | `?token=`, consent | The configured camera as MJPEG |
| GET | /camera/{camera_id}/stream | token +onb | `?token=`, consent | A camera as MJPEG |
| GET | /vision/status | token +onb | | What the camera pipeline is doing, and why when it is not |
| GET | /vision/config | token +onb | | The vision settings as the pipeline applies them |
| PUT | /vision/config | token +onb | | Change and save vision settings |
| GET | /vision/events/stream | token +onb | consent | Camera events as they happen (SSE) |
| GET | /rules | token +onb | | Every sensor rule |
| POST | /rules | token +onb | | Create a sensor rule |
| GET | /rules/{id} | token +onb | | One rule |
| PUT | /rules/{id} | token +onb | | Replace a rule's name and spec |
| DELETE | /rules/{id} | token +onb | | Remove a rule |
| POST | /rules/{id}/pause | token +onb | | Stop it firing |
| POST | /rules/{id}/resume | token +onb | | Let it fire again |

### Schedules

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /schedules | token +onb | | All scheduled tasks |
| POST | /schedules | token +onb | | Create a task |
| GET | /schedules/upcoming | token +onb | | Active tasks by next fire |
| PUT | /schedules/{id} | token +onb | | Update a task |
| DELETE | /schedules/{id} | token +onb | | Delete a task |
| POST | /schedules/{id}/pause | token +onb | | Pause |
| POST | /schedules/{id}/resume | token +onb | | Resume |
| POST | /schedules/{id}/run-now | token +onb | | Fire now |
| GET | /schedules/{id}/runs | token +onb | | Run history |
| GET | /schedules/events | token +onb | | Completion events (SSE) |

### Memory, reminders, suggestions and proposals

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /memories | token +onb | | The 50 most recent memories |
| POST | /memories | token +onb | | Save a memory |
| PUT | /memories/{id} | token +onb | | Reword or re-tier a memory |
| DELETE | /memories/{id} | token +onb | | Delete a memory |
| GET | /memories/extraction-status | token +onb | | What the extraction engine is doing |
| POST | /memory/consolidate | token +onb | | Run consolidation (SSE) |
| POST | /memory/consolidate/stop | token +onb | | Stop consolidation |
| GET | /reminders | token +onb | scoped | Pending reminders |
| POST | /reminders/{id}/dismiss | token +onb | scoped | Dismiss a reminder |
| GET | /suggestions | token +onb | scoped | What the household might want to ask |
| POST | /suggestions/{id}/taken | token +onb | | Record a composed suggestion as taken |
| GET | /proposals | token +onb | scoped | This member's live proposals |
| POST | /proposals/{id}/decide | token +onb | scoped | Approve or reject a proposal |

### Personal context

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /context/sources | token +onb | scoped | Sources this caller may see |
| POST | /context/sources | token +onb | scoped | Connect a source |
| DELETE | /context/sources/{id} | token +onb | scoped | Disconnect a source |
| POST | /context/sync | token +onb | | Pull every connected account now |
| GET | /context/items | token +onb | scoped | What the pond has read, keyword search |
| GET | /context/index/health | token +onb | | The vector index's coverage |
| POST | /context/index/rebuild | token +onb | | Empty the index for the sweep to refill |

### Activity, logs and telemetry

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /activity | token +onb | | Recent events, newest first |
| DELETE | /activity | token +onb | | Clear the event log, or a filtered part |
| GET | /activity/summary | token +onb | | Per-category counts |
| GET | /security/policy-report | token +onb | | What the policy would deny under enforcement |
| GET | /logs | token +onb | | Recent `event_log` rows |
| GET | /logs/export | token +onb | | The event log as CSV |
| GET | /telemetry/turns | token +onb | | Per-turn metrics for a session |
| GET | /telemetry/summary | token +onb | | Aggregated metrics for a session |

### Extensions, marketplace, secrets and sign-in

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /extensions | token +onb | | Active MCP extensions |
| POST | /extensions | token +onb | | Register an extension |
| PATCH | /extensions/{name} | token +onb | | Enable or disable |
| DELETE | /extensions/{name} | token +onb | | Remove with its saved config |
| GET | /extensions/{name}/secrets | token +onb | | Secret requirements and which are set |
| POST | /extensions/{name}/secrets | token +onb | | Set several secrets |
| GET | /marketplace | token +onb | | The curated extension registry |
| POST | /marketplace/{id}/install | token +onb | | Install from the registry |
| GET | /secrets | token +onb | | Stored secret names, never values |
| PUT | /secrets/{key} | token +onb | | Set a secret |
| DELETE | /secrets/{key} | token +onb | | Delete a secret |
| GET | /secrets/{key}/exists | token +onb | | Whether a secret is set |
| GET | /web-search | token +onb | | Web search provider and key status |
| PUT | /web-search/key | token +onb | | Check and store a Brave key |
| DELETE | /web-search/key | token +onb | | Remove the key |
| GET | /oauth/providers | token +onb | | Supported sign-in providers |
| POST | /oauth/authorize | token +onb | | Start a PKCE sign-in |
| GET | /oauth/callback | open +onb | | The provider's redirect target |
| GET | /oauth/status/{state} | token +onb | | How a sign-in ended |
| POST | /oauth/refresh | open +onb | internal | Refresh a provider's access token |

### Agent, tools, prompts, skills and recipes

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /agent/tools | token +onb | | Every MCP tool the agent has loaded |
| POST | /tools/invoke | token +onb | | Run an MCP tool directly |
| POST | /mcp/tools/call | token +onb | | MCP Apps `callServerTool()` |
| GET | /mcp/resources | token +onb | | An MCP App `ui://` resource |
| GET | /agent/extras | token +onb | | System prompt extras |
| POST | /agent/extras | token +onb | | Create or replace an extra |
| DELETE | /agent/extras/{key} | token +onb | | Delete an extra |
| GET | /prompts | token +onb | | Prompt templates |
| GET | /prompts/{name} | token +onb | | One template |
| PUT | /prompts/{name} | token +onb | | Create or replace a template |
| DELETE | /prompts/{name} | token +onb | | Delete a user template |
| POST | /prompts/{name}/reset | token +onb | | Restore a built-in template |
| GET | /skills | token +onb | | Skills |
| POST | /skills | token +onb | | Create a skill |
| PUT | /skills/{id} | token +onb | | Update a skill |
| DELETE | /skills/{id} | token +onb | | Delete a skill |
| GET | /recipes | token +onb | | Recipes |
| POST | /recipes | token +onb | | Create a recipe |
| PUT | /recipes/{id} | token +onb | | Update a recipe |
| DELETE | /recipes/{id} | token +onb | | Delete a recipe |
| POST | /recipes/{name}/run | token +onb | | Run a recipe (SSE) |

### Music player

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /musickit/developer-token | token +onb | | Apple Music developer token for the player page |
| GET | /player/events | token +onb | | The player page's command stream (SSE) |
| POST | /player/reply | token +onb | | The page answers a command |
| GET | /player/state | token +onb | | The last state a page reported |
| POST | /player/state | token +onb | | A page reports what is playing |
| GET | /player/user-token | token +onb | | The page's access token for a service |
| POST | /player/egress-policy | token +onb | | Whether the network setting allows a service's script |
| POST | /player/command | host +onb | internal | An extension asks the page to act |
| GET | /player/status | host +onb | internal | Which services have a page and credentials |
| POST | /extension/egress | host +onb | internal | Whether an extension's outbound call is allowed |
| GET | /music/now-playing | token +onb | | Spotify playback snapshot |
| POST | /music/control | token +onb | | Play, pause, next, previous on Spotify |

### Rides and Uber accounts

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| POST | /rides/quote | token +onb | member | An upfront fare; books nothing |
| GET | /rides/{id} | token +onb | member | A quoted or booked ride |
| POST | /rides/{id}/confirm | token +onb | member | Book at the quoted fare |
| POST | /rides/{id}/decline | token +onb | member | Turn the fare down |
| POST | /rides/{id}/cancel | token +onb | member | Cancel a booked ride |
| GET | /uber/accounts | token +onb | loopback, cred | Members who connected Uber |
| POST | /uber/accounts/connect | token +onb | loopback, cred | Start a member's Uber sign-in |
| DELETE | /uber/accounts/{profile_id} | token +onb | loopback, cred | Forget a member's Uber sign-in |

### My Pond (administration)

Contract and refusals: [pond administrator](architecture/pond-administrator.md#wire-contract).

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /admin/status | token +onb | member | Whether the pond has an admin card and this device is unlocked |
| POST | /admin/unlock | token +onb | member | Unlock this device with the admin card or its own key; wrong ones are slowed |
| POST | /admin/lock | token +onb | member | End this device's unlock |
| POST | /admin/enroll | token +onb | unlocked | This device's own key, shown once, replacing any older one |
| DELETE | /admin/key | token +onb | member | Revoke this device's key and lock it |
| GET | /admin/overview | token +onb | unlocked | Disk, storage per area, databases and record counts |
| GET | /admin/files | token +onb | unlocked | One data-directory folder, largest first; credentials never listed |
| DELETE | /admin/files | token +onb | unlocked | Delete a file or folder the pond does not need |
| GET | /admin/caches | token +onb | unlocked | KV snapshots, download leftovers and accelerator caches |
| POST | /admin/caches/{id}/clear | token +onb | unlocked | Clear one cache |
| GET | /admin/tasks | token +onb | unlocked | Long tasks and their artifacts; another member's words withheld |
| DELETE | /admin/tasks/{id} | token +onb | unlocked | Delete a finished task with its report |
| DELETE | /admin/tasks/{id}/artifacts/{artifact_id} | token +onb | unlocked | Delete one artifact of a finished task |
| GET | /admin/health | token +onb | unlocked | Host, memory, disk, temperatures and checks |
| GET | /admin/power | token +onb | unlocked | Power mode, modes, supply and battery |
| PUT | /admin/power/mode | token +onb | unlocked | Switch power mode |
| POST | /admin/power/restart | token +onb | unlocked | Restart the computer; waits for `force` while a long task runs |

### Faces

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /faces | token +onb | | Every household member and their face enrolment |
| POST | /faces/register | token +onb | | Enroll a face sample |
| POST | /faces/register-from-camera | token +onb | consent | Enroll from the pond's own camera |
| POST | /faces/identify | token +onb | | Best-matching member above threshold |
| POST | /faces/identify-burst | token +onb | | Multi-frame consensus with liveness |
| POST | /faces/enroll-quality | token +onb | | Would this frame enroll? Persists nothing |
| GET | /faces/profile/{profile_id} | token +onb | | Enrollment metadata |
| GET | /faces/profile/{profile_id}/threshold | token +onb | | Per-member threshold override |
| PUT | /faces/profile/{profile_id}/threshold | token +onb | | Set the override |
| DELETE | /faces/profile/{profile_id}/threshold | token +onb | | Clear the override |
| GET | /faces/models | token +onb | | Face model files on disk |
| GET | /faces/debug/pairwise | token +onb | | Pairwise similarities |
| GET | /faces/debug/eval | token +onb | | FAR/FRR threshold sweep |
| DELETE | /users/{profile_id}/biometrics | token +onb | | Forget a member's biometric data |

### Mesh

| Method | Path | Auth | Also | Summary |
|---|---|---|---|---|
| GET | /mesh/self | token +onb | | This pond's peer id and invite link |
| GET | /mesh/peers | token +onb | | Trusted peers, connection and credit |
| POST | /mesh/peers | token +onb | | Trust a peer |
| DELETE | /mesh/peers/{peer_id} | token +onb | | Revoke trust |
| POST | /mesh/peers/{peer_id}/credit | token +onb | | Top up a peer's credit |
| GET | /mesh/peers/{peer_id}/capabilities | token +onb | | A peer's capabilities, asked live |
| GET | /mesh/settlement | token +onb | | The rate and each peer's pending usage |

### Remote access (pond-server)

| Method | Path (full) | Listener | Auth | Summary |
|---|---|---|---|---|
| GET | /api/v1/remote-access | dashboard | loopback, cred | Embedded node state and addresses |
| POST | /api/v1/remote-access | dashboard | loopback, cred | Enable with a `Config` |
| DELETE | /api/v1/remote-access | dashboard | loopback, cred | Disable |
| POST | /api/v1/remote-access/identity | dashboard | loopback, cred | The authority identity |
| POST | /api/v1/remote-access/register | dashboard | loopback, cred | Register this pond with the coordinator |
| GET | /api/v1/remote-access/device | dashboard | loopback, cred | Whether the pond holds a device certificate |
| GET | /api/v1/remote-access/recovery-requests | dashboard | loopback, cred | Pending phone recovery requests |
| POST | /api/v1/remote-access/recovery-requests/{id} | dashboard | loopback, cred | Approve one |
| GET | /api/v1/remote-access/configuration | companion | token, paired device | Remote access state for this phone |
| POST | /api/v1/remote-access/enrollment | companion | token, LAN | Enroll this phone on the tailnet |
| POST | /api/v1/remote-access/recovery | companion | token, LAN | Ask to replace this phone's enrollment |
| GET, POST, DELETE | /api/v1/remote-access/recovery/{id} | companion | token, LAN | Poll, execute or cancel that request |

### Non-API paths

| Method | Path | Listener | Auth | Summary |
|---|---|---|---|---|
| GET | /dev/test | dashboard | host | Developer test page |
| GET | /dev/face | dashboard | host | Developer face page |
| GET | any other path | dashboard | host | The web UI: the `pond-desktop/dist` embedded at build time, else `--static-dir`; paths without a `.` fall back to `index.html` |

---

## System and diagnostics

**`GET /health`** answers `{"status":"ok","version":"<crate version>"}` to a loopback peer and
`{"status":"ok"}` to anyone else, including loopback on the development plaintext listener.

**`GET /system/info`** without a valid token:

```json
{ "https_port": 4443, "tailnet_address": "100.64.0.7", "protocol": 3 }
```

`protocol: 3` (CPace pairing over pinned HTTPS) appears only in this anonymous answer; a client
built for another version should stop and say an update is needed. With a valid token the answer is
the pairing material plus the dashboard port and build: `hostname`, `lan_address`,
`tailnet_address`, `https_port`, `tls_spki_sha256`, `port`, `version`, `platform`, `arch`.
`lan_address` and `tailnet_address` are `null` without such a route. While the development
plaintext listener runs, both answers add `"insecure_dev": true`, and the anonymous one also
carries `tls_spki_sha256`.

**`GET /warmup`** serialises the warm-up snapshot (`phase`, start and finish times) and adds
`elapsed_ms`, `reason` and `message`.

| Route | Request | Response |
|---|---|---|
| `GET /time/zones` | | `{zones: [{zone, offset, place}]}` |
| `POST /location/detect` | optional `DetectLocationRequest {system_zone?, typed_name?, latitude?, longitude?}` | `{name, latitude, longitude, timezone, source, certain, has_coordinates, note}` |
| `GET /test` | | `{whisper, llamafile, ollama, llm}`; each has `status` (`ok`, `error`, `unavailable` or `not_configured`) |
| `POST /test/speak` | optional `{text}` | `{status, text, latency_ms}` |
| `GET /dev/goose` | | `{goose_active, extension_count, extensions: [{name, kind, tools}], tool_count}`, or `{goose_active: false, message}` |
| `GET /weather` | | `{enabled: false}` without a location, else the current conditions and `forecast` |
| `GET /usage/summary` | | `{total_prompt_tokens, total_completion_tokens, total_reasoning_tokens, counted_reasoning_turns, total_tokens, session_count, cloud_input_price_per_million, cloud_output_price_per_million}` |
| `GET /lane` | | The lane's jobs: per job `title`, `enabled`, `since_last_run_secs`, `blocked_by`, refusal counters, plus `running` and `observed_secs_ago` |
| `POST /lane/jobs/{job}/run` | | `{known, lane, job, woken, reason}`; `404` for no such job. The job still waits for the single slot |

## Pairing and tokens

Phones pair with CPace over the pinned HTTPS listener; the desktop pairs over loopback with an HMAC
challenge. Every pairing with a pairing code, by any method, latches the pond's claim.

### Phone pairing: `POST /handshake/pake/init` and `/handshake/pake/verify`

CPace (`draft-irtf-cfrg-cpace-21`, suite `CPACE-RISTR255-SHA512`); the pond is initiator A, the
phone responder B. The cryptography, labels and encodings are in
`crates/pond-infra/src/pairing_cpace.rs`; one complete run with every intermediate value is
`crates/pond-infra/tests/fixtures/cpace-vectors.json`.

| Value | Definition |
|-------|------------|
| PRS | the six-digit code as six ASCII bytes, leading zeros kept |
| CI | `lv_cat("goose-pair-cpace-v1", spki_pin, client_id)`; `spki_pin` is the ASCII `sha256/<base64>` pin. The pond always uses its own pin |
| sid | `challenge (32 bytes) ‖ client_nonce (32 bytes)` |
| ADa, ADb | empty |
| g | RFC 9496 element derivation of `SHA-512(generator_string(DSI, PRS, CI, sid, 128))` |
| ISK | `SHA-512(lv_cat("CPaceRistretto255_ISK", sid, K) ‖ lv_cat(Ya, ADa) ‖ lv_cat(Yb, ADb))` |
| mac_key | `SHA-512("CPaceMac" ‖ sid ‖ ISK)` |
| T_client | `HMAC-SHA-512(mac_key, lv_cat("goose-pair-cpace-client-v1", Ya, ADa, Yb, ADb))` |
| T_server | `HMAC-SHA-512(mac_key, lv_cat("goose-pair-cpace-server-v1", Ya, ADa, Yb, ADb))` |

`lv_cat` prefixes each field with its LEB128 length. Shares are 32-byte ristretto255 encodings in
standard base64; tags are 64 bytes in lowercase hex.

`init` takes `PakeInitRequest {client_id, client_type, client_version, channel_binding, client_nonce, credential?}`.
`channel_binding` is the pin the phone pinned; it is checked only so a phone at the wrong pond
fails early. `credential` names the secret: `"pairing"` (default, a six-digit pairing code) or
`"setup"` (the eleven-digit code on the printed setup card). For `"setup"` the PRS is the eleven
digits and CI uses the label `goose-setup-cpace-v1`. `"device"` belongs to the BLE setup channel
only and is refused here. `init` answers `PakeChallenge {challenge_id, challenge, expires_at, suite, share}`.

`verify` takes `PakeVerifyRequest {challenge_id, share, confirm, device_name?}` and answers
`HandshakeResponse`: `accepted`, `session_token`, `refresh_token`, `expires_at`, `hostname`,
`server_version`, `capabilities`, `rejection_reason`, and `server_proof` (T_server in hex), which
the phone must verify before keeping the tokens. A refusal is a `200` with `accepted: false` and the
same code in both `rejection_reason` and `error`.

| Route | Status | `error` | Meaning |
|-------|--------|---------|---------|
| init | 403 | `pairing_requires_lan` | Not a direct LAN or loopback peer |
| init | 403 | `channel_binding_mismatch` | The phone pinned a key that is not this pond's |
| init | 409 | `no_active_pairing_code` | No live code |
| init | 400 | `invalid_client_nonce` | `client_nonce` is not 32 bytes of base64 |
| init | 400 | `invalid_credential` | `credential` is not `"setup"` or `"pairing"` |
| init | 409 | `setup_code_unavailable` | `"setup"` on a pond that is claimed or has no setup code |
| init | 429 | `setup_code_cooling`, `device_cooling` | Too many wrong codes; `retry_after` and `Retry-After` say when |
| init | 503 | `pairing_busy` | Too many runs are waiting for their verify |
| verify | 429 | `pairing_busy` | The per-IP verify limit; `retry_after_secs` says when |
| verify | 200 | `invalid_confirmation` | T_client is wrong: a wrong code or pin |
| verify | 200 | `invalid_share` | Yb did not decode, or the shared point was the identity |
| verify | 200 | `code_retired` | The code was consumed, replaced, expired or spent its 10 failed attempts |
| verify | 200 | `unknown_challenge` | No such run, or it was already used |
| verify | 200 | `challenge_expired` | The run is older than 60 s |
| verify | 200 | `setup_code_unavailable` | Another device claimed the pond first; nothing was minted |

Each pairing code allows 10 confirmation attempts in all, and issuing a new code retires every
older one. The setup code is never retired: five attempts are free, then one per 30 s, doubling to
one per 10 minutes, across every client. A confirmed setup run latches the claim before any token
exists, then registers the device, bound to the household's only member if it has exactly one.

### Dashboard pairing: `POST /handshake/init` and `/handshake/verify`

For the desktop on the host, with the code it reads from `GET /handshake/pairing-code`. `init`
takes `InitRequest {client_id, client_type, client_version}` and answers
`ChallengeResponse {challenge_id, challenge, expires_at}`; `verify` takes
`VerifyRequest {challenge_id, mac, device_name?}` with `mac = HMAC-SHA256(code, challenge ‖ client_id)`
in hex, and answers `HandshakeResponse`. A peer that is not loopback, or any peer on the
development plaintext listener, gets `403 pairing_requires_cpace`.

### `POST /handshake` (legacy)

Single step: `HandshakeRequest {client_id, client_type, client_version, pairing_code?}` answered by
`HandshakeResponse`. It carries the code itself and binds nothing, so it is refused from any peer
but loopback: `403 pairing_requires_lan` for a tailnet or unclassifiable peer, else
`403 legacy_pairing_host_only`.

### Tokens

- `POST /handshake/refresh` takes `{refresh_token}` and answers `HandshakeResponse` with a new pair.
- `POST /handshake/revoke` revokes the bearer's session and refresh token, queues the device's
  remote-access revocation, and removes the device from the registry: `{"revoked": true}`.
  `401 authentication_required` or `invalid_token`; `503 revocation_unavailable` if the revocation
  could not be queued (nothing is revoked then).

### Pairing codes

`GET /handshake/pairing-code` answers `{code, expires_at, profile_id, pairing}` (`code: null` when
none is live). `POST /handshake/pairing-code` issues a fresh single-use code; its optional body
`{profile_id?, unattributed?}` (unknown fields refused) binds the device that pairs with it to a
member. With no body, a household with exactly one member binds to that member. `pairing` is the
pairing material: `hostname`, `lan_address`, `tailnet_address`, `https_port`, `tls_spki_sha256`.

### `POST /handshake/spoken-code`

No body. The pond issues an ordinary pairing code and reads it aloud twice; no answer ever carries
the code.

| Status | Body | Meaning |
|--------|------|---------|
| 202 | `{"spoken": true, "expires_at": "<RFC 3339>"}` | Reading has started; the code lives 10 minutes |
| 403 | `{"error": "spoken_codes_disabled"}` | `spoken_pairing_codes` is off |
| 403 | `{"error": "pairing_requires_lan"}` | Not a direct LAN or loopback peer |
| 429 | `{"error": "spoken_code_limited", "retry_after": <s>}` | One code per 30 s and ten an hour, whoever asks |
| 503 | `{"error": "announcer_unavailable"}` | The pond has no speaker; no code was issued |

## Onboarding

Steps, in order: `Welcome`, `Basics`, `Location`, `Accessibility`, `Personality`, `GooseIdentity`,
`WakeWord`, `Model`, `Extensions`, `Completed` (`OnboardingStep`,
`crates/pond-core/src/user_data/domain/onboarding.rs`).

| Route | Request | Response |
|---|---|---|
| `GET /onboard/status` | | `{onboarded, current_step, steps_completed, total_steps}`; `current_step` is `not_started` before the first step |
| `POST /onboard` | | `{status: "started", ...}`, `{status: "in_progress", current_step, ...}` or `{status: "already_complete"}` |
| `POST /onboard/step/{name}` | | The status body. Progress only moves forward; an unknown name is `400` with `valid_steps`, and `Completed` is refused in favour of `/onboard/complete` |
| `POST /onboard/complete` | | `{status: "completed"}`; `400` with `missing_fields` while any of `user_name`, `assistant_name`, `timezone`, `chat_model` is empty |
| `POST /onboard/reset` | | The status body at `Welcome` |

After completion, if the setup record's `claimed_by` device is still unattributed and
`primary_profile_id` names an existing member, that device is attributed to the member. A failure
there is logged (`kind="owner_attribution_failed"`) and does not change the answer.

A claimed pond that has lost every token is not stuck: the host can still issue a pairing code, and
`pond-server setup-code --rearm` makes it unclaimed again.

## Connections and setup card

The contract is [architecture/pond-connections.md](architecture/pond-connections.md); the setup
channel is [architecture/pond-setup-channel.md](architecture/pond-setup-channel.md). Every refusal
is `{"error": "<code>"}`. The routes share the setup channel's Wi-Fi station, which runs one radio
operation at a time, so an overlapping scan, join or forget is `409 busy`.

**`GET /network/status`**: `{ble: {enabled, state, reason, session_open}, wifi: {scan, join}}`.
`ble.enabled` is the `ble_setup_enabled` setting (`null` if unreadable); `ble.state` is `off`,
`serving`, `advertising` or `unavailable` with `reason`. A server without the setup channel answers
`unavailable` with `"this pond is not running the setup channel"` for every part.

**`GET /connections`**:

```json
{
  "hostname": "nano",
  "https_port": 4443,
  "wifi": {
    "state": "connected", "reason": null,
    "ssid": "Home", "address": "192.168.1.11", "signal": 70,
    "capabilities": {
      "scan":   {"available": true,  "reason": null},
      "join":   {"available": false, "reason": "..."},
      "forget": {"available": false, "reason": "..."}
    }
  },
  "ethernet": [{"interface": "eth0", "link": "up", "address": "192.168.1.20", "speed_mbps": 1000}],
  "bluetooth": {"enabled": true, "state": "advertising", "reason": null, "advertising_as": "Pond 4e81", "session_open": false},
  "can_change": {"allowed": true, "reason": null}
}
```

`wifi.state` is `connected`, `disconnected` or `unavailable`. `can_change` is about the caller;
what the OS allows is in `wifi.capabilities`. `503 unsupported` when the server has no Wi-Fi
station.

**`GET /connections/wifi/networks`** scans within 15 s: `{networks: [{ssid, security, signal, known, current}]}`,
one entry per SSID, strongest first, at most 50. `403 not_permitted` (macOS without Location
permission), `409 busy`, `503 unsupported`, `504 timeout`.

**`POST /connections/wifi/join`** takes `{ssid, passphrase?, hidden?}` and waits up to 45 s on its
own task, so a join that moves the pond to another network still finishes. `200 {ssid, address, https_port, hostname}`.
Refusals: `400 invalid_request`, `400 unsupported_security` (enterprise networks), `422 wrong_password`,
`403 not_allowed` (this caller) or `not_permitted` (the OS), `404 not_found`, `409 busy`,
`502 no_address`, `503 unsupported`, `504 timeout`. The passphrase is never logged, stored or echoed.

**`DELETE /connections/wifi/networks/{ssid}[?confirm=true]`** deletes every saved profile of that
SSID: `204`; `409 in_use` when the pond is on that network and `confirm` is not `true`.

**`GET /setup/status`**: `{id, claimed, claimed_at, claimed_by_this_device}`, never the code. A
pond with no setup record answers `id: null, claimed: false`.

## Settings

`GET /settings` returns the whole `Settings` struct
(`crates/pond-core/src/user_data/domain/settings.rs`). Every field, its default and where it is
edited is in [architecture/settings-catalogue.md](architecture/settings-catalogue.md).

`PUT /settings` takes any subset of the fields and writes only those keys, so concurrent writers do
not revert each other. It answers with the merged record (floating-point fields echo widened, so
diff against the patch you sent). Side effects apply at once: provider and model changes reload
the model router, privacy and audio-device settings apply live, a Matter edit reconciles the
Matter controller, and the warmed prompt prefix is re-warmed in the background.

| Status | Body |
|---|---|
| 400 | `{"error": "Invalid settings body: ..."}` |
| 422 | `{"error": "...", "fields": [{"field", "message"}]}` from `settings_validation::validate_patch` |
| 422 | `agent_backend: "pond"`: the pond agent is quarantined |
| 422 | `{"error": "...", "field": "matter_ws_url"}` for an unusable Matter URL |

## Chat and runs

### `POST /chat`

`ChatRequest {session_id?, message, images?, voice_mode?, canvas_mode?, tool_group_allowlist?, resumable?}`.
Creates the session when `session_id` is absent. Answers
`{session_id, response, model_role}`. Turn persistence goes through `ChatService`.

### `POST /chat/stream`

The same `ChatRequest`, answered as Server-Sent Events. Every frame is an unnamed SSE event whose
`data` is one JSON object; clients switch on its `type`.

| `type` | Fields | Meaning |
|---|---|---|
| `run_started` | `run_id, session_id, epoch, resumable` | First frame of a resumable run |
| `status` | `content` | Progress ("Compacting context...", "Model starting...") |
| `thinking` | `content` | Reasoning text |
| `text` | `content`, `token` (same value) | Answer text |
| `tool_call` | `tool, id, input` | A tool call |
| `tool_result` | `tool, id, content, ui?, transient?` | Its result; `ui` drives the MCP-UI renderer. `transient: true` (web search): show it in the conversation in view, never store it; history keeps only a stub |
| `subagent_progress` | | A sub-agent's progress |
| `review_status` | `content` | Answer review in progress |
| `review_revision` | `content, score, rounds` | A revised answer replaces the streamed text |
| `turn_limit_reached` | `max_turns` | The agent hit its turn limit |
| `turn_stats` | `ttft_ms, prefill_ms, decode_tok_per_sec, prefill_tok_per_sec, prefilled_tokens, reused_prefix_tokens, prompt_tokens, completion_tokens, reasoning_tokens, context_used_tokens, context_limit_tokens, context_pct, model_load_ms, inference_count, reengagements` | Engine numbers for the turn; `null` means not measured |
| `context_warning` | `utilization_pct, turns_remaining, avg_growth_rate, warning` | The session should be compacted |
| `cancelled` | `run_id, at_seq` | The run was cancelled |

An error frame is `{"error": "..."}` with no `type`. The last frame is
`{"done": true, "interrupted", "run_id", "session_id", "model_role", "model_name", "usage": {prompt_tokens, completion_tokens}}`.

Before the stream opens the route can answer: `400` bad body; `409 {"code":"no_model"}` when no
conversation model is chosen; `409` with `code` `vision_unsupported` or `vision_not_ready` for
images the model cannot read; `413` and `415` (`code: "image_unreadable"`) for images over the
limits in `crates/pond-core/src/models/domain/image_limits.rs` (4 per turn, 4 MiB each, 8 MiB
total; JPEG, PNG, WebP, GIF, BMP); `503` for stream or run caps. `images` is
`[{data: "<base64>", mime_type}]`.

### Resumable runs

With `resumable: true` the turn runs detached: dropping the connection does not stop it, and its
frames carry SSE `id` = sequence number. A client that reconnects:

- `GET /sessions/{session_id}/active-run`: `{run_id, session_id, state, started_at, first_seq, last_seq, epoch}`, or `404`.
- `GET /chat/runs/{run_id}/events?after_seq=N&epoch=E` replays frames after `N` (or after
  `Last-Event-ID`, whichever is larger) and follows the tail. It opens with
  `{type: "reattached", run_id, session_id, from_seq, replay_depth, state, epoch}`; may send
  `{type: "replay_gap", requested_after_seq, first_available_seq, advice: "reload_session_messages"}`
  when the buffer rolled past the client, or `{type: "run_evicted", run_id, advice}` if the run is
  dropped. An `epoch` from before a restart is `410 {"error":"run_lost","reason":"server_restarted",...}`.
- `POST /chat/runs/{run_id}/cancel` and `DELETE /sessions/{session_id}/active-run` stop the run:
  `{run_id, state, at_seq}`. Idempotent.

### `POST /agent/chat/stream`

The agent loop with MCP tools, streamed. The body is read loosely: `message`, `session_id`,
`images`. Frames are the same JSON shapes as `/chat/stream` (`status`, `thinking`, `text`,
`tool_call`, `tool_result`, `turn_stats`, `error`), without run ids or resumption.

### `POST /recipes/{name}/run`

`RunRecipeRequest {session_id?, voice_mode?, canvas_mode?, parameters?}`. The SSE matches
`/chat/stream` frame for frame; never resumable. `400` with `missing` names required parameters.

## Sessions

All chat history lives in `pond_system.db`.

| Route | Request | Response |
|---|---|---|
| `GET /sessions` | | `{sessions: [{id, title, preview, message_count, total_prompt_tokens, total_completion_tokens, model_name, created_at, updated_at}]}` |
| `PATCH /sessions/{session_id}` | `{title}` | `{session_id, title}` |
| `DELETE /sessions/{session_id}` | | `204`, also for a missing session |
| `GET /sessions/{session_id}/messages` | `?limit=` (default 100, max 500), `?offset=` | `{messages: [...]}`. Without `offset`, the newest `limit`; with it, oldest-first from `offset` |
| `DELETE /sessions/{session_id}/messages/{message_id}` | | `204`; the client then resubmits |
| `PUT .../messages/{message_id}/feedback` | `{liked: true \| false \| null}` | `204` |
| `GET /sessions/{session_id}/attachments/{attachment_id}` | | Raw bytes, `Content-Type`, `Cache-Control: private, max-age=31536000, immutable`; `404` if the attachment is not this session's |
| `POST /sessions/{session_id}/compact` | | `200` with `status` and `reason`; `404` only for an unknown session |
| `POST /sessions/retitle` | | `{started, reason}`; `503` without a language model |
| `POST /sessions/{session_id}/retitle` | | `{session_id, outcome, title, reason}` |

A message has `id, session_id, role, content, created_at`, and when they apply: `liked`, tool calls
(`name`, `arguments`), `images: [{id, mime_type, byte_size, url}]` (the `url` is the protected
attachment route, so fetch it with the bearer token rather than a bare `<img src>`), and `thinking`
(reasoning passages, kept while `persist_thinking` is on).

## Voice and audio

Speech synthesis is the in-process Kokoro engine (`AppState.tts`).

| Route | Request | Response |
|---|---|---|
| `POST /tts` | `{text}` | `audio/wav` bytes; `400` empty text, `503 {"error":"No TTS backend is running"}` |
| `POST /voice/tts/apply` | optional `{voice?, speed?, quality?}`; omitted fields come from settings | `{voice, speed, quality, downloaded_voice, downloaded_weights, engine_reloaded, installed_voices}` |
| `POST /voice/sample` | exactly `{voice}` | `202 {"speaking": true}` |
| `GET /voice/tts/status` | | `{available, loaded}` |
| `POST /voice/tts/prewarm`, `/voice/tts/unload` | | `{status}`; `503` without an engine |
| `POST /voice/calibrate` | multipart `audio` (WAV), optional `prompt` | `{transcript, normalized, heard, new_variant, all_variants, sample_count, target_count, complete}` |
| `DELETE /voice/calibrate` | | `{cleared, message}` |
| `POST /transcribe` | multipart `audio` | `{text}` |
| `GET /audio/devices` | | `AudioDeviceStatus {input, output, updated_at_ms, source, pid, raised?, sound_server?}` |
| `POST /audio/devices/test` | empty, or `{chime: bool}` | `AudioDeviceStatus` after a re-test |

`/voice/sample` reads one fixed sentence in an installed voice on the pond's speaker, changing no
setting: `400` for any other body, `404 unknown_voice` (it never downloads one),
`429 voice_sample_limited` (one per 3 s, with `retry_after`), `503 voice_unavailable`.

`/transcribe` and `/voice/calibrate` use the in-process whisper-rs model when the server loaded
one, and otherwise post to `voice_whisper_url` (gated by the network-egress policy; `502` if it
refuses or fails). Wake-phrase calibration is described in
[wake-word-calibration.md](wake-word-calibration.md).

## Models

Categories (`ModelCategory`): `gguf`, `litert`, `llamafile`, `ollama`, `whisper`, `tts_kokoro`,
`tts_piper`, `tts_http`, `embedding`. Roles (`ModelRole`): `chat`, `think`, `task`, `tool`, `asr`,
`tts`, `embedding`. A model's id is `"{category}/{name}"`.

**`GET /models`** answers `{whisper, llamafile, tts, gguf, litert, ollama, embedding}`, each a list
of `ModelStatusEntry` (`crates/pond-api/src/lib.rs`): `id`, `category`, `name`, `title`,
`description`, `size_mb`, `downloaded`, `active`, `url`, `hf_id`, `filename`, `ram_estimate_mb`,
`recommended_role`, `context_length`, `quantization`, `engine`, `provenance`, `kind`, `acquire`,
`recommended`, `companions` (`[{kind: "pictures", label, size_bytes, state}]`), `reads_images`,
`image_support_bytes`, `available` and `unavailable_reason` (for example `litert_library_missing`).
All three `tts_*` categories are listed under `tts`.

| Route | Request | Response |
|---|---|---|
| `GET /models/download/progress` | | `{downloads: [...]}`; finished entries are evicted after 5 minutes, paused ones stay |
| `POST /models/download/control` | `{action: "pause" \| "resume" \| "cancel", filename? \| model_id?}` | `{filename, status}` or `{files}`. Pause keeps the partial file; cancel deletes it |
| `POST /models/{category}/{name}/download` | optional `{pictures: false}` | `{status, name, note?}`; the picture add-on comes too unless refused |
| `POST /models/{category}/{name}/activate` | `{role}` | `{role, model_id}`; mirrored into settings, so the `chat` role sets `chat_model` |
| `POST /models/download/url` | `{url, filename, category?, pictures?}`; `url` must be HTTPS | `202 {status, filename, category}` |
| `POST /models/{category}/{name}/companions/pictures` | | `{status, model_id, size_bytes, parts, message}` |
| `POST /models/{category}/{name}/revoke` | | `{status, category, name}` |
| `DELETE /models/{category}/{name}` | | `204`; `409` while the model holds a role |
| `GET /models/capabilities` | | `ModelCapabilities {thinking, vision, audio_input, context_window_tokens, structured_output, tool_calling}`; see [architecture/model_capabilities.md](architecture/model_capabilities.md) |
| `GET /models/vision-status` | | `{state, model, size_bytes, message}`; a pure read, polled |
| `GET /models/memory-status` | | `{total_mb, available_for_llm_mb, budget_mb, loaded_model, reclaimable_mb}` |
| `GET /models/active-roles` | | `{chat: {provider, model, model_id}, tool: {model}, asr: {model_id}, tts: {model_id}, embedding: {model_id, model, provider}, router_name}` |
| `POST /models/registry/refresh` | | `{status}`; the refresh runs in the background |
| `POST /models/scan` | | `{found, entries}` |
| `POST /models/cleanup` | | `{reclaimed_bytes, removed}` |
| `GET /models/disk-usage` | | `{total_bytes, by_category, hf_cache_bytes, incomplete_bytes}` |
| `GET /models/ollama` | | Ollama's tag list, or `{models: [], error}` |
| `POST /models/ollama/pull` | `{model}` | `202 {status, model}` |
| `GET /models/search/gguf?q=` | | `{models: [{id, downloads, likes, tags, url}]}` |
| `GET /models/search/gguf/files?repo=owner/name` | | `{files: [...]}` |
| `GET /models/search/llamafile?q=` | | `{models: [{name, version, size_mb, url, release_url}]}` |

A download or activation for a model whose engine this pond lacks is refused
(`refuse_unavailable_engine`).

## Profiles and session identity

A profile is a household member: `Profile {id, display_name, avatar_emoji, preferences, created_at, updated_at}`.

| Route | Request | Response |
|---|---|---|
| `POST /profiles` | `CreateProfileRequest {display_name, avatar_emoji?}` | `201` with the profile |
| `PATCH /profiles/{id}` | `{preferences: {key: value}}`, merged | `{id, display_name, preferences, updated_at}` |
| `GET /profiles` | | `{profiles: [...]}` |
| `GET /profiles/{id}` | | the profile, or `404` |
| `DELETE /profiles/{id}` | | `{profile_id, display_name, deleted: {memories, face_embeddings, uber_accounts}, released: {sessions}, cleared_primary_profile}` |

Deleting a member removes their memories, face embeddings and Uber sign-in, and releases their
sessions (the conversation stays, unattributed). Household-scoped memories are untouched. If the
member was `primary_profile_id`, it is cleared first.

Session identity records who a session belongs to and on what evidence. `identification_source`
is `paired_device`, `explicit`, `face` or `unknown`, strongest first; a weaker source never takes
over a session a stronger one bound.

| Route | Request | Response |
|---|---|---|
| `GET /sessions/{session_id}/user` | | `{session_id, profile_id, identification_source, confidence}`; nobody rather than an error for an unknown session |
| `PUT /sessions/{session_id}/user` | `{profile_id}` | `{session_id, profile_id, identification_source, bound}`; `403` when the security policy refuses the assertion |
| `DELETE /sessions/{session_id}/user` | | `{session_id, cleared}` |
| `POST /sessions/{session_id}/identify-user` | multipart as `/faces/identify` | `{session_id, identified, profile_id, confidence, threshold, bound}` |

## Devices, Matter and notifications

A device is `Device {id, name, device_type, hostname, ip_address, capabilities, registered_at, last_seen, is_online, room}`.
`is_online` is derived when read, from `last_seen` within 300 s, so a client that registered
itself must keep calling the heartbeat.

| Route | Request | Response |
|---|---|---|
| `GET /devices/self` | | `{device_id, profile: {id, display_name} \| null}`; `404` when the request did not come from a paired device, `503` when attribution could not be read |
| `POST /devices/{id}/push-token` | `{token, platform: "fcm" \| "apns" \| "expo"}` | `{ok}` |
| `DELETE /devices/{id}/push-token` | | `{ok}` |
| `GET /devices` | | `{devices: [...]}` |
| `POST /devices` | `RegisterDeviceRequest {id?, name, device_type, hostname?, capabilities, room?}` | `201` with the device |
| `PUT /devices/{id}` | `{name?, hostname?, room?}`; left out keeps, `null` clears | the device; `400 name_required`, `403 device_not_yours`, `404 device_not_found` |
| `DELETE /devices/{id}` | | `204`; `409` with `hub_id` for an endpoint bridged by a Matter hub; `502` if the device could not leave the Matter fabric; `503 revocation_unavailable` |
| `POST /devices/{id}/heartbeat`, `/offline` | | `{status}` |
| `POST /devices/commission` | `{code, name?}` | `201 {id, name, node_id}` |
| `GET /matter/status` | | `MatterStatus {enabled, url, state}` |

`PUT /devices/{id}` on a paired client's own row (`device_type` `gotg`, `desktop` or `cli`) is
that device's or the host's to change; a shared row (a Matter device, a sensor, another pond) is
any authenticated caller's (`pond_core::user_data::services::device_edit`).

How a notice reaches a phone, what the Pond notifies about, and how often, is in
[architecture/notifications.md](architecture/notifications.md). The three routes below take a paired
device's token in every onboarding state, never answer a tokenless caller, and act only on the
caller's own device, named by its token.

**`GET /notifications/stream?device_id=X`** first sends the device's undelivered queue, oldest first,
then follows live notifications addressed to it or to `"broadcast"`; dedupe by `id`. `device_id`
must be the caller's own (`403 device_mismatch`). For a phone (`device_type = "gotg"`) the stream
never marks anything delivered, so each new connection re-sends every copy the phone has not acked;
for any other device the stream marks what it sends delivered. Each SSE `data` is a
`Notification {id, target, category, title, body, timestamp, data}` with `category` `alert`,
`info` or `action_required`. `data` may carry:

| Key | Values | Sent by |
|---|---|---|
| `action` | `open_url`, `ride_offer`, `ride_update` | travel; see [architecture/rides-and-maps.md](architecture/rides-and-maps.md) |
| `open` | `schedule` (with `schedule_id`, `run_id`), `devices`, `chat`, `models`, `extensions`, `remote_access` | schedule runs, pairing outcomes, Matter, proposals, system notices |
| `kind` | a system notice's catalogue name, e.g. `model_downloaded`; `pond_warmup` for the live-only warm-up status | system notices, warm-up |

`open` is written only through `OpenScreen` (`crates/pond-core/src/mcp/ports/notification.rs`) and
names the screen a tap opens.

**`GET /notifications/pending`** answers `{notifications: [Notification]}`, the caller's
undelivered notifications oldest first (for a phone, every copy it has not acked, including those
the stream already sent). It takes no parameter and marks nothing delivered. `403` when the request
did not come from a paired device, `500 notifications_pending_failed` when the queue cannot be
read, `503` when no queue is wired.

**`POST /notifications/{id}/ack`** marks one of the caller's notifications delivered and answers
`{id, acknowledged: true}`; acknowledging twice is not an error. Percent-encode `{id}`: a phone's
copy of a household notice is `"{notification_id}:{device_id}"`. `404` when no notification by that
id is the caller's (another device's answers the same, so the route cannot probe someone else's
queue), `500 notification_ack_failed`, `503` when no queue is wired.

## Sensors, camera and rules

| Route | Request | Response |
|---|---|---|
| `POST /sensors` | `{device_id, sensor_type, value, unit}` | `201` |
| `GET /sensors/{device_id}` | `?limit=&sensor_type=&since=&until=&agg=min\|max\|avg\|current` | `{readings, count, truncated}` |
| `POST /camera/events` | `{camera_id, event_type, confidence?, snapshot_path?, metadata?}` | `201 {id}`; also published on `/vision/events/stream` |
| `GET /camera/events` | `?camera_id=&limit=` (camera defaults to `vision_camera_id`, limit to 20, at most 100) | `{events: [CameraEvent]}`, newest first |
| `PATCH /camera/events/{id}/acknowledge` | | `{status}` |
| `GET /camera/events/{id}/snapshot` | | `image/jpeg`; `404` when the event has no snapshot or its file is outside `<data_dir>/snapshots` |
| `GET /cameras` | | `{cameras: [{id, live, last_frame_at, snapshot_url, stream_url}]}`; `live` only with a frame in the last 10 s |
| `GET /camera/snapshot`, `/camera/{camera_id}/snapshot` | | `image/jpeg`: the live frame, else the camera's newest event snapshot, else `404` |
| `GET /camera/stream`, `/camera/{camera_id}/stream` | | `multipart/x-mixed-replace; boundary=frame` (MJPEG); ends when camera consent is revoked |
| `GET /vision/events/stream` | | SSE, `event: vision_event`, `data` one `CameraEvent` (`acknowledged: false`); ends when camera consent is revoked |
| `GET /vision/status` | | `VisionStatus` below |
| `GET /vision/config` | | `{vision_enabled, vision_camera_id, vision_fps, vision_motion_threshold, vision_event_cooldown_secs, vision_classifier_confidence, vision_face_recognition_enabled, vision_face_on_motion, vision_event_types, min_event_interval_secs, classifier_confidence_threshold, face_detection_enabled, face_on_motion, enabled_event_types, supported_signals, active_cameras, event_listener_count}`, values clamped (`vision_fps` to 1..30) |
| `PUT /vision/config` | any of `vision_enabled`, `vision_camera_id`, `vision_fps`, `vision_motion_threshold`, `min_event_interval_secs`, `classifier_confidence_threshold`, `face_detection_enabled`, `face_on_motion`, `enabled_event_types` (or their `vision_*` settings names) | The `GET` shape. Saved to settings, so it survives a restart and `GET /settings` shows it |

`CameraEvent` is `{id, camera_id, event_type, confidence, snapshot_path, snapshot_url, metadata,
acknowledged, created_at}`. `snapshot_url` is `/api/v1/camera/events/{id}/snapshot`, or null when
the event has no snapshot. `metadata` is an object or null; pond vision events carry
`{changed_fraction, source, face_detected, face_unknown, identified_profile?, face_confidence?}`.

With `cameras_enabled` off, the snapshot, stream, event-snapshot and SSE routes and
`POST /faces/register-from-camera` answer `403 {"error", "reason": "cameras_disabled"}`, read from
the stored setting on every request. The `?token=` form of the session token works on the three
image and stream `GET` families only (see [Credentials](#credentials)).

`VisionStatus`:

```json
{
  "cameras_enabled": true,
  "vision_enabled": true,
  "state": "running",
  "camera_id": "camera-1",
  "source": "rtsp://***@192.168.1.20/live",
  "source_kind": "configured",
  "fps": 2,
  "capture": "hardware",
  "last_frame_at": "2026-10-26T12:00:01Z",
  "last_event_at": "2026-10-26T11:58:40Z",
  "last_error": null,
  "restarts": 0,
  "classifier": { "available": true },
  "faces": { "available": true, "status": "ready", "enrolled_people": 2 },
  "event_listeners": 0
}
```

`state` is `consent_off` (`cameras_enabled` false; it wins), `off` (`vision_enabled` false),
`no_camera` (nothing configured, nothing discovered), `starting`, `running` (frames arriving) or
`reconnecting` (the stream failed; `last_error` says why). `source` has URL credentials replaced by
`***`; `source`, `source_kind` and `last_error` are null unless the pipeline is starting, running or
reconnecting. `capture` is how frames reach the pipeline, null unless it is starting or running:
`hardware` (a Jetson's engines through GStreamer, for whichever format the camera offers: NVJPG
decodes a USB or network camera's own MJPEG; NVDEC decodes H.264 or H.265 from a USB camera or
over RTSP; VIC converts a USB camera's raw YUY2, UYVY, NV12, I420 or GRAY8 frames; a CSI sensor
comes through Argus and the ISP; VIC scales, and NVJPG encodes the live JPEG when the camera sent
none), `camera_jpeg` (the camera's own MJPEG through ffmpeg, never re-encoded; motion decodes its
luma on the CPU) or `software` (ffmpeg decodes every frame and the pond encodes the JPEG). They
are tried in that order. A camera offering several formats gets a `hardware` attempt for each,
cheapest first (MJPEG, then H.264/H.265, then raw, then a CSI sensor), and a path is kept once it
delivers a frame within 10 s. `restarts` counts pipeline restarts
since the server started: a stream that ended, or a capture setting (camera address, camera name,
fps, motion threshold) that changed; a restart waits up to 3 s for the old capture process to exit,
so the camera is free when it reopens. The vision supervisor
(`crates/pond-adapters-vision/src/supervisor.rs`) re-reads settings every 2 s, and at once after
`PUT /settings` or `PUT /vision/config`, so a change applies within seconds; it logs each restart
and stop at `info` with its reason.

A rule is a schedule whose kind is `SensorTrigger`. Unlike `/schedules`, the rule routes validate
the spec and refuse to reach a cron schedule by id. The body is `ApiRuleRequest {id?, name, ...SensorTriggerSpec}`
with the spec flattened: `source` (`kind`: `sensor`, `camera` or `device`), `condition`, `actions`
(`agent_prompt`, `device_power`, `notify`) and `cooldown_secs`. Create answers `201`; the reads and
updates answer the rule; delete, pause and resume answer `{deleted}`, `{paused}`, `{resumed}`.
See [architecture/scheduling.md](architecture/scheduling.md).

## Schedules

Every schedule route answers `503 {"error":"scheduler not configured"}` when the scheduler did not
start. A task is `Schedule {id, label, cron, fire_at, timezone, kind, paused, currently_running, last_run, next_run, created_at}`;
`kind` is tagged by `type`: `agent_prompt {prompt}`, `webhook {webhook_url}` or `sensor_trigger`.
`cron` is six fields with a leading seconds field.

| Route | Request | Response |
|---|---|---|
| `GET /schedules` | | `[Schedule]` |
| `POST /schedules` | `ApiCreateScheduleRequest {id?, name (alias label), cron, prompt?, timezone?, kind?, payload?, fire_at?, once?}` | `201` with the task |
| `GET /schedules/upcoming` | `?limit=` | active tasks by next fire |
| `PUT /schedules/{id}` | `ApiUpdateScheduleRequest {name?, cron?, timezone?, prompt?, fire_at?, once?}` | the task |
| `DELETE /schedules/{id}` | | `{deleted}` |
| `POST /schedules/{id}/pause` | | `{paused}` |
| `POST /schedules/{id}/resume` | | `{resumed}`; `409 code: "one_shot_time_passed"` for a one-off whose time passed while paused |
| `POST /schedules/{id}/run-now` | | `202 {fired}` |
| `GET /schedules/{id}/runs` | `?limit=` | `[ScheduleRun {id, schedule_id, status, result, error, started_at, finished_at, duration_ms}]` |
| `GET /schedules/events` | | SSE; each `data` is `ScheduleResultEvent {schedule_id, schedule_label, run_id, status, result, error, duration_ms}` |

The task kind comes from `kind`, else `payload.webhook_url`, else `prompt` (or `payload.prompt`). A
one-off is `fire_at` (an RFC 3339 instant, stored with cron `@once`), or `once: true` with a `cron`
whose next occurrence is the instant; `fire_at` already past is `400 code: "fire_at_in_past"`. A
`sensor_trigger` kind is validated as `/rules` validates it.

## Memory, reminders, suggestions and proposals

See [architecture/memory_system.md](architecture/memory_system.md).

| Route | Request | Response |
|---|---|---|
| `GET /memories` | | `[MemoryFragment]`, the 50 most recent |
| `POST /memories` | `SaveMemoryRequest {content, tags?, source? (default "api"), segment?, importance?, tier?}` | `201` with the fragment (household-scoped; embedded at write, or by the sweep when the embedder fails) |
| `PUT /memories/{id}` | `{content?, tier?}`; at least one | `204`; new `content` is re-embedded, a tier-only change keeps the vector |
| `DELETE /memories/{id}` | | `204` |
| `GET /memories/extraction-status` | | `{sessions_total, sessions_pending, mode, last_pass_at, last_pass_*, unattributed_sessions, blocked_on, running}` |
| `POST /memory/consolidate` | | SSE of `ConsolidationEvent`, tagged by `type`: `started`, `proposer_done`, `adversary_done`, `judge_done`, `applied`, `completed`, `error`, `cancelled`; `501` when consolidation is not built in or not enabled |
| `POST /memory/consolidate/stop` | | `200` |
| `GET /reminders` | `?limit=&session_id=` | `{reminders, limit}`, pending only, newest first |
| `POST /reminders/{id}/dismiss` | `?session_id=` | `{id, disposition}`; `404` when nothing moved |
| `GET /suggestions` | `?session_id=` | `{suggestions: [{id, prompt, because, answered_by, composed}], considered, audience}` |
| `POST /suggestions/{id}/taken` | | `{id, settled}`; an unknown id is `200` with `false` |
| `GET /proposals` | `?session_id=` (required) | `{profile_id, proposals}`, live only, newest first |
| `POST /proposals/{id}/decide` | `{session_id, decision: "approve" \| "reject"}` (unknown fields refused) | `{id, status, executed}`; approving executes nothing. `403` when the caller may not decide it, `404` no live proposal, `409` cannot move to that status |

`MemoryFragment` fields: `id, profile_id, session_id, content, source, tags, created_at, segment,
importance, tier, decay_rate, access_count, last_accessed_at, lifecycle, superseded_by, corrects`.
`tier` is `short`, `long` or `permanent`; re-tiering resets the decay rate, and rewording replaces
the embedding with the new text's, in the row and in the personal-context index. A save or edit
whose text held a credential is stored redacted and without a vector, for the sweep to embed.

## Personal context

See [architecture/personal-context-index.md](architecture/personal-context-index.md). Reads and
writes are scoped to the member the caller resolves to; a guest cannot create a source.

| Route | Request | Response |
|---|---|---|
| `GET /context/sources` | `?session_id=` | `{sources: [{id, kind, provider, profile_id, status, last_sync, needs_credentials, items, awaiting_index}]}` |
| `POST /context/sources` | `{kind, provider, session_id, credentials?: {username, password, base_url?}}` | `{id, kind, profile_id}` |
| `DELETE /context/sources/{id}` | `?session_id=` | `{id, items_removed}` |
| `POST /context/sync` | | the sync summary; `503` when the pond has no encrypted secret store to read sign-ins from |
| `GET /context/items` | `?session_id=&q=` | `{items: [{id, source_id, source_kind, kind, title, body, occurred_at, participants, searchable}]}` |
| `GET /context/index/health` | | `{indexed, reason, model_id, dims, coverage, corpora: [...]}` |
| `POST /context/index/rebuild` | | `{cleared, corpora, refilling}` |

## Activity, logs and telemetry

| Route | Request | Response |
|---|---|---|
| `GET /activity` | `?since=&until=&category=&session_id=&limit=` | `{count, events}`; secret events are never returned |
| `DELETE /activity` | the same filters; none purges everything | `{purged}` |
| `GET /activity/summary` | `?window=` | `{window, since, total, by_category}` |
| `GET /security/policy-report` | `?window=` | `{window, since, events, allow, would_deny, deny, unclassified, scanned, truncated, process: {...}}` |
| `GET /logs` | `?limit=&level=INFO\|WARN\|ERROR` | recent `event_log` rows |
| `GET /logs/export` | | `text/csv` |
| `GET /telemetry/turns` | `?session_id=` | `{turns: [...]}`; `503` when telemetry is off |
| `GET /telemetry/summary` | `?session_id=` | an aggregate for the session |

## Extensions, marketplace, secrets and sign-in

| Route | Request | Response |
|---|---|---|
| `GET /extensions` | | `{extensions: [ExtensionInfo {name, kind, description, tools, enabled, status, last_error, status_reason}]}` |
| `POST /extensions` | `AddExtensionRequest {name, kind, description?, command?, args?, env?, uri?}` | `201`; persisted, so it reconnects on restart |
| `PATCH /extensions/{name}` | `{enabled}` | `{name, enabled}`; applies to future agent sessions |
| `DELETE /extensions/{name}` | | `204` |
| `GET /extensions/{name}/secrets` | | `{requirements, fulfilled, values}` |
| `POST /extensions/{name}/secrets` | `{"KEY": "value", ...}`; keys are global | `{stored, restarted, restart_error}` |
| `GET /marketplace` | | `{extensions: [...]}` |
| `POST /marketplace/{id}/install` | optional `{secrets: {...}}` | `201`; `428` with `missing` lists required secrets |
| `GET /secrets` | | `{keys}` |
| `PUT /secrets/{key}` | `{value}` | `{key, stored, restarted, restart_errors}` |
| `DELETE /secrets/{key}` | | `204` |
| `GET /secrets/{key}/exists` | | `{key, exists}` |
| `GET /web-search` | | `{provider, configured, status, local_results: {enabled, town}}`; `town` is what a search would send with local places on, `null` without a place name ([small-model-tools.md §8.10.5](architecture/small-model-tools.md)) |
| `PUT /web-search/key` | `{key}` | `{configured, status}`; one Brave request first, `422` if Brave refused the key |
| `DELETE /web-search/key` | | `204` |
| `GET /oauth/providers` | | `{providers: [{id, display_name, scopes}]}` |
| `POST /oauth/authorize` | `{provider, extension_id?, origin?}` | `{auth_url, state}` |
| `GET /oauth/callback` | `?code=&state=` from the provider | an HTML page; the query is never logged |
| `GET /oauth/status/{state}` | | `{status: "pending" \| "completed" \| "failed" \| "unknown", error?}` |
| `POST /oauth/refresh` | `{provider}` | `{refreshed}` |

Secret keys starting with `UBER_` are reserved for the Uber routes: the generic secret routes answer
`403` with `"code": "secret_reserved"` for them, and `GET /secrets` leaves them out. `origin` on
`/oauth/authorize` sets where the provider sends the browser back; see
[architecture/music-player.md](architecture/music-player.md).

## Agent, tools, prompts, skills and recipes

| Route | Request | Response |
|---|---|---|
| `GET /agent/tools` | | `[{extension, name, description, ui?, _meta?}]`; `[]` without an agent |
| `POST /tools/invoke` | `InvokeToolRequest {server?, tool, args?}` | the tool's result |
| `POST /mcp/tools/call` | `McpToolCallRequest {name, arguments?}` | the tool's result |
| `GET /mcp/resources` | `?uri=` | `{contents: [{uri, text, ...}]}` |
| `GET /agent/extras` | | `[PromptExtra {key, instruction, active, sort_order}]` |
| `POST /agent/extras` | `UpsertExtraRequest {key, instruction, active? (true), sort_order? (0)}` | `{key, status: "ok"}` |
| `DELETE /agent/extras/{key}` | | `204` |
| `GET /prompts` | | `[PromptTemplate {name, content, description, is_system, is_customized, factory_version, updated_at}]` |
| `GET /prompts/{name}` | | the template |
| `PUT /prompts/{name}` | `{content, description?}` | the template |
| `DELETE /prompts/{name}` | | `204`; `403` for a built-in |
| `POST /prompts/{name}/reset` | | the factory template; `400` unless `balanced`, `concise`, `technical` or `warm` |
| `GET /skills` | | `[UserSkill {id, name, description, icon, content, active, created_at}]` |
| `POST /skills` | `{name, description?, icon?, content}` | `201` with the skill |
| `PUT /skills/{id}` | `{name?, description?, icon?, content?, active?}` | the skill |
| `DELETE /skills/{id}` | | `204` |
| `GET /recipes` | | `[AgentRecipe {id, name, description, yaml, active, created_at} + title, parameters, extensions, activities]` |
| `POST /recipes` | `{name, description?, yaml}` | `201` |
| `PUT /recipes/{id}` | `{description?, yaml?, active?}` | the recipe |
| `DELETE /recipes/{id}` | | `204` |

Prompt templates, extras, skills and recipes answer `501` when their repository is not wired.

Extras are short household notes added to the system prompt each turn while `active`, in
`sort_order` then `key` order, each as `<extension-notes name="{key}">{instruction}</extension-notes>`.
The rules, from `crates/pond-core/src/user_data/domain/prompt_extra.rs`, are enforced on write
(`400` with `code`) and at render time (a breaking row is skipped):

| `code` | Rule |
|---|---|
| `invalid_key` | `key` matches `^[a-z0-9_]{1,64}$` |
| `empty_instruction` | `instruction` is not blank |
| `instruction_too_long` | at most 1,000 characters |
| `too_many_extras` | at most 20 active |
| `extras_too_long` | active instructions total at most 4,000 characters |

Skills are listed by name and description in one `<extension-notes name="skills">` block each turn;
their content loads on demand through a tool, and they do not count against the extras limits.

## Music player

The player page (`/player.html` in the web UI) holds a long-lived `GET /player/events?service=`
stream with named SSE events: `ready` (`{service}`) once, then `command` for each command an
extension sends. The page answers with `POST /player/reply {id, ok, result?, error?, code?}` and
reports state with `POST /player/state {service, state}`. Extensions call
`POST /player/command {service, op, args?, timeout_ms?}` with the internal token and wait for the
reply; a failure the caller can act on is `200 {ok: false, code, error}`. See
[architecture/music-player.md](architecture/music-player.md).

| Route | Request | Response |
|---|---|---|
| `GET /musickit/developer-token` | `?probe=true` | `{token, expires_at}`, or with `probe` `{available}` |
| `GET /player/state` | `?service=` | `{attached, state}` |
| `GET /player/status` | | `{attached, configured}` |
| `GET /player/user-token` | `?service=spotify&refresh=` | `{token}` |
| `POST /player/egress-policy` | `{url, method?}` | whether the network setting allows it |
| `POST /extension/egress` | `{url, method?, extension}` | the same, logged as `giap-<extension>` |
| `GET /music/now-playing` | | `{connected, service, player, playing, ...}`, or `204` |
| `POST /music/control` | `{action: "play" \| "pause" \| "next" \| "previous"}` | `{ok}`; `404` Spotify not connected, `409` no active device |

## Rides and Uber accounts

A member books a ride from their own paired phone; the member is the one the phone's pairing
belongs to, never a request field. Another member's ride answers `404`. The desktop, and a phone
whose pairing names no member, get `403`. Without travel switched on (`ext_travel_enabled`) and Uber
sign-in support at startup every ride route is `503`; while travel is off, quote and confirm are
`503` and the rest still work. See [architecture/rides-and-maps.md](architecture/rides-and-maps.md).

| Route | Request | Response |
|---|---|---|
| `POST /rides/quote` | `{pickup: {latitude, longitude, name?}, dropoff: {...}}` | the ride: `{id, provider, pickup, dropoff, fare: {display, currency_code, expires_at}, pickup_eta_mins, state}`; `422` for a drop-off more than 150 km away |
| `GET /rides/{id}` | | the ride |
| `POST /rides/{id}/confirm` | | the ride, `state.state: "requested"`; `202` when Uber's answer was lost (`outcome_unknown`; the pond keeps checking and never resends); `409` already decided; `410` fare expired; `502` Uber refused |
| `POST /rides/{id}/decline` | | `204` |
| `POST /rides/{id}/cancel` | | `204`; `409` unless booked |

`state.state` is `awaiting_confirmation`, `requesting`, `requested` (with `ride`),
`outcome_unknown` (with `reason`), `declined` or `failed` (with `reason`). While a ride is under way
the pond reads it every `GIAP_RIDE_POLL_SECS` (default 15, at least 5) and sends a `ride_update`
notification on each change.

Each member connects their own Uber account through the credentials service
(`POND_CREDENTIALS_URL`, see [architecture/pondcredentials.md](architecture/pondcredentials.md));
tokens stay in this pond's secret store. Every Uber route needs a loopback peer and the host
credential; a refusal is `{error, code}`, with `code` `host_only` off this machine.

| Route | Request | Response |
|---|---|---|
| `POST /uber/accounts/connect` | `{profile_id, origin?}` | `{auth_url, state}`; follow with `GET /oauth/status/{state}`. `404 not_a_member`, `502 uber_relay_failed`, `503 no_secret_store` or `uber_sign_in_off` |
| `GET /uber/accounts` | | `{connected: [profile_id]}` |
| `DELETE /uber/accounts/{profile_id}` | | `204` |

## Faces

Face routes answer `503` until recognition is loaded, except `GET /faces` and `GET /faces/models`,
which answer `200` with `available: false` and a `status` of `not_built`, `downloading` (with
`progress`) or `unavailable` (with `reason`). The 503 says which in `code`:
`face_models_downloading` (with `progress`; recognition switches on by itself when the models
land, no restart) or `face_recognition_unavailable` (with `reason`); a build without `face-onnx`
has no `code`. Images are multipart `image` fields; see [face-recognition.md](face-recognition.md).

| Route | Request | Response |
|---|---|---|
| `GET /faces` | | `{available, status, progress?, reason?, default_threshold, people: [{profile_id, name, enrollments, threshold, last_enrolled_at}]}`; every household profile, enrolled or not; `200` with `available: false` and no thresholds until recognition is loaded |
| `POST /faces/register` | multipart `profile_id`, `image`, optional `bbox` | `{id, profile_id, model_dims, created_at}` |
| `POST /faces/register-from-camera` | `{profile_id, camera_id?}` | `{id, profile_id, model_dims, created_at}` from the camera's latest frame (at most 10 s old); `403 reason: "cameras_disabled"`, `404` unknown profile, `409 reason: "no_frame"`, `422 reason: "no_face"` |
| `POST /faces/identify` | multipart `image`, optional `bbox` | `{identified, profile_id, confidence, threshold}` |
| `POST /faces/identify-burst` | multipart, several `image` frames, optional `bbox` | `{identified, profile_id, liveness, frames_total, per_frame, votes, ...}` |
| `POST /faces/enroll-quality` | multipart | `{ok, reason, matches_existing, matched_profile, existing_samples, recommended_min_samples, ...}` |
| `GET /faces/profile/{profile_id}` | | `{enrollments, count}`, never the vectors |
| `GET/PUT/DELETE /faces/profile/{profile_id}/threshold` | PUT `{threshold}` | `{profile_id, threshold, global_threshold?}` |
| `GET /faces/models` | | `{models: [...], models_dir}` |
| `GET /faces/debug/pairwise`, `/faces/debug/eval` | | calibration diagnostics |
| `DELETE /users/{profile_id}/biometrics` | | `{profile_id, face_embeddings_deleted}` |

## Mesh

| Route | Request | Response |
|---|---|---|
| `GET /mesh/self` | | `{mesh_enabled, peer_id, invite_url}` |
| `GET /mesh/peers` | | `{peers: [{peer_id, trust_scope, connected, credit_balance_millisats}]}` |
| `POST /mesh/peers` | `{peer_id, trust_scope, address?}` | `201 {peer_id, trust_scope}` |
| `DELETE /mesh/peers/{peer_id}` | | `204` |
| `POST /mesh/peers/{peer_id}/credit` | `{amount_millisats}` | `{peer_id, credit_balance_millisats}`; `404` for an untrusted peer |
| `GET /mesh/peers/{peer_id}/capabilities` | | `{peer_id, inference_available, lightning_available}`; `503` with the mesh off |
| `GET /mesh/settlement` | | `{configured, millisats_per_token, peers: [...]}` |

## Remote access

Served by `pond-server`, not `pond-api`. The design is [remote-access.md](remote-access.md).

**Management** (`embedded_network::management`, merged into the dashboard listener): every route
needs a loopback peer and the host credential, and none takes a bearer token.

| Route | Request | Response |
|---|---|---|
| `GET /api/v1/remote-access` | | `Status {state, addresses, authUrl?, nodeKey, machineKey}` |
| `POST /api/v1/remote-access` | `Config {enabled, controlUrl?, enrollmentUrl?}` (unknown fields refused) | `{accepted: true}`; `503` if the node could not start |
| `DELETE /api/v1/remote-access` | | `{accepted: true}` |
| `POST /api/v1/remote-access/identity` | | the authority identity |
| `POST /api/v1/remote-access/register` | optional `{invite?}` (unknown fields refused) | the registration outcome |
| `GET /api/v1/remote-access/device` | | `{provisioned, serial?, registered}` |
| `GET /api/v1/remote-access/recovery-requests` | | pending and recently refused recoveries |
| `POST /api/v1/remote-access/recovery-requests/{id}` | | `{approved: true}` |

**Companion** (`embedded_network::companion_management`, merged into the HTTPS listener): the
bearer-token middleware applies, with its own 20-a-minute limit and a 4 KiB body.

| Route | Request | Response |
|---|---|---|
| `GET /api/v1/remote-access/configuration` | | `{enabled, controlUrl, state, lapsesAt}` for a paired device; `403` otherwise |
| `POST /api/v1/remote-access/enrollment` | `{authUrl, nodeKey, machineKey}` (unknown fields refused) | the enrollment; `409` while a revocation is pending |
| `POST /api/v1/remote-access/recovery` | | `{id}` for the host to approve |
| `GET/POST/DELETE /api/v1/remote-access/recovery/{id}` | | poll, execute (after approval) or cancel |

Enrollment and recovery need a direct LAN peer (`403` otherwise).
