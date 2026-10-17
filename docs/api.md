# GIAP REST API Reference

> Base URL: `http://pond.<HOSTNAME>.local:<PORT>/api/v1`
> Last updated: April 2026

---

## Global Concerns

### Authentication

Protected routes require a Bearer token in the `Authorization` header:

```
Authorization: Bearer <token>
```

Obtain a token via `POST /api/v1/handshake`. The token format is validated by middleware, but semantics are not yet enforced — any well-formed Bearer string passes. This is a known gap (see Known Bugs in AGENTS.md).

### Onboarding Gate

Protected routes return `403 Forbidden` until `OnboardingStep::Completed` is recorded. Public routes are always accessible. Settings write (`PUT /settings`) and profile create/patch are public specifically so onboarding steps can persist data before completion.

### Rate Limiting

Remote clients (non-loopback): **600 req / 60 s** (≈10 req/s burst). Loopback clients (local dashboard, CLI) are never rate-limited.

### Response Format

All errors return JSON:
```json
{ "error": "<description>", "status": <http_code> }
```

---

## Route Index

| Method | Path | Auth | Summary |
|--------|------|------|---------|
| GET | /health | Public | Service health check |
| POST | /handshake | Loopback only (legacy) | GIAP ↔ GOTG authentication handshake |
| POST | /handshake/pake/init | Public, direct LAN peers | Phone pairing (CPace), phase 1 |
| POST | /handshake/pake/verify | Public, direct LAN peers | Phone pairing (CPace), phase 2 |
| POST | /handshake/init | Loopback only | Dashboard pairing (HMAC), phase 1 |
| POST | /handshake/verify | Loopback only | Dashboard pairing (HMAC), phase 2 |
| POST | /onboard | Public | Start first-run onboarding flow |
| POST | /onboard/complete | Public | Mark onboarding finished |
| GET | /onboard/status | Public | Onboarding progress |
| GET | /system/info | Public | Connection bootstrap; full record only with a valid bearer |
| GET | /test | Public | Probe all backend services |
| POST | /test/speak | Loopback + host credential | Play text on server speakers |
| GET | /dev/goose | Public | Goose agent + MCP tool status |
| POST | /transcribe | Public | Proxy audio to Whisper ASR |
| GET | /settings | Protected | Get all settings |
| PUT | /settings | Public | Update settings (partial) |
| POST | /chat | Protected | Single-turn agent conversation |
| POST | /chat/stream | Protected | Streaming agent conversation (SSE) |
| POST | /tts | Token, or loopback + host credential | Synthesise speech (returns WAV) |
| GET | /sessions | Protected | List conversation sessions |
| PATCH | /sessions/{id} | Protected | Rename session |
| GET | /sessions/{id}/messages | Protected | Get session messages |
| GET | /sessions/{id}/attachments/{attachment_id} | Protected | Raw bytes of one chat-image attachment |
| GET | /models | Protected | List model catalog |
| GET | /models/capabilities | Protected | Active model's runtime capabilities |
| GET | /models/active-roles | Protected | Current role assignments |
| GET | /models/memory-status | Protected | LLM RAM budget |
| POST | /models/registry/refresh | Protected | Refresh online model catalog |
| POST | /models/scan | Protected | Scan filesystem for new models |
| GET | /models/download/progress | Protected | In-progress download status |
| POST | /models/download/url | Protected | Download model from arbitrary URL |
| POST | /models/{category}/{name}/download | Protected | Download catalogued model |
| POST | /models/{category}/{name}/activate | Protected | Assign model to a role |
| DELETE | /models/{category}/{name} | Protected | Delete model file from disk |
| GET | /models/ollama | Protected | List models from local Ollama |
| POST | /models/ollama/pull | Protected | Pull Ollama model |
| GET | /models/search/gguf | Protected | Search HuggingFace for GGUF models |
| GET | /models/search/gguf/files | Protected | List GGUF files in a HF repo |
| GET | /models/search/llamafile | Protected | Search GitHub for llamafile releases |
| POST | /profiles | Public | Create household profile |
| PATCH | /profiles/{id} | Public | Update profile preferences |
| GET | /profiles | Protected | List all profiles |
| GET | /profiles/{id} | Protected | Get a profile |
| DELETE | /profiles/{id} | Protected | Delete a profile |
| GET | /devices | Protected | List registered devices |
| GET | /devices/self | Protected | The calling device, and the household member it belongs to |
| POST | /devices | Protected | Register a device |
| PUT | /devices/{id} | Protected | Rename a device, or set its hostname and room |
| DELETE | /devices/{id} | Protected | Unregister a device |
| POST | /devices/{id}/heartbeat | Protected | Update device last-seen |
| POST | /sensors | Protected | Record a sensor reading |
| GET | /sensors/{device_id} | Protected | Recent sensor readings |
| POST | /camera/events | Protected | Record a camera event |
| GET | /camera/events | Protected | List camera events |
| PATCH | /camera/events/{id}/acknowledge | Protected | Dismiss a camera alert |
| GET | /schedules | Protected | List scheduled tasks |
| POST | /schedules | Protected | Create a scheduled task |
| DELETE | /schedules/{id} | Protected | Delete a scheduled task |
| POST | /schedules/{id}/pause | Protected | Pause a scheduled task |
| POST | /schedules/{id}/resume | Protected | Resume a paused task |
| POST | /schedules/{id}/run-now | Protected | Fire a task immediately |
| GET | /extensions | Protected | List loaded MCP extensions |
| POST | /extensions | Protected | Add an MCP extension |
| DELETE | /extensions/{name} | Protected | Remove an MCP extension |
| PATCH | /extensions/{name} | Protected | Enable or disable an MCP extension |
| GET | /marketplace | Protected | List marketplace extensions |
| POST | /marketplace/{id}/install | Protected | Install a marketplace extension |
| GET | /prompts | Protected | List prompt templates |
| GET | /prompts/{name} | Protected | Get a prompt template |
| PUT | /prompts/{name} | Protected | Create or update a prompt template |
| DELETE | /prompts/{name} | Protected | Delete a user-defined template |
| GET | /agent/extras | Protected | List system-prompt extras |
| POST | /agent/extras | Protected | Create or update a prompt extra |
| DELETE | /agent/extras/{key} | Protected | Remove a prompt extra |
| GET | /agent/tools | Protected | List available MCP tools |
| GET | /memories | Protected | List memory fragments |
| POST | /memories | Protected | Save a memory fragment |
| PUT | /memories/{id} | Protected | Reword a memory, change its tier, or both |
| DELETE | /memories/{id} | Protected | Delete a memory fragment |
| GET | /skills | Protected | List user skills |
| POST | /skills | Protected | Create a skill |
| PUT | /skills/{id} | Protected | Update a skill |
| DELETE | /skills/{id} | Protected | Delete a skill |
| GET | /recipes | Protected | List agent recipes |
| POST | /recipes | Protected | Create a recipe |
| PUT | /recipes/{id} | Protected | Update a recipe |
| DELETE | /recipes/{id} | Protected | Delete a recipe |
| POST | /rides/quote | Protected, member's phone | An upfront fare from the phone's location; books nothing |
| GET | /rides/{id} | Protected, member's phone | A quoted or booked ride |
| POST | /rides/{id}/confirm | Protected, member's phone | Book the ride at the quoted fare |
| POST | /rides/{id}/decline | Protected, member's phone | Turn the fare down |
| POST | /rides/{id}/cancel | Protected, member's phone | Cancel a booked ride |
| GET | /uber/accounts | Protected, host only | Members who have connected Uber |
| POST | /uber/accounts/connect | Protected, host only | Start a member's Uber sign-in |
| DELETE | /uber/accounts/{profile_id} | Protected, host only | Forget a member's Uber sign-in on this pond |

---

## System

### GET /health

Health check. Always returns 200 even before onboarding.

**Response 200**, to a loopback peer on the host (the dashboard, the desktop app):
```json
{ "status": "ok", "version": "0.1.0" }
```

**Response 200**, to anyone else, and on the plaintext development listener even from loopback:
```json
{ "status": "ok" }
```

---

### GET /system/info

Public, so a phone can find the Pond before pairing, but it answers by authentication (`system_info`).

**Response 200**, without a valid bearer:
```json
{
  "https_port": 4443,
  "tailnet_address": "100.64.0.7",
  "protocol": 3
}
```

`protocol: 3` (CPace pairing over pinned HTTPS, 2026-10-13) appears only in this anonymous
answer. A client built for another version should stop and say the Pond or the app needs an
update; version 2's HMAC pairing is no longer served to phones.

**Response 200**, with a valid bearer:
```json
{
  "hostname": "my-pond",
  "lan_address": "192.168.1.20",
  "tailnet_address": "100.64.0.7",
  "https_port": 4443,
  "tls_spki_sha256": "sha256/...",
  "port": 4000,
  "version": "0.1.0",
  "platform": "linux",
  "arch": "aarch64"
}
```

`lan_address` and `tailnet_address` are `null` when the Pond has no such route. Pairing material (the addresses and the `tls_spki_sha256` pin) is returned as `pairing` by the host-only `/handshake/pairing-code`, `GET` and `POST`.

`"insecure_dev": true` appears only while the debug-build plaintext development
listener runs (`POND_DEV_INSECURE_LAN=1`); the key is absent otherwise. See
[the security posture](auth-network-posture.md).

---

### GET /test

Probes all backend services in parallel and reports latency. Intended for the diagnostic page and healthcheck scripts.

**Response 200**
```json
{
  "whisper":   { "status": "ok", "url": "http://127.0.0.1:9000", "latency_ms": 12 },
  "llamafile": { "status": "unavailable", "url": "http://127.0.0.1:8080", "latency_ms": null },
  "ollama":    { "status": "ok", "url": "http://127.0.0.1:11434", "latency_ms": 5 },
  "llm":       { "status": "ok", "provider": "ollama", "response": "pong", "latency_ms": 340 }
}
```

`llm.status` values: `"ok"` | `"error"` | `"not_configured"`

---

### POST /test/speak

Speaks a text string on the server device using the configured TTS engine. Intended for voice pipeline testing.
Needs `X-Pond-Host-Credential` from every caller (2026-10-12): `403 host_credential_required`
without it, `503 host_credential_unavailable` on the companion listener, which holds none.
`POND_DEV_ALLOW_LOOPBACK` admits a request without it in development.

**Request** *(optional body)*
```json
{ "text": "Hello from Goose In A Pond!" }
```

**Response 200**
```json
{
  "status": "ok",
  "text": "Hello from Goose In A Pond!",
  "latency_ms": 280
}
```

`status` values: `"ok"` | `"error"` | `"unavailable"`

---

### GET /dev/goose

Reports Goose agent status and loaded MCP tools. Useful for verifying the GIAP builtin extension loaded correctly.

**Response 200 — agent inactive**
```json
{ "goose_active": false, "message": "Goose agent not configured" }
```

**Response 200 — agent active**
```json
{
  "goose_active": true,
  "extension_count": 1,
  "extensions": [{ "name": "giap", "kind": "builtin", "tools": ["giap__get_current_weather", "..."] }],
  "tool_count": 9
}
```

---

## Authentication & Onboarding

### POST /handshake

Exchanges a session token between a GOTG mobile client and GIAP. Returns connection details.

Legacy single-step pairing. It carries the code in the body and binds nothing, so it is refused from any peer but loopback: a tailnet or unclassifiable peer gets `403 {"error":"pairing_requires_lan"}`, and any other non-loopback peer `403 {"error":"legacy_pairing_host_only"}` (`handshake_handler`). Phones pair with CPace on `/handshake/pake/init` and `/handshake/pake/verify`, below.

> **Status**: Token validation is real. `SqliteHandshakeAdapter::validate_token` (`crates/pond-infra/src/sqlite_handshake.rs`) looks the token up by SHA-256 hash and accepts it only when it is unrevoked and unexpired, touching `last_seen_at` on success. Tokens are hashed at rest. Phones pair with CPace; the loopback dashboard with a two-phase HMAC challenge.

**Request**
```json
{
  "device_name": "Jerry's Phone",
  "device_type": "gotg",
  "client_version": "1.0.0"
}
```

**Response 200**
```json
{
  "token": "abc123...",
  "pond_version": "0.1.0",
  "hostname": "my-pond",
  "capabilities": ["chat", "voice", "schedules"]
}
```

| Code | Meaning |
|------|---------|
| 400 | Invalid request body |
| 403 | `legacy_pairing_host_only` or `pairing_requires_lan`: the caller is not on this host |
| 500 | Handshake handler error |

---

### Phone pairing: POST /handshake/pake/init and /handshake/pake/verify (2026-10-13)

Phones pair with CPace (`draft-irtf-cfrg-cpace-21`, suite `CPACE-RISTR255-SHA512`). The Pond
is initiator A and the phone responder B. Both routes take direct LAN peers and loopback only
(a tailnet or unclassifiable peer gets `403 pairing_requires_lan`), sit in the pairing rate
bucket, and are served on the HTTPS listener, the loopback listener and, in debug builds with
`POND_DEV_INSECURE_LAN=1`, the plaintext development listener. The cryptography, labels and
encodings are in `crates/pond-infra/src/pairing_cpace.rs`; one complete run with every
intermediate value is `crates/pond-infra/tests/fixtures/cpace-vectors.json`.

| Value | Definition |
|-------|------------|
| PRS | the six-digit code as six ASCII bytes, leading zeros kept |
| CI | `lv_cat("goose-pair-cpace-v1", spki_pin, client_id)`; `spki_pin` is the ASCII `sha256/<base64>` pin. The Pond always uses its own pin |
| sid | `challenge (32 bytes) ‖ client_nonce (32 bytes)` |
| ADa, ADb | empty |
| g | RFC 9496 element derivation of `SHA-512(generator_string(DSI, PRS, CI, sid, 128))` |
| ISK | `SHA-512(lv_cat("CPaceRistretto255_ISK", sid, K) ‖ lv_cat(Ya, ADa) ‖ lv_cat(Yb, ADb))` |
| mac_key | `SHA-512("CPaceMac" ‖ sid ‖ ISK)` |
| T_client | `HMAC-SHA-512(mac_key, lv_cat("goose-pair-cpace-client-v1", Ya, ADa, Yb, ADb))` |
| T_server | `HMAC-SHA-512(mac_key, lv_cat("goose-pair-cpace-server-v1", Ya, ADa, Yb, ADb))` |

`lv_cat` prefixes each field with its LEB128 length. Shares are 32-byte ristretto255
encodings in standard base64; tags are 64 bytes in lowercase hex.

**`POST /handshake/pake/init`**
```json
{
  "client_id": "install-abcdef",
  "client_type": "gotg",
  "client_version": "1.0.0",
  "channel_binding": "sha256/TnkUsN+AaLec3BDTh/KUwSokXTmCq2+4rlfBnmJxPkE=",
  "client_nonce": "<32 random bytes, base64>"
}
```

`channel_binding` is the pin the phone pinned, and on the plaintext development listener the
pin that listener's `/system/info` reports. It is checked only so a phone at the wrong Pond fails
early: the run itself always uses the Pond's own pin.

**Response 200**: the Pond has committed to the newest active pairing code.
```json
{
  "challenge_id": "…",
  "challenge": "<32 bytes, base64>",
  "expires_at": "2026-10-13T09:01:00Z",
  "suite": "CPACE-RISTR255-SHA512",
  "share": "<Ya, base64>"
}
```

**`POST /handshake/pake/verify`**
```json
{
  "challenge_id": "…",
  "device_name": "Liz's phone",
  "share": "<Yb, base64>",
  "confirm": "<T_client, hex>"
}
```

**Response 200**, accepted: the token fields of `HandshakeResponse` plus `"server_proof"`, which
is `T_server` in hex. The phone must check it and discard the tokens if it does not verify.
**Response 200**, refused: `"accepted": false`, with the same code in both `rejection_reason`
and `error`, and no `server_proof`.

Every refusal from either route carries a stable code in `error`, which a client should read
whatever the status:

| Route | Status | `error` | Meaning |
|-------|--------|---------|---------|
| init | 403 | `pairing_requires_lan` | Not a direct LAN or loopback peer |
| init | 403 | `channel_binding_mismatch` | The phone pinned a key that is not this Pond's |
| init | 409 | `no_active_pairing_code` | No live code; show a new one |
| init | 400 | `invalid_client_nonce` | `client_nonce` is not 32 bytes of base64 |
| init | 503 | `pairing_busy` | Too many runs are waiting for their verify; retry shortly |
| verify | 429 | `pairing_busy` | The per-IP verify limit (10 a minute); `retry_after_secs` says when |
| verify | 200 | `invalid_confirmation` | `T_client` is wrong, which means a wrong code (or a wrong pin) |
| verify | 200 | `invalid_share` | `Yb` did not decode, or the shared point was the identity |
| verify | 200 | `code_retired` | The code this run used was consumed, replaced, expired, or spent its 10 failed attempts; show a new code |
| verify | 200 | `unknown_challenge` | No such run, or it was already used (each run gets one attempt) |
| verify | 200 | `challenge_expired` | The run is older than 60 s |

The pairing rate bucket (30 a minute per IP, loopback exempt) answers `429` with
`{"error": "Rate limit exceeded"}` and a `Retry-After` header before either handler runs, so a
client should treat any 429 as `pairing_busy`.

Each code allows 10 confirmation attempts in all; the one that spends the budget answers
`code_retired`, and the code stops working. Issuing a new code retires every older one.

### Dashboard pairing: POST /handshake/init and /handshake/verify

The desktop dashboard pairs over loopback with the code it reads from the host-only
`/handshake/pairing-code`: `init` returns `{challenge_id, challenge, expires_at}` and `verify`
takes `{challenge_id, mac, device_name?}` with `mac = HMAC-SHA256(code, challenge ‖ client_id)`
in hex. Any peer but loopback on the dashboard or HTTPS listener, and every peer on the plaintext
development listener, gets `403 {"error": "pairing_requires_cpace"}` (a tailnet peer still gets
`pairing_requires_lan` first). A refused verify is a 200 with `accepted: false` and a
`rejection_reason`.

---

### POST /onboard

Starts the onboarding flow if not already in progress. Idempotent — safe to call multiple times.

**Response 200 — started fresh**
```json
{ "status": "started", "message": "Onboarding started" }
```

**Response 200 — already in progress**
```json
{ "status": "in_progress", "current_step": "Basics", "message": "Onboarding already started" }
```

**Response 200 — already done**
```json
{ "status": "already_complete" }
```

---

### POST /onboard/complete

Marks onboarding as complete. After this call, all protected routes become accessible.

**Response 200**
```json
{ "status": "completed" }
```

---

### POST /onboard/reset

Returns onboarding to `Welcome` and reopens the wizard's routes. Open to anyone while the
wizard runs. Once the Pond is set up it needs a valid bearer token, or a loopback peer
presenting `X-Pond-Host-Credential` (2026-10-12): a reset reopens `PUT /settings`,
`POST /profiles` and `/onboard/*` to every LAN and tailnet peer. A LAN caller without a
token gets `401`; a loopback caller with neither gets `403 host_credential_required`, or
`503 host_credential_unavailable` on the companion listener.

**Response 200** — the status body below, at `Welcome`.

---

### GET /onboard/status

Returns detailed onboarding progress.

**Response 200**
```json
{
  "onboarded": false,
  "current_step": "Personality",
  "steps_completed": 4,
  "total_steps": 9
}
```

Steps: `Welcome` → `Basics` → `Location` → `Accessibility` → `Personality` → `GooseIdentity` → `WakeWord` → `Model` → `Extensions` → `Completed`

---

## Settings

### GET /settings

Returns the full settings object.

**Response 200**
```json
{
  "assistant_name": "Goose",
  "assistant_personality": "friendly and concise",
  "user_name": "Friend",
  "timezone": "UTC",
  "llm_max_tokens": 1024,
  "llm_temperature": 0.7,
  "llm_provider": "llamafile",
  "chat_provider": "local",
  "chat_model": "gemma-4-E4B-it-Q4_K_M",
  "tool_model": null,
  "thinking_mode": "auto",
  "show_thinking": false,
  "review_mode": "off",
  "review_max_rounds": 1,
  "review_pass_threshold": 3,
  "context_window_override": 0,
  "prompt_style": "balanced",
  "custom_system_prompt": null,
  "prompt_addendum": "",
  "agent_goose_mode": "auto",
  "agent_max_turns": 50,
  "agent_memory_inject": false,
  "agent_memory_limit": 5,
  "voice_wake_word": "goose",
  "voice_tts_voice": "en_US-lessac-medium.onnx",
  "voice_recording_duration_secs": 5,
  "voice_whisper_url": "http://127.0.0.1:9000",
  "weather_enabled": false,
  "weather_latitude": 0.0,
  "weather_longitude": 0.0,
  "weather_location_name": "",
  "retention_event_log_days": 30,
  "retention_sensor_days": 7,
  "retention_session_messages_keep": 500
}
```

---

### PUT /settings

Partial update — only include fields you want to change. When `chat_provider`, `chat_model`, `think_*`, or `task_*` fields change, the ModelRouter is hot-reloaded automatically.

**Request** *(any subset of Settings fields)*
```json
{
  "assistant_name": "Jarvis",
  "prompt_style": "concise",
  "agent_memory_inject": true,
  "chat_provider": "ollama",
  "chat_model": "llama3.2"
}
```

**Response 200**
```json
{ "status": "ok" }
```

| Code | Meaning |
|------|---------|
| 400 | Invalid JSON |
| 500 | Failed to read or persist settings |

---

## Chat & Voice

### POST /chat

Single-turn conversation with the Goose agent. Persists both the user message and the assistant response to the session. Classifies the request to select the appropriate model role (chat/think/task).

**Request**
```json
{
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "message": "What's the weather today?"
}
```

`session_id` is optional — omitting it creates a new session automatically.

**Response 200**
```json
{
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "response": "It's currently 22°C and sunny in Nairobi.",
  "model_role": "chat"
}
```

`model_role` values: `"chat"` | `"think"` | `"task"`

| Code | Meaning |
|------|---------|
| 400 | Invalid or missing request body |
| 403 | Onboarding not complete |
| 500 | Session creation or agent error |

---

### POST /chat/stream

Streaming variant of `/chat` using Server-Sent Events. The response body is a stream of SSE events. Connect with `EventSource` or `fetch` with `ReadableStream`.

**Request** — same as `/chat`

**Request**
```json
{
  "session_id": "550e8400-...",
  "message": "What is MKBHD?",
  "images": []
}
```

`images` is optional -- array of `{data: "base64...", mime_type: "image/jpeg"}` for multimodal models.

**SSE Event stream**

| Event Type | Example | Description |
|------------|---------|-------------|
| `status` | `{"type":"status","content":"Thinking..."}` | Pipeline progress |
| `thinking` | `{"type":"thinking","content":"...reasoning..."}` | Chain-of-thought (when `show_thinking` enabled) |
| `tool_call` | `{"type":"tool_call","tool":"wikipedia","id":"..."}` | Tool invocation |
| `tool_result` | `{"type":"tool_result","tool":"wikipedia","content":"..."}` | Tool result data |
| `text` | `{"type":"text","content":"MKBHD is..."}` | Streamed response tokens |
| `review_status` | `{"type":"review_status","content":"Reviewing..."}` | Answer review progress (when review enabled) |
| `review_revision` | `{"type":"review_revision","content":"...","score":4}` | Revised answer (replaces streamed text) |
| `done` | `{"done":true,"session_id":"...","model_role":"chat"}` | Stream complete |
| `error` | `{"error":"Agent failed: ..."}` | Error |

| Code | Meaning |
|------|---------|
| 400 | Invalid request body |
| 403 | Onboarding not complete |
| 500 | Session/history loading failed |

---

### POST /tts

Synthesises speech on the server and returns a WAV audio file. Priority: Piper HTTP → 503.
A valid bearer token admits the caller. A loopback caller without one needs
`X-Pond-Host-Credential` (2026-10-12), so the dashboard sends it with every speech
request; without it the answer is `403 host_credential_required`, or `503
host_credential_unavailable` on the companion listener.

**Request**
```json
{ "text": "Good morning, Jerry." }
```

**Response 200**

Headers:
```
Content-Type: audio/wav
Content-Length: <bytes>
```

Body: raw WAV audio bytes

| Code | Meaning |
|------|---------|
| 400 | Missing or empty `text` field |
| 403 | Onboarding not complete |
| 503 | No TTS backend is running |

---

### POST /transcribe

Proxies audio to the local whisper.cpp server and returns the transcription. Primarily a testing tool — voice input in the chat loop uses a different internal path.

**Request** — multipart/form-data with field `audio` (WAV binary)

**Response 200**
```json
{ "text": "What's the weather today?" }
```

| Code | Meaning |
|------|---------|
| 400 | Missing or invalid audio field |
| 502 | Whisper server unreachable or returned error |

---

## Sessions

### GET /sessions

Lists all conversation sessions, newest first.

**Response 200**
```json
{
  "sessions": [
    {
      "id": "550e8400-...",
      "title": "Weather check",
      "created_at": "2026-04-09 08:00:00",
      "updated_at": "2026-04-09 08:05:32"
    }
  ]
}
```

---

### PATCH /sessions/{session_id}

Renames a session.

**Request**
```json
{ "title": "Morning briefing" }
```

**Response 200**
```json
{ "session_id": "550e8400-...", "title": "Morning briefing" }
```

| Code | Meaning |
|------|---------|
| 400 | Missing title |
| 404 | Session not found |
| 500 | Update failed |

---

### GET /sessions/{session_id}/messages

Returns all messages in a session, oldest first.

**Response 200**
```json
{
  "messages": [
    {
      "id": "msg-uuid",
      "session_id": "550e8400-...",
      "role": "user",
      "content": "What's the weather?",
      "created_at": "2026-04-09 08:00:01"
    },
    {
      "id": "msg-uuid-2",
      "role": "assistant",
      "content": "It's 22°C and sunny.",
      "created_at": "2026-04-09 08:00:03"
    }
  ]
}
```

`role` values: `"user"` | `"assistant"` | `"system"` | `"tool"`

Two optional fields, each ABSENT rather than empty when it does not apply:

- `images` — on a message that had images attached:
  `[{ "id": "...", "mime_type": "image/png", "byte_size": 70, "url": "/api/v1/sessions/<sid>/attachments/<aid>" }]`.
  `url` is relative to the API base and is protected; see the next route before using it.
- `thinking` — on an assistant message recorded while `persist_thinking` was on: the reasoning
  passages that produced it, in order, as `string[]`. Absent means nothing was kept; `[]` means it
  was kept and there was none.

---

### GET /sessions/{session_id}/attachments/{attachment_id}

The raw bytes of one persisted chat image, with its `Content-Type` and
`Cache-Control: private, max-age=31536000, immutable`. 404 when the attachment does not belong to
that session, or its bytes are gone.

**Send the bearer token.** This is on the protected router and the middleware reads only
`Authorization: Bearer`, so a loader that cannot set a header — a bare `<img src>` — gets a 401 on
every pond not started with `POND_DEV_ALLOW_LOOPBACK`. The desktop fetches the bytes with its token
and shows them through an object URL (`PondApiClient.getSessionAttachment`); React Native's
`Image` takes `source={{ uri, headers: { Authorization: "Bearer <token>" } }}`.

---

## Models

### GET /models

Lists all models in the catalog, grouped by category. The `downloaded` flag reflects actual disk presence. The `active` flag is true when the model is assigned to any role.

**Response 200**
```json
{
  "gguf": [
    {
      "category": "gguf",
      "name": "llama-3.2-3b",
      "description": "Llama 3.2 3B Q4_K_M",
      "size_mb": 2100,
      "downloaded": true,
      "active": true,
      "url": "https://huggingface.co/...",
      "hf_id": "lmstudio-community/Llama-3.2-3B-Instruct-GGUF:Q4_K_M",
      "filename": "llama-3.2-3b-q4.gguf",
      "ram_estimate_mb": 2400,
      "recommended_role": "chat"
    }
  ],
  "whisper": [...],
  "llamafile": [...],
  "tts": [...]
}
```

---

### GET /models/active-roles

Returns the current role assignments for all five roles.

**Response 200**
```json
{
  "chat":  { "provider": "llamafile", "model": "llama-3.2-3b", "model_id": "gguf/llama-3.2-3b" },
  "think": { "provider": "ollama",    "model": "mistral",        "model_id": "ollama/mistral" },
  "task":  { "provider": "llamafile", "model": "llama-3.2-3b", "model_id": "gguf/llama-3.2-3b" },
  "asr":   { "model_id": "whisper/base" },
  "tts":   { "model_id": "tts_piper/en_US-lessac-medium" },
  "router_name": "ModelRouter(llamafile→ollama→llamafile)"
}
```

---

### GET /models/capabilities

Returns the active model's runtime capabilities. Used by the frontend to conditionally enable features (image upload, thinking display, etc.).

**Response 200**
```json
{
  "thinking": true,
  "vision": true,
  "audio_input": false,
  "context_window_tokens": 128000,
  "structured_output": true
}
```

See [Model Capabilities](./architecture/model_capabilities.md) for details on how capabilities are detected.

---

### GET /models/memory-status

Returns available RAM for LLM use. Only populated when the `local-inference` feature is active.

**Response 200**
```json
{
  "total_mb": 8192,
  "available_for_llm_mb": 5120,
  "loaded_model": "llama-3.2-3b"
}
```

---

### POST /models/registry/refresh

Fetches the latest model catalog from the configured online registry URL in the background. Updates `downloaded` flags by checking the filesystem. Returns immediately; the refresh runs asynchronously.

**Response 200**
```json
{ "status": "refresh_started" }
```

`status` values: `"refresh_started"` | `"no_registry"` | `"no_catalog_provider"`

---

### POST /models/scan

Explicitly scans the model directories for files not yet in the catalog and adds them as custom entries.

**Response 200**
```json
{ "found": 2, "entries": [{ "category": "gguf", "name": "my-custom-model", "downloaded": true }] }
```

---

### GET /models/download/progress

Returns state of all active and recently completed downloads.

**Response 200**
```json
{
  "downloads": [
    {
      "filename": "llama-3.2-3b-q4.gguf",
      "category": "gguf",
      "downloaded_bytes": 524288000,
      "total_bytes": 2202009600,
      "status": "downloading"
    }
  ]
}
```

`status` values: `"downloading"` | `"done"` | `"error"`

---

### POST /models/{category}/{name}/download

Triggers an async download of a catalogued model. Returns 202 immediately; poll `/models/download/progress` for status.

**Path params**
- `category`: `gguf` | `llamafile` | `whisper` | `tts_piper`
- `name`: model name as in catalog

**Response 202**
```json
{ "status": "download_started", "name": "llama-3.2-3b", "category": "gguf" }
```

**Response 200** *(already on disk)*
```json
{ "status": "already_downloaded" }
```

| Code | Meaning |
|------|---------|
| 400 | Invalid category or model has no URL/filename |
| 404 | Model not in catalog |
| 503 | Model repo or data directory not configured |

---

### POST /models/download/url

Downloads a model from an arbitrary HTTPS URL. Useful for custom GGUF files not in the catalog.

**Request**
```json
{
  "url": "https://huggingface.co/TheBloke/Mistral-7B-GGUF/resolve/main/mistral-7b.Q4_K_M.gguf",
  "category": "gguf",
  "filename": "mistral-7b-q4.gguf"
}
```

**Response 202**
```json
{ "status": "downloading", "filename": "mistral-7b-q4.gguf", "category": "gguf" }
```

| Code | Meaning |
|------|---------|
| 400 | Missing url/filename, or URL is not HTTPS |
| 503 | Data directory not configured |

---

### POST /models/{category}/{name}/activate

Assigns a model to a role. Validates that the category is compatible with the role (e.g., only `whisper` models can be assigned to `asr`; only `gguf`/`llamafile`/`ollama` models can be assigned to `chat`/`think`/`task`). For LLM roles, hot-reloads the ModelRouter.

**Request**
```json
{ "role": "chat" }
```

`role` values: `"chat"` | `"think"` | `"task"` | `"asr"` | `"tts"`

**Response 200**
```json
{ "role": "chat", "model_id": "gguf/llama-3.2-3b" }
```

| Code | Meaning |
|------|---------|
| 400 | Invalid category or role/category mismatch |
| 404 | Model not in catalog |
| 503 | Model repo not configured |

---

### DELETE /models/{category}/{name}

Deletes the model file from disk. The catalog record is kept with `downloaded = false`. Fails if the model is currently assigned to any role.

**Response 204** — no body

| Code | Meaning |
|------|---------|
| 404 | Model not in catalog |
| 409 | Model is assigned to an active role — deactivate first |
| 500 | File deletion failed |

---

### GET /models/ollama

Proxies `GET http://localhost:11434/api/tags`. Returns Ollama's native response shape.

---

### POST /models/ollama/pull

Spawns `ollama pull <model>` as a background process. Returns 202 immediately.

**Request**
```json
{ "model": "mistral" }
```

**Response 202**
```json
{ "status": "pulling", "model": "mistral" }
```

| Code | Meaning |
|------|---------|
| 400 | Missing or empty model name |
| 503 | ollama not found or not running |

---

### GET /models/search/gguf?q={query}

Searches HuggingFace for GGUF-tagged models matching the query.

**Response 200**
```json
{
  "models": [
    {
      "id": "TheBloke/Mistral-7B-GGUF",
      "downloads": 1200000,
      "likes": 3400,
      "tags": ["gguf", "mistral"],
      "url": "https://huggingface.co/TheBloke/Mistral-7B-GGUF"
    }
  ]
}
```

---

### GET /models/search/gguf/files?repo={owner/name}

Lists `.gguf` files available in a specific HuggingFace repository.

**Response 200**
```json
{
  "files": [
    {
      "filename": "mistral-7b.Q4_K_M.gguf",
      "size_mb": 4368,
      "url": "https://huggingface.co/TheBloke/Mistral-7B-GGUF/resolve/main/mistral-7b.Q4_K_M.gguf"
    }
  ]
}
```

---

### GET /models/search/llamafile?q={query}

Searches GitHub for llamafile releases from Mozilla-Ocho/llamafile matching the query.

**Response 200**
```json
{
  "models": [
    {
      "name": "mistral-7b-instruct-v0.2",
      "version": "v0.2",
      "size_mb": 4368,
      "url": "https://github.com/Mozilla-Ocho/llamafile/releases/download/...",
      "release_url": "https://github.com/Mozilla-Ocho/llamafile/releases/tag/..."
    }
  ]
}
```

---

## Profiles

Profiles represent household members. Sessions and memories are scoped to a profile.

### POST /profiles *(Public)*

**Request**
```json
{
  "display_name": "Jerry",
  "avatar_emoji": "🦆",
  "preferences": { "language": "en", "timezone": "Africa/Nairobi" }
}
```

**Response 201**
```json
{
  "id": "uuid",
  "display_name": "Jerry",
  "avatar_emoji": "🦆",
  "preferences": { "language": "en", "timezone": "Africa/Nairobi" },
  "created_at": "2026-04-09 08:00:00",
  "updated_at": "2026-04-09 08:00:00"
}
```

---

### GET /profiles

**Response 200** — array of profile objects (same shape as above)

---

### GET /profiles/{id}

**Response 200** — single profile object

| Code | Meaning |
|------|---------|
| 404 | Profile not found |

---

### PATCH /profiles/{id} *(Public)*

Merges preference keys into the profile. Existing keys not in the request are preserved.

**Request**
```json
{ "preferences": { "timezone": "America/New_York" } }
```

**Response 200** — updated profile object

| Code | Meaning |
|------|---------|
| 404 | Profile not found |

---

### DELETE /profiles/{id}

Removes a household member and reports what went with them.

**Response 200**
```json
{
  "profile_id": "8133c258-3392-4f0e-ba36-3d28a51f23a4",
  "display_name": "Liz",
  "deleted":  { "memories": 42, "face_embeddings": 3, "uber_accounts": 1 },
  "released": { "sessions": 7 },
  "cleared_primary_profile": false
}
```

`deleted` and `released` are separate on purpose. Memories and face embeddings
are removed (`ON DELETE CASCADE`), and so is the member's Uber sign-in
(`uber_accounts`: 1 when they had one), forgotten before the member is deleted so
that a failure deletes nothing. Sessions are **released** — the conversation
survives, stripped of its attribution, because a conversation is not solely the
speaker's. Household-scoped memories (`profile_id IS NULL`) are shared context
and are never counted or removed.

`cleared_primary_profile` is `true` when this member was `settings.primary_profile_id`,
which is cleared before the delete so it cannot dangle.

**Response 404** — no such profile.

---

## Session identity

Who a chat session belongs to, and on what evidence. `identification_source` is
one of `paired_device`, `explicit`, `face`, `unknown`, in descending order of
strength — a weaker source may never take over a session a stronger one bound.

### GET /sessions/{id}/user

**Response 200** — an unidentified or unknown session reports nobody rather than
erroring; "whose session is this" has a correct answer for a session that does
not exist.
```json
{
  "session_id": "sess-1",
  "profile_id": null,
  "identification_source": "unknown",
  "confidence": null
}
```
`confidence` is set only for `face`.

### PUT /sessions/{id}/user

Explicit identification — the member picked themselves, or said who they are.

**Request** `{ "profile_id": "..." }`

**Response 200** — `bound` is `false`, with the existing binding returned
unchanged, when a stronger source already holds the session.
```json
{ "session_id": "sess-1", "profile_id": "...", "identification_source": "explicit", "bound": true }
```

**Response 400** — empty `profile_id`. **404** — no such session.

### DELETE /sessions/{id}/user

Releases the binding. `cleared` reports whether there was one.

**Response 200** `{ "session_id": "sess-1", "cleared": true }` · **404** — no such session.

### POST /sessions/{id}/identify-user

Wake-on-face. Same multipart payload as `/faces/identify`, plus optional `bbox`.

**Response 200** — `bound` is `false` when the match would downgrade a stronger
binding, or when `identified` is `false`.
```json
{ "session_id": "sess-1", "identified": true, "profile_id": "...",
  "confidence": 0.62, "threshold": 0.5, "bound": true }
```

---

## Devices

Devices are GOTG mobile clients, IoT sensors, cameras, or other Pond instances.

### GET /devices

**Response 200**
```json
{
  "devices": [
    {
      "id": "uuid",
      "name": "Jerry's Phone",
      "device_type": "gotg",
      "hostname": "jerry-iphone.local",
      "ip_address": "192.168.1.42",
      "capabilities": ["chat", "push"],
      "registered_at": "2026-04-01 10:00:00",
      "last_seen": "2026-04-09 07:58:12",
      "is_online": true
    }
  ]
}
```

`is_online` is computed live from `last_seen > now − 5min`, not stored.

---

### GET /devices/self

The calling device, and the household member it is attributed to, if anyone. **For display only:** a greeting, the name on a profile screen. It proves nothing and grants nothing. `Principal::profile_id` is not populated from it, and it never feeds `ProfileScope`.

The device comes from `proven_device()`, the token this Pond issued at pairing, never from anything the client sends about itself. The attribution comes from the pairing code (migration 0043). The route is scoped to the caller on purpose. Adding `profile_id` to every row of `GET /devices` would hand every paired client the whole device-to-member map.

**Response 200**, attributed:
```json
{ "device_id": "liz-phone", "profile": { "id": "p-1", "display_name": "Liz" } }
```

**Response 200**, unattributed. Nobody has claimed this device, which is the normal case, because pairing happens before anyone says who they are:
```json
{ "device_id": "kitchen-tablet", "profile": null }
```

| Status | Meaning |
|---|---|
| 200 | `profile` is the member, or `null`. Only `id` and `display_name` are returned, not preferences and not the avatar. |
| 401 | No token. The route is not on `PUBLIC_ROUTES`. |
| 404 | The request did not come from a paired device, for example a loopback bypass. |
| 503 | The attribution read failed. It is logged, and it is deliberately **not** reported as `profile: null`, which would look exactly like an unclaimed device. |

A Pond from before this route answers a GET here with **405**, because the request falls through to `/devices/{id}`, which only accepts DELETE and PUT. Clients should treat 404 and 405 as "this Pond cannot say".

### POST /devices

**Request**
```json
{
  "name": "Jerry's Phone",
  "device_type": "gotg",
  "hostname": "jerry-iphone.local",
  "ip_address": "192.168.1.42",
  "capabilities": ["chat", "push"]
}
```

**Response 201** — device object

---

### PUT /devices/{id}

The Devices panel's "Configure" save, and a phone renaming itself after setup.

**Body** — every field optional: `{"name"?: string, "hostname"?: string | null, "room"?: string | null}`.
A field left out keeps its stored value (2026-10-17). An explicit `null`, or a blank string, clears a
hostname or room. A blank name is refused.

**Who may change which row** (2026-10-17, `pond_core::user_data::services::device_edit`):

| Row | May change it |
|---|---|
| A paired client's own (`device_type` `gotg`, `desktop` or `cli`) | that device (its own token) or the host (the loopback listener with `X-Pond-Host-Credential`) |
| A shared row (a Matter device, a sensor, another pond) | any authenticated caller, because the household's devices are managed from any of its phones |

Until then any paired token could rename any row, the owner's phone included. The desktop's
Devices panel sends the host credential, since it is the one place another phone's name is edited.

**Response 200** — the updated device.

**Response 400** `{"error":"name_required"}` — a blank name.

**Response 403** `{"error":"device_not_yours"}` — another paired client's row, without the host
credential. Logged as `kind="device_edit_denied"` with the row, its type and the caller.

**Response 404** `{"error":"device_not_found"}`.

---

### DELETE /devices/{id}

Before revoking the device's sessions, the Pond queues its tailnet revocation through the durable remote-revocation queue (`RemoteRevocation::queue`); Matter devices are skipped.

**Response 204** — no body

**Response 503** `{"error":"revocation_unavailable"}` — the queue could not be written. The device is left listed with its sessions intact, so the removal can be retried. See `docs/auth-network-posture.md`.

---

### POST /devices/{id}/heartbeat

Updates `last_seen` and marks device online.

`is_online` is not stored as a fact the registry is told. It is derived when read, from `last_seen` against `ONLINE_THRESHOLD_SECS` (300s, `crates/pond-infra/src/sqlite_device_registry.rs`). **A device that registered itself through the handshake has to keep calling this, or it ages out and reads offline while in use.** The handshake's upsert is the only other write to its row.

| Caller | Cadence | Where |
|---|---|---|
| The desktop app, for its own row | every 120s while it has a session (`SELF_HEARTBEAT_MS`) | `pond-desktop/src/state/AppContext.tsx`, via `heartbeatSelf()` |
| Goose On The Go, for its own row | every 120s while in the foreground | the phone app's `ServerProvider` |
| The Matter bridge | as it syncs each device | `crates/pond-adapters-matter/src/bridge.rs` |

`{id}` must be the id the device registered under. For a client that paired, that is the `client_id` it sent at `/handshake/init`. A beat against any other id succeeds and refreshes nothing. The desktop therefore beats through its own `clientId()` rather than taking an id as an argument.

**Response 200**
```json
{ "status": "ok" }
```

---

## Sensors

Sensor data lives in `pond_logs.db` with a 7-day TTL.

### POST /sensors

**Request**
```json
{
  "device_id": "sensor-uuid",
  "sensor_type": "temperature",
  "value": 22.5,
  "unit": "C"
}
```

`sensor_type` values: `temperature` | `humidity` | `motion` | `door` | `energy` | `light_level`

**Response 201** — no body

---

### GET /sensors/{device_id}?limit={n}

`limit` defaults to 20, max 100.

**Response 200**
```json
{
  "readings": [
    {
      "device_id": "sensor-uuid",
      "sensor_type": "temperature",
      "value": 22.5,
      "unit": "C",
      "recorded_at": "2026-04-09 08:00:00"
    }
  ]
}
```

---

## Camera Events

Camera events live in `pond_logs.db`. Unacknowledged alerts are kept indefinitely; acknowledged ones prune at 14 days.

### POST /camera/events

**Request**
```json
{
  "camera_id": "front-door-cam",
  "event_type": "person",
  "confidence": 0.94,
  "snapshot_path": "/data/snapshots/2026-04-09-08-00-01.jpg",
  "metadata": "{\"zone\": \"driveway\"}"
}
```

`event_type` values: `motion` | `person` | `pet` | `package` | `vehicle`

**Response 201**
```json
{ "id": 42 }
```

---

### GET /camera/events?camera_id={id}&limit={n}

`camera_id` defaults to `"default"`. `limit` defaults to 20, max 100.

**Response 200**
```json
{
  "events": [
    {
      "id": 42,
      "camera_id": "front-door-cam",
      "event_type": "person",
      "confidence": 0.94,
      "snapshot_path": "/data/snapshots/...",
      "acknowledged": false,
      "created_at": "2026-04-09 08:00:01"
    }
  ]
}
```

---

### PATCH /camera/events/{id}/acknowledge

Dismisses an alert. Does not delete it — pruning happens automatically at 14 days.

**Response 200**
```json
{ "status": "ok" }
```

---

## Schedules

The scheduler uses **6-field cron** with a leading seconds field:
`<sec> <min> <hour> <day-of-month> <month> <day-of-week>`

All schedule endpoints return `503` if the scheduler failed to initialise at startup.

### GET /schedules

**Response 200** — array of task objects:
```json
[
  {
    "id": "morning-brief",
    "label": "Morning Briefing",
    "cron": "0 0 8 * * *",
    "last_run": "2026-04-09 08:00:00",
    "next_run": "2026-04-10 08:00:00",
    "paused": false,
    "currently_running": false
  }
]
```

---

### POST /schedules

**Request**
```json
{
  "id": "morning-brief",
  "label": "Morning Briefing",
  "cron": "0 0 8 * * *",
  "payload": {
    "webhook_url": "http://127.0.0.1:4000/api/v1/chat",
    "message": "Give me a morning briefing."
  }
}
```

The `WebhookTaskExecutor` POSTs `payload` to `payload.webhook_url` on each fire.

**Response 201** — task object

| Code | Meaning |
|------|---------|
| 400 | Invalid cron expression or missing fields |
| 503 | Scheduler not configured |

---

### DELETE /schedules/{id}

**Response 200**
```json
{ "deleted": "morning-brief" }
```

---

### POST /schedules/{id}/pause

**Response 200**
```json
{ "paused": "morning-brief" }
```

---

### POST /schedules/{id}/resume

**Response 200**
```json
{ "resumed": "morning-brief" }
```

---

### POST /schedules/{id}/run-now

Fires the task immediately regardless of its cron schedule.

**Response 202**
```json
{ "fired": "morning-brief" }
```

---

## MCP Extensions

Extensions expose additional tools to the Goose agentic loop. Saved configs auto-reconnect on restart.

### GET /extensions

**Response 200**
```json
{
  "extensions": [
    {
      "name": "giap",
      "kind": "builtin",
      "tools": [
        "giap__get_current_weather",
        "giap__list_registered_devices",
        "giap__get_user_profile",
        "giap__get_model_assignments",
        "giap__recall_memories",
        "giap__save_memory",
        "giap__list_schedules",
        "giap__get_recipe",
        "giap__list_skills"
      ]
    }
  ]
}
```

---

### POST /extensions

**Request**
```json
{
  "name": "filesystem",
  "kind": "stdio",
  "command": "npx @modelcontextprotocol/server-filesystem /home/jerry/documents",
  "description": "Access local documents"
}
```

`kind` values: `"builtin"` | `"stdio"` | `"streamable_http"`

For `"streamable_http"`: provide `uri` instead of `command`.

**Response 201** — extension info object

| Code | Meaning |
|------|---------|
| 400 | Invalid request |
| 503 | Extension manager not available |

---

### DELETE /extensions/{name}

Removes the extension and deletes its persisted config so it won't reconnect on restart.

**Response 204** — no body

| Code | Meaning |
|------|---------|
| 404 | Extension not found |
| 503 | Extension manager not available |

---

### PATCH /extensions/{name}

Enables or disables an extension without removing its persisted config. Disabled extensions are excluded from future Goose agent sessions but keep their registration so they can be re-enabled later.

**Request**
```json
{ "enabled": false }
```

**Response 200**
```json
{ "name": "my-ext", "enabled": false }
```

| Code | Meaning |
|------|---------|
| 404 | Extension not found |
| 503 | Extension manager not available |

---

## Marketplace

The marketplace provides a curated registry of popular MCP extensions for one-click installation.

### GET /marketplace

Lists all available extensions in the curated registry.

**Response 200**
```json
{
  "extensions": [
    {
      "id": "filesystem",
      "name": "Filesystem",
      "description": "Read, write, and search files on the local filesystem.",
      "kind": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/"],
      "category": "productivity",
      "author": "Anthropic",
      "tools": ["read_file", "write_file", "list_directory"],
      "featured": true
    }
  ]
}
```

| Code | Meaning |
|------|---------|
| 503 | Marketplace not configured |

---

### POST /marketplace/{id}/install

Installs a marketplace extension by its registry ID. Looks up the extension in the catalogue, converts it to an `AddExtensionRequest`, registers it with the extension manager, and persists the config so it reconnects on restart.

**Response 201** — extension info object (same shape as `POST /extensions` response)

| Code | Meaning |
|------|---------|
| 404 | Extension ID not found in marketplace |
| 503 | Marketplace or extension manager not available |

---

## Prompt Templates

Named system prompt templates stored in the database. The active template is selected by `settings.prompt_style`. Four built-in templates (`balanced`, `concise`, `technical`, `warm`) are seeded at setup and cannot be deleted, but their content is always editable.

Templates support `{{placeholder}}` variables: `{{assistant_name}}`, `{{user_name}}`, `{{personality}}`, `{{timezone}}`, `{{location}}`, `{{prompt_addendum}}`.

### GET /prompts

**Response 200** — array of template objects:
```json
[
  {
    "name": "balanced",
    "content": "You are {{assistant_name}}...",
    "description": "Warm, practical, complete behaviour rules.",
    "is_system": true,
    "updated_at": "2026-04-09 10:00:00"
  }
]
```

---

### GET /prompts/{name}

**Response 200** — single template object

| Code | Meaning |
|------|---------|
| 404 | Template not found |

---

### PUT /prompts/{name}

Creates a new user-defined template or updates any existing template (including system ones — content is editable even for built-ins).

**Request**
```json
{
  "content": "You are {{assistant_name}}. Reply only in Swahili.",
  "description": "Swahili-only mode"
}
```

**Response 200**
```json
{ "name": "swahili", "status": "ok" }
```

Activate by: `PUT /settings { "prompt_style": "swahili" }`

---

### DELETE /prompts/{name}

Deletes a user-defined template. Built-in templates (`is_system = true`) cannot be deleted.

**Response 204** — no body

| Code | Meaning |
|------|---------|
| 403 | Cannot delete a built-in template |
| 404 | Template not found |

---

## System Prompt Extras

Per-key extra instructions injected via `agent.extend_system_prompt(key, instruction)` on every chat turn. Active extras are injected in `sort_order` ascending order.

### GET /agent/extras

**Response 200** — array of extra objects:
```json
[
  {
    "key": "safety",
    "instruction": "Never suggest actions that could cause physical harm.",
    "active": true,
    "sort_order": 0,
    "updated_at": "2026-04-09 10:00:00"
  }
]
```

---

### POST /agent/extras

Creates or updates an extra. Identified by `key` — upsert semantics.

**Request**
```json
{
  "key": "language",
  "instruction": "Always respond in French.",
  "active": true,
  "sort_order": 10
}
```

**Response 200**
```json
{ "key": "language", "status": "ok" }
```

---

### DELETE /agent/extras/{key}

**Response 204** — no body

---

### GET /agent/tools

Lists all MCP tools currently available to the Goose agent across all loaded extensions.

**Response 200**
```json
{
  "tools": [
    {
      "name": "giap__get_current_weather",
      "extension": "giap",
      "description": "Get current weather at the configured location."
    }
  ],
  "total": 9
}
```

---

## Memories

Memory fragments are stored in `pond_system.db`. When `settings.agent_memory_inject = true`, recent fragments are injected into the system prompt on every turn.

### GET /memories

Returns the 50 most recent memory fragments.

**Response 200**
```json
{
  "memories": [
    {
      "id": "uuid",
      "content": "User prefers temperatures in Celsius.",
      "source": "api",
      "tags": ["preferences"],
      "created_at": "2026-04-09 08:00:00"
    }
  ]
}
```

---

### POST /memories

**Request**
```json
{
  "content": "User prefers temperatures in Celsius.",
  "tags": ["preferences"],
  "source": "api"
}
```

`source` defaults to `"api"`. Other values: `"chat"` | `"sensor_summary"`

**Response 201** — no body

---

### PUT /memories/{id}

**Request** — send `content`, `tier`, or both. A field left out is left as it is.
```json
{
  "content": "User prefers temperatures in Celsius.",
  "tier": "permanent"
}
```

`tier` is `"short"` | `"long"` | `"permanent"`, and the decay rate is reset to that tier's
default. `"permanent"` keeps a memory forever: decay never archives it and consolidation never
prunes it. Rewording clears the memory's embedding, which is rebuilt on the next sweep.

**Response 204** — no body. **400** when the body changes nothing or `content` is blank.

---

### DELETE /memories/{id}

**Response 204** — no body

---

## Skills

User-defined skills are markdown instruction blocks injected as `skill:<name>` system prompt extras on every turn when `active = true`.

### GET /skills

**Response 200**
```json
{
  "skills": [
    {
      "id": "uuid",
      "name": "home_automation",
      "content": "When user asks about lights or locks, call giap__list_registered_devices first.",
      "active": true,
      "created_at": "2026-04-09 10:00:00"
    }
  ]
}
```

---

### POST /skills

**Request**
```json
{
  "name": "home_automation",
  "content": "When user asks about lights or locks, call giap__list_registered_devices first."
}
```

**Response 201** — skill object

---

### PUT /skills/{id}

Partial update — include only fields to change.

**Request**
```json
{ "active": false }
```

**Response 200** — updated skill object

| Code | Meaning |
|------|---------|
| 404 | Skill not found |

---

### DELETE /skills/{id}

**Response 204** — no body

---

## Recipes

Agent recipes are Goose Recipe YAML definitions. Running a recipe submits its `prompt` to the Goose agentic loop.

### GET /recipes

**Response 200**
```json
{
  "recipes": [
    {
      "id": "uuid",
      "name": "morning_briefing",
      "description": "Daily morning summary",
      "yaml": "title: Morning Brief\nprompt: Give me weather and schedule.",
      "active": true,
      "created_at": "2026-04-09 10:00:00"
    }
  ]
}
```

---

### POST /recipes

**Request**
```json
{
  "name": "morning_briefing",
  "description": "Daily morning summary",
  "yaml": "title: Morning Brief\nprompt: Give me weather and schedule for today."
}
```

**Response 201** — recipe object

---

### PUT /recipes/{id}

Partial update.

**Request**
```json
{ "yaml": "title: Morning Brief\nprompt: Give me weather, schedule, and reminders." }
```

**Response 200** — updated recipe object

| Code | Meaning |
|------|---------|
| 404 | Recipe not found |

---

### DELETE /recipes/{id}

**Response 204** — no body

---

## Rides

A member books a ride from **their own paired phone**. The member is the one the phone's pairing
token belongs to (`DeviceAttribution`), never a field in the request. A ride belonging to another
member answers **404**, as if it did not exist. The pond's desktop, and a phone whose pairing names
no member, get **403**. Booking needs travel switched on (`ext_travel_enabled`) and Uber sign-in
support (see *Uber accounts*) when the pond starts; without them every route answers **503**. While
travel is switched off, quote and confirm answer **503**; reading, declining and cancelling still
work.

The order is always: quote, then the member confirms. `book_ride` (the assistant's tool) only sends
the phone a `ride_offer` notification whose `data` names the drop-off; the phone then asks for the
fare from where it is.

### POST /rides/quote

**Request**
```json
{
  "pickup": { "latitude": -1.2676, "longitude": 36.8108, "name": "Home" },
  "dropoff": { "latitude": -1.319167, "longitude": 36.9275, "name": "JKIA" }
}
```

**Response 200**
```json
{
  "id": "…", "provider": "uber",
  "pickup": { "name": "Home", "latitude": -1.2676, "longitude": 36.8108 },
  "dropoff": { "name": "JKIA", "latitude": -1.319167, "longitude": 36.9275 },
  "fare": { "display": "KES 1,250", "currency_code": "KES", "expires_at": "2026-10-06T08:02:00Z" },
  "pickup_eta_mins": 4,
  "state": { "state": "awaiting_confirmation" }
}
```

`state.state` is one of `awaiting_confirmation`, `requesting`, `requested` (with `ride`: status,
driver, vehicle, `pickup_eta_mins`), `outcome_unknown` (with `reason`: Uber's answer was lost and it
may have booked the ride), `declined`, `failed` (with `reason`: Uber refused). A trip the pond took
over at startup, rather than quoted, has `pickup`, `dropoff`, `fare` and `pickup_eta_mins` null.

**422** — the drop-off is more than 150 km from the pickup; no fare is asked for.

### POST /rides/{id}/confirm

Books the ride and returns it as above, with `state.state` = `requested`.

| Status | Meaning |
|---|---|
| 202 | Uber's answer was lost and no trip of the member's shows the ride yet: `state.state` = `outcome_unknown`, and `message` says to check the Uber app. The pond keeps checking and never sends the request again |
| 404 | No such ride for this member |
| 409 | Already confirmed, declined or tried; a fare is confirmed at most once |
| 410 | The fare expired; quote again |
| 502 | Uber refused (its reason is in `error`). The ride is not retried |

### POST /rides/{id}/decline · POST /rides/{id}/cancel

**204.** Cancel works only on a booked ride (409 otherwise; for an `outcome_unknown` ride the pond
first looks for it among the member's trips under way); Uber may charge a cancellation fee.

While a ride is under way the pond reads it every `GIAP_RIDE_POLL_SECS` (default 15, at least 5)
and sends the member a `ride_update` notification (`data`: `ride_id`, `status`) on each change. If
40 reads in a row fail it stops, with a last `ride_update` whose `data` has `followed: false`.

## Uber accounts

Each household member connects their own Uber account, so rides go on their own Uber payment
method. Sign-in goes through Jarida's credentials service (`POND_CREDENTIALS_URL`), which adds the
Uber app's client secret; the member's tokens are kept in this pond's secret store only. See
[pondcredentials](architecture/pondcredentials.md).

**Host only:** every route needs the request to come from this machine **and** carry the host
credential (`X-Pond-Host-Credential`). A paired phone gets 403.

The tokens are kept under secret-store keys starting with `UBER_`, and these routes are the only
way to them: the generic secrets API (`/secrets`, `/extensions/{name}/secrets`, a marketplace
install's `secrets`) answers 403 with `"code": "secret_reserved"` for such a key, and
`GET /secrets` leaves them out.

### POST /uber/accounts/connect

**Request**
```json
{ "profile_id": "p-1" }
```

**Response 200**
```json
{ "auth_url": "https://auth.uber.com/oauth/v2/authorize?client_id=...&state=...", "state": "..." }
```

Open `auth_url` in a browser. Uber returns to `/oauth/callback`, which finishes the sign-in; follow
it with `GET /oauth/status/{state}`. A sign-in not finished within ten minutes expires (Uber's code
lives that long): the status turns `failed` and a late return is refused. When Uber itself ends the
sign-in, its `error` and `error_description` are the failure's reason.

| Status | `code` | Meaning |
|---|---|---|
| 404 | `not_a_member` | No such household member |
| 502 | `uber_relay_failed` | The credentials service could not start an Uber sign-in |
| 503 | `no_secret_store` | This pond has no secret store to keep sign-ins in |
| 503 | `uber_sign_in_off` | The credentials service is turned off (`POND_CREDENTIALS_URL=off`) |

The tokens are kept only if the member is still in the household when Uber's code has been
exchanged; a member removed meanwhile gets nothing kept, and the sign-in is reported failed.

### GET /uber/accounts

**Response 200**
```json
{ "connected": ["p-1"] }
```

Only members still in the household are listed. Works with the credentials service turned off.

### DELETE /uber/accounts/{profile_id}

**Response 204.** Forgets the member's tokens on this pond. Their Uber account is untouched. Works
with the credentials service turned off.

Every refusal on these routes is `{ "error": "...", "code": "..." }`; a 403 off this machine has
`code` `host_only`.

## Error Codes Summary

| Code | Meaning |
|------|---------|
| 200 | OK |
| 201 | Created |
| 202 | Accepted (async operation started) |
| 204 | No Content (success, no body) |
| 400 | Bad Request — invalid or missing body/params |
| 403 | Forbidden — onboarding not complete, or cannot delete system resource |
| 404 | Not Found |
| 409 | Conflict — e.g. deleting a model assigned to an active role |
| 500 | Internal Server Error |
| 501 | Not Implemented — optional repository not configured for this deployment |
| 502 | Bad Gateway — upstream service (Whisper, etc.) returned an error |
| 503 | Service Unavailable — required backend (scheduler, extension manager, TTS, etc.) not running |
