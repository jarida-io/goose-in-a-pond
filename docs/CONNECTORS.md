# Connectors

A connector is a service the Pond can connect to: a mailbox, a calendar, the web search key, the
home's devices, a member's Uber or Spotify, an MCP server from the marketplace, or a webhook any
service can call. My Goose on the phone lists them (`GET /api/v1/me/connectors`), draws each one's
setup form, and connects or disconnects it. The phone draws whatever a manifest declares, so a new
connector needs no app release.

The contract the phone reads is section 5 of
[architecture/my-goose-hub.md](architecture/my-goose-hub.md). This page is for the developer who
adds one.

## Where things are

| What | Where |
|---|---|
| Built-in manifests | `connectors/*.json`, compiled in by `BUILT_IN` in `crates/pond-core/src/shared/domain/connector.rs` |
| Manifest types and checks | `crates/pond-core/src/shared/domain/connector.rs` (`ConnectorManifest`, `problems`) |
| States, read from what the Pond keeps | `crates/pond-core/src/shared/services/connector_hub.rs` |
| Routes, connect and disconnect | `crates/pond-api/src/connectors.rs` |
| MCP servers anyone can install | `crates/pond-core/src/extensions/marketplace_registry.json` |
| Events for to-do hooks | the `hook_events` table, written through `HookEventSink` |

## A manifest

```json
{
  "id": "mail",
  "name": "Email",
  "summary": "Goose reads new mail in an inbox you connect, so it can tell you what came in.",
  "icon": "mail",
  "category": "mail",
  "kind": "account",
  "connect": { "via": "context_source", "source_kind": "mail" },
  "setup": {
    "fields": [
      { "key": "provider", "label": "Mail service", "kind": "select", "required": true,
        "options": [{ "value": "gmail", "label": "Gmail" }] },
      { "key": "username", "label": "Email address", "kind": "text", "required": true },
      { "key": "password", "label": "App password", "kind": "secret", "required": true,
        "help": "Make an app password in your mail service's security settings." }
    ]
  },
  "events": [{ "id": "new_mail", "label": "A new email arrives" }],
  "feeds": ["todos"]
}
```

| Field | Meaning |
|---|---|
| `id` | Lower-case letters, digits, `-` and `_`. The connector's id in the routes. |
| `name`, `summary` | What the member reads. Plain words, no system names. |
| `icon` | A name from the phone's icon registry. |
| `category` | `mail`, `calendar`, `home`, `web`, `travel`, `weather`, `music`, `money` or `custom`. |
| `kind` | `built_in`, `account`, `mcp` or `webhook`; it must match `connect.via` (below). |
| `connect` | How the Pond connects it. |
| `setup.fields` | What the phone asks for before connecting. |
| `setup.oauth` | Leave it out: the Pond fills it with a sign-in address when connecting needs a browser. |
| `events` | The events it raises for to-do hooks, from the vocabulary below. |
| `feeds` | Which parts of My Goose it can feed: `news`, `todos`, `ideas`. |
| `tools` | Tools it brings, when no running extension can be counted. |
| `extension` | The running extension whose tools it brings (`giap-weather`), for the count. |

### `connect.via`

Each way goes through the service behind an existing route; a connector never holds a second copy
of it.

| `via` | Kind | Connects by | Its state comes from |
|---|---|---|---|
| `setting` (`setting`, `also`, `needs_location`) | `built_in` | switching those settings on through `PUT /settings`'s handler | the settings |
| `matter` (`setting`) | `built_in` | switching the tool group on | the Matter runtime's status |
| `web_search_key` | `built_in` | the Brave key route, which checks the key once | the key and the Knowledge switch |
| `context_source` (`source_kind`) | `account` | `POST /context/sources` with the fields as credentials | the member's sources |
| `oauth` (`provider`) | `account` | starting the provider's sign-in; its address comes back in `setup.oauth` | a stored token |
| `uber` | `account` | nothing from a phone: Uber is connected on the Pond itself | the member's Uber sign-in |
| `webhook` | `webhook` | making a webhook (below) | the member's webhooks |
| `marketplace` (`extension`) | `mcp` | installing that marketplace entry with the fields as its secrets, or switching it back on | the extension manager |

A connection that raises events (a mailbox, a calendar, a webhook) is also listed on its own, under
the id its events carry: `mail:<source id>`, `calendar:<source id>`, `webhook:<id>`. That is the
`connector_id` a to-do hook names, and the one to disconnect.

### Setup fields

| Field | Meaning |
|---|---|
| `key` | The name the value is sent under, in `{"fields": {...}}`. |
| `label`, `help` | What the member reads. |
| `kind` | `text`, `secret` (masked, never shown back), `url`, `toggle` or `select`. |
| `options` | For a `select` only: `[{"value", "label"}]`. |
| `required` | Whether connecting needs it. |

The phone draws one form from these and posts it to `POST /me/connectors/{id}/connect`. When the
answer carries `setup.oauth`, the phone opens that address instead. The sign-in returns to the
Pond's own address (`127.0.0.1`), so it finishes in a browser on the Pond itself.

### Events

The vocabulary is the one in [architecture/my-goose-hub.md](architecture/my-goose-hub.md). A
connector raises:

| `source_kind` / `source_id` | `event` | `payload` |
|---|---|---|
| `connector` / `mail:<context source id>` | `new_mail` | `{from, subject}` |
| `connector` / `calendar:<context source id>` | `event_added`, `event_starting` | `{title, starts_at}` |
| `connector` / `webhook:<connector_webhooks id>` | `call` | the JSON body the caller sent |

A manifest may only declare events its `via` raises (`EVENT_VOCABULARY`); a new source of events
adds its row there and writes them through `HookEventSink`. Write `profile_id` as the member the
event is about, and give it a `once_key` when the same event could be seen twice.

## Adding a connector

1. **An MCP server.** Add it to `marketplace_registry.json`: its command or URI, its tools, and
   the secrets it needs (`required_secrets`). It is listed at once as `mcp:<id>`, its secrets as
   its setup fields, and connecting installs it. Write a manifest only to give it its own card
   (`"connect": {"via": "marketplace", "extension": "<id>"}` with a name, summary, icon, category
   and feeds).
2. **A service that calls in.** Nothing to build: the member makes a webhook (below).
3. **A switch or an account the Pond already has a service for.** Write a manifest with the `via`
   that names it, add it to `BUILT_IN`, and run the checks.
4. **Something new to sync.** Build the port and adapter as `HOWTOAI.md` section 5 says, give
   `ConnectVia` a variant, add its arm to `connector_hub::of_manifest` (its state) and to
   `connect` and `disconnect` in `crates/pond-api/src/connectors.rs`, and write its events.

The checks:

```bash
cargo test -p pond-core shared::domain::connector    # every manifest parses, is sound, names real settings
cargo test -p pond-core shared::services::connector_hub
```

`problems(&manifest)` lists what is wrong with a manifest in sentences: a bad id or category, a
`kind` that does not match its `via`, an event its `via` never raises, a feed that is not one, a
field twice, or a `select` without options.

## The webhook connector

`POST /api/v1/me/connectors/webhook` `{"name": "GitHub"}` answers the new connector
(`webhook:<id>`) with its `webhook_url`, once:

```
https://<the address the phone reached the Pond at>/api/v1/hooks/in/<token>
```

The address is the request's host, else the Pond's remote-access address. Only the token's
SHA-256 is kept, so a lost address means a new webhook. A service on the open internet can call it
only when that address is reachable from there.

`POST /api/v1/hooks/in/<token>` is public: the token is the credential. It takes any JSON body up
to 64 KB, answers `202`, and writes a `hook_events` row (`connector` / `webhook:<id>` / `call`,
the body as its payload). Each token may call 30 times a minute; past that it gets `429` and a
`Retry-After`. An unknown token gets `404`, and a body that is not JSON `400`.

## Worked example: GitHub notifications

A member wants a reminder to review a pull request whenever one is opened on `jarida-io/goose-in-a-pond`.

1. In My Goose, Connectors, they add "Your own webhook" and name it GitHub. The phone posts
   `{"name": "GitHub"}` and shows the `webhook_url` once, to copy.
2. On GitHub, under the repository's Settings, Webhooks, Add webhook: the payload URL is that
   address, the content type `application/json`, and the event "Pull requests".
3. Each delivery becomes a row:

   ```json
   {"source_kind": "connector", "source_id": "webhook:3f2c...", "event": "call",
    "payload": {"action": "opened", "pull_request": {"title": "...", "html_url": "..."}}}
   ```

4. A to-do of theirs hooks it: `{"kind": "connector", "spec": {"connector_id": "webhook:3f2c...",
   "event": "call", "filter": {"action": "opened"}}}`. When the hook fires, the reminder reaches
   their phone.

To make this a connector of its own, with its own card in the list, write a manifest whose
`via` is `marketplace` for the GitHub MCP server already in the marketplace (it brings the tools
to read the pull request too), and keep the webhook for the events: one gives Goose hands, the
other tells it when to use them.
