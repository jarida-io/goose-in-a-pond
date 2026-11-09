# Settings defaults and user intent

How `Settings` is stored, how a new field is added without being lost, and how a changed default
reaches installs that already exist without overriding a household's choice.

## Storage

`Settings` (`crates/pond-core/src/user_data/domain/settings.rs`) is persisted as a flat key-value
table, `settings(key, value, updated_at, is_user_set)`, one row per field. A new field is a new
row, so it needs no migration. `Settings::default()` supplies a value only when the key is
**absent**: once anything writes a key, the stored value wins over every later default.

The SQLite adapter is `crates/pond-infra/src/sqlite_settings.rs`. Every write is
`INSERT ... ON CONFLICT(key) DO UPDATE`, never `INSERT OR REPLACE`, which would delete the row
and reset `is_user_set`.

## Factory defaults: `factory_defaults.toml`

Most `default_*()` functions read their literal from `crates/pond-core/config/factory_defaults.toml`
via `user_data::domain::factory_defaults`, instead of returning a hardcoded value directly. The
file is embedded into the binary at compile time (`include_str!`), so there is no runtime
file-path or missing-file risk — it is as reliable as a hardcoded literal, just in one readable
place instead of scattered across ~120 functions.

Every lookup takes a fallback — the same literal the function used to return directly — so a
missing key or an unparsable file degrades to that fallback rather than breaking startup. A test
in `factory_defaults.rs` (`every_expected_key_is_present_in_the_file`) catches the file and the
code drifting apart instead of letting it fail silently.

A handful of fields are deliberately **not** in this file: ones whose default already comes from a
named constant owned by another module (several `vision_*` fields, `compaction_verbatim_days`,
`agent_prefill_retention_days`) keep reading that constant directly. Moving them into the TOML
file would create a second, independently-editable copy of a value another part of the codebase
already treats as its single source of truth.

This is unrelated to `DEFAULT_ADOPTIONS` below, which never reads `default_*()` or this file — it
compares a *stored* value against literal old/new snapshots taken at the time a default changed,
so moving where "the current default" lives does not affect it.

## Adding a field

Each field needs all of these, or it deserializes, applies, and vanishes on the next read:

1. The field on `Settings` with a `#[serde(default = "...")]` and its `default_*()` function —
   add its factory literal to `factory_defaults.toml` too, unless it falls under the exception
   above.
2. An `upsert!` line in `SqliteSettingsRepository::update_fields`.
3. An arm in `apply_key` (same file), which turns the stored string back into the field.
4. A disposition in `every_settings_field_is_dispositioned` (pond-core): `UI_WIRED` (a desktop
   control and a mirror in `pond-desktop/src/api/types.ts`) or `HEADLESS_BY_DESIGN`.
5. If it takes a closed set of values, a `FieldRule` in `settings_validation.rs`;
   `PUT /api/v1/settings` then answers 422 naming the field and the accepted values.

`roundtrip_persists_every_field` (pond-infra) fails when step 2 or 3 is missing;
`every_settings_field_is_dispositioned` fails when step 4 is. The user-facing catalogue of
settings is [settings-catalogue.md](../architecture/settings-catalogue.md).

## Recording intent: `is_user_set`

A row means "something wrote this". `is_user_set = 1` (migration `0035_settings_user_intent.sql`)
means "the user chose this". Only the `update_settings` handler sets it, from the keys of the
request **patch**:

- `PUT /api/v1/settings` writes only the keys the request carries
  (`update_fields(&merged, Some(&write_keys))`) and marks them with `mark_user_set(&user_keys)`.
- Server-derived values are written but not marked: geocoded `weather_latitude` /
  `weather_longitude` join `write_keys`, not `user_keys`.
- Server-side snapshot writes (`update()`, `update_fields(_, None)`, `set_key`) mark nothing.

`SettingsRepository::mark_user_set` and `is_user_set` have no-op trait defaults; the SQLite
adapter and `MockSettingsRepository` implement them.

### The client half

The marker is only as good as the patch. A client that PUTs the whole settings object marks
every key on its first save and ends default adoption for that install. `PUT /api/v1/settings` is
a **patch endpoint**: send only the keys that changed.

- The hub settings views (`pond-desktop/src/hub/views/settings/*`) send one field per change.
- The settings catalogue (`pond-desktop/src/settings/SettingsCatalogue.tsx`) batches edits behind
  one Save. It keeps the last server snapshot as a baseline and sends
  `diffSettings(baseline, settings)` (`settings/state.ts`); with no edits it sends nothing. A
  stale full-object save would also revert whatever another surface changed since the panel
  loaded.
- Server-owned fields stay out of every diff. `voice_wake_word_transcriptions` is written by
  `POST /api/v1/voice/calibrate` and cleared by `DELETE`; after calibrating, the catalogue
  re-reads settings and folds them in.

### The f32 echo is not the literal you sent

serde_json has no `f32`: `serialize_f32` widens to `f64`, so every float field
(`llm_temperature`, `voice_kws_energy_threshold`, `memory_prune_threshold`,
`memory_archive_threshold`, `memory_decay_*`) echoes as a different number, `0.7` as
`0.699999988079071`. That is the value the server holds, so the server does not round it.

A client that diffs its local value against the echo sees every float as permanently edited,
re-sends it on each save, and re-marks it `is_user_set`. After a successful PUT, fold against the
patch that was sent, not the old baseline:

```ts
setSettings((prev) => foldServerState(prev, { ...baseline, ...body }, updated));
setBaseline(structuredClone(updated));
```

The request succeeded, so the sent keys are what the server holds; the widened echo is adopted
and the field settles on the first save. A server-side clamp sticks the same way. Do not compare
with an epsilon or `Math.fround`: `Settings` also holds integers, and `Math.fround` collapses
distinct integers above 2^24.

## Changing a default: `DEFAULT_ADOPTIONS`

`DEFAULT_ADOPTIONS` in `settings.rs` registers every factory default that changed after installs
existed:

| Key | Old | New | Migration |
|---|---|---|---|
| `agent_max_turns` | `20` | `50` | 0035 |
| `hybrid_compaction_enabled` | `false` | `true` | 0035 |
| `vad_backend` | `rms` | `silero` | 0052 |
| `voice_max_turns` | `8` | `0` | 0053 |

Each migration adopts with one narrow rule: a stored value still **byte-equal to the old
default**, never marked by the user, takes the new one.

```sql
UPDATE settings SET value = '50' WHERE key = 'agent_max_turns'
  AND value = '20' AND is_user_set = 0;
```

The accepted false positive: a user who deliberately chose exactly the old default before 0035
cannot be told apart from one who never chose. They move once; their next explicit save marks the
key.

A stored value that was never a factory default is never adopted. `embedding_provider = 'none'`
is such a value (the default has always been `fastembed`), so an install holding it stays there
until someone changes it.

Ordering and idempotency: migrations run in `Database::init` before anything reads settings; the
`ALTER TABLE` precedes the UPDATEs; every UPDATE is guarded on the stored value and on
`is_user_set = 0`, so a re-run is a no-op and can never walk back a marked key. `is_user_set` is a
column, not a `Settings` field, so it needs no disposition or TypeScript mirror.

### Adding the next default change

1. Change the `Settings::default_*()` function.
2. `every_adoption_entry_states_the_literal_the_adapter_writes` (pond-infra) now fails and its
   message names the entry to add. It lives in pond-infra because only the adapter knows how a
   value is rendered into the store: the adapter writes numbers with `Display`, while a serde_json
   round-trip widens `f32` (`0.05f32` stores as `0.05` but renders as `0.05000000074505806`).
3. **Append** a `DefaultAdoption` with the new migration number. Never edit an existing entry: its
   migration has already run on real machines.
4. Add `00NN_*.sql` with one guarded UPDATE per entry.
5. Run `cargo test -p pond-core -p pond-infra`. The replay tests and the SQL tie enumerate the
   registry and read the migration directory, so nothing else needs wiring.

A key that moves again gets a chained entry, never an edit:

```rust
DefaultAdoption { key: "agent_max_turns", old_default: "20", new_default: "50", migration: "0035" },
DefaultAdoption { key: "agent_max_turns", old_default: "50", new_default: "80", migration: "00NN" },
```

The second migration guards on `value = '50'`, so it moves installs that adopted 50 and installs
created with 50 alike.

## What the tests guarantee

| Test | Crate | Guarantee |
|---|---|---|
| `default_adoptions_are_structurally_sound` | pond-core | Every key is a real field, no entry is a no-op, and entries for one key chain (`old[n] == new[n-1]`) in ascending migration order |
| `adoption_defects_are_rejected` | pond-core | Each structural rule actually fires |
| `every_adoption_entry_states_the_literal_the_adapter_writes` | pond-infra | Writes `Settings::default()` through the real adapter: the newest entry per key states what an install stores |
| `every_adoption_entry_has_matching_migration_sql` | pond-infra | Textual tie: each entry's migration has an UPDATE with its key, old value, new value and `is_user_set = 0` guard, and no UPDATE is unregistered |
| `migration_lookup_resolves_to_the_shipped_files` | pond-infra | The forward tie reads the real migration directory |
| `migration_adopts_defaults_the_user_never_chose`, `migration_leaves_a_deliberately_different_value_alone`, `migration_is_idempotent` | pond-infra | Replays the shipped SQL against seeded rows |
| `only_a_patch_write_records_user_intent` | pond-infra | Snapshot writers mark nothing; `ON CONFLICT` keeps the marker |
| `roundtrip_persists_every_field` | pond-infra | Every field survives a write and a read |
| `every_settings_field_is_dispositioned` | pond-core | Every field is UI-wired or headless by design |
| `sends only what changed, and adopts the value the server answers with` (`settings/SettingsCatalogue.test.tsx`) | pond-desktop | The catalogue sends a patch and converges on the echo |
| `diffSettings`, `foldServerState` (`settings/state.test.ts`) | pond-desktop | Arrays and maps compare by value; a background refresh moves the baseline so untouched fields stay out of the next patch |
