import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { Switch, Button } from "@heroui/react";
import {
  Search,
  Crosshair,
  AlertCircle,
  Wand2,
  ChevronDown,
  X,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { BackgroundJobs } from "./BackgroundJobs";
import type {
  ModelEntry,
  RetitleResult,
  Settings,
  ZoneChoice,
} from "../api/types";
import { allZones, detectPlace, deviceZone } from "../lib/place";
import { diffSettings, foldServerState } from "./state";
import { ErrorBanner, SkeletonList } from "../components/shared";
import {
  CATALOGUE,
  TIER_NOTE,
  allEntries,
  inertCount,
  type CatalogueCategory,
  type Consumer,
  type Entry,
  type OptionSource,
  type Subcategory,
} from "./catalogue";
import { AppearanceView } from "../hub/views/settings/Appearance";
import { WakeWordCalibration } from "../components/WakeWordCalibration";
import "../styles/settings-catalogue.css";

// ─── Marks ────────────────────────────────────────────────────────────────
// The signature of this page: every control says whether anything reads it.

const MARK_TITLE: Record<Consumer, string> = {
  live: "Connected — the pond reads this and acts on it",
  app: "App only — this app honours it, the pond never sees it",
  none: "Not connected — nothing reads this yet",
};

function Mark({ consumer }: { consumer: Consumer }) {
  return (
    <span
      className={`scat__dot scat__dot--${consumer}`}
      title={MARK_TITLE[consumer]}
    />
  );
}

// ─── Re-titling result ────────────────────────────────────────────────────

/**
 * One short line describing what asking for a re-titling pass answered.
 *
 * Sits beside the button in a narrow column, so it stays terse.
 *
 * It used to report "Renamed 4 conversations", and that reading is gone on
 * purpose: producing it meant holding the request open through every model
 * call, which on a small board took minutes and returned a timeout rather than
 * a count. The pass now runs on the inference lane like every other background
 * job, so the honest thing this button can say is that it started — the
 * Automations panel shows it running, and the conversation list shows the
 * names.
 */
export function summariseRetitle(r: RetitleResult): string {
  if (r.started) return "Renaming — names appear as it goes";
  // A pond with no titling loop is not a broken pond, and saying so is the
  // distinction this line exists for: the person must be able to tell "nothing
  // here does this" from "it did not work".
  return r.reason ?? "Nothing here to run";
}

// ─── Options from the model registry ──────────────────────────────────────

/**
 * Provider filters copied from `sections/Models.tsx`, which is this app's
 * existing authority on which `provider` value belongs to which role. Kept as
 * one table so the two cannot drift apart silently.
 */
const PROVIDERS: Record<
  Exclude<OptionSource, "llm-providers" | "time-zones">,
  (m: ModelEntry) => boolean
> = {
  "llm-models": (m) => ["gguf", "llamafile", "ollama"].includes(m.provider),
  "whisper-models": (m) => m.provider === "whisper",
  "tts-voices": (m) =>
    ["tts", "tts_piper", "tts_kokoro", "tts_http"].includes(m.provider),
  "embedding-models": (m) =>
    m.provider === "embedding" || m.category === "embedding",
};

interface Option {
  value: string;
  label: string;
}

function optionsFor(
  source: OptionSource,
  models: ModelEntry[],
  zones: ZoneChoice[],
  meshEnabled?: boolean,
): Option[] {
  if (source === "time-zones") {
    // "Africa/Nairobi — Nairobi (+03:00)". The offset is worth showing: it is
    // how somebody confirms they picked the right one of two zones with
    // similar names, and it is resolved for TODAY rather than assumed.
    return zones.map((z) => ({
      value: z.zone,
      label: z.place
        ? `${z.zone} — ${z.place} (${z.offset})`
        : `${z.zone} (${z.offset})`,
    }));
  }
  if (source === "llm-providers") {
    const seen = [
      ...new Set(models.filter(PROVIDERS["llm-models"]).map((m) => m.provider)),
    ];
    // "mesh" (#132) has no catalog row — it is not a downloadable model, it is
    // a trusted peer's compute — so it can never appear via the `models` scan
    // above. Gated on `mesh_enabled`, same principle as the `downloaded`
    // filter below: offering a provider that cannot actually serve a turn
    // right now is offering a failure, not a choice.
    const providers = meshEnabled ? [...seen, "mesh"] : seen;
    return providers.sort().map((p) => ({ value: p, label: p }));
  }
  return (
    models
      .filter(PROVIDERS[source])
      // A model the device has not downloaded cannot be selected into service,
      // so offering it would be offering a failure. `downloaded` is optional in
      // the registry, and absent means "not tracked" rather than "missing".
      .filter((m) => m.downloaded !== false)
      .map((m) => ({
        value: source === "tts-voices" ? (m.filename ?? m.name) : m.name,
        label: m.display_name ?? m.name,
      }))
      .sort((a, b) => a.label.localeCompare(b.label))
  );
}

// ─── Value helpers ────────────────────────────────────────────────────────

/**
 * Render a stored value into a text box.
 *
 * `retention_events_by_category` is the one map-valued setting, shown as
 * `network 14, sensor 7` rather than raw JSON — nobody editing retention should
 * have to type braces. `parseText` is its inverse.
 */
function textValue(v: unknown): string {
  if (v == null) return "";
  if (Array.isArray(v)) return v.join(", ");
  if (typeof v === "object") {
    return Object.entries(v as Record<string, number>)
      .map(([k, n]) => `${k} ${n}`)
      .join(", ");
  }
  return String(v);
}

const NULLABLE_TEXT = new Set([
  "custom_system_prompt",
  "searxng_url",
  "voice_kws_whisper_url",
]);

/** Inverse of `textValue`. Returns the shape the server expects for this key. */
function parseText(key: string, raw: string): unknown {
  if (key === "voice_wake_word_transcriptions") {
    return raw
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
  }
  if (key === "retention_events_by_category") {
    const out: Record<string, number> = {};
    for (const part of raw.split(",")) {
      const [name, days] = part.trim().split(/\s+/);
      const n = Number(days);
      if (name && Number.isFinite(n)) out[name] = n;
    }
    return out;
  }
  // An emptied box means "unset", not the empty string — both of these are
  // `Option<String>` server-side.
  if (raw === "" && NULLABLE_TEXT.has(key)) return null;
  return raw;
}

// ─── The sentence ─────────────────────────────────────────────────────────

interface Clause {
  text: string;
  category: string;
}

/** What the pond may currently do, as one sentence, before you touch anything. */
function postureClauses(s: Partial<Settings>): {
  clauses: Clause[];
  reach: Clause | null;
  tail: string;
} {
  const clauses: Clause[] = [
    {
      text: (s.mic_enabled ?? true) ? "listens" : "hears nothing",
      category: "privacy",
    },
    {
      text: (s.vision_enabled ?? false) ? "watches" : "watches nothing",
      category: "vision",
    },
    {
      text:
        (s.memory_extraction_enabled ?? true)
          ? "remembers what you tell it"
          : "forgets everything",
      category: "memory",
    },
    {
      text:
        (s.unprompted_speech_enabled ?? false)
          ? "speaks on its own"
          : "speaks only when spoken to",
      category: "automation",
    },
  ];
  const mode = s.network_mode ?? "open";
  if (mode === "offline")
    return { clauses, reach: null, tail: "Nothing leaves this house." };
  if (mode === "allowlist")
    return {
      clauses,
      reach: null,
      tail: "It reaches only the hosts you allow.",
    };
  return {
    clauses,
    reach: { text: "any host on the internet", category: "privacy" },
    tail: "",
  };
}

// ─── Row ──────────────────────────────────────────────────────────────────

function EntryRow({
  entry,
  value,
  error,
  options,
  onChange,
  extra,
  dev = false,
}: {
  entry: Entry;
  value: unknown;
  error: string | null;
  options: Option[] | null;
  onChange: (key: keyof Settings, v: unknown) => void;
  extra?: React.ReactNode;
  /** Developer view: reveals field names, types and the consumer marks. */
  dev?: boolean;
}) {
  // A control nothing reads is not offered. Leaving it operable would let
  // someone spend a decision on a value that changes nothing — the exact
  // defect this page exists to surface.
  const inert = entry.consumer === "none";
  const { control } = entry;
  const errId = error ? `scat-err-${entry.key}` : undefined;
  const described = [errId].filter(Boolean).join(" ") || undefined;

  return (
    <div className="scat__row" data-invalid={error ? "true" : undefined}>
      <div className="scat__rowMain">
        <div className="scat__rowLabel">
          {dev && <Mark consumer={entry.consumer} />}
          <span>{entry.label}</span>
          {entry.proposed && <span className="scat__new">New</span>}
        </div>

        {/* Always. A control whose effect has to be guessed is a control the
            household will leave alone, which is the same as not shipping it. */}
        <p className="scat__desc">{entry.description}</p>

        {/* The field name is a maintenance detail. Somebody living here cannot
            act on `voice_tts_quality`, and showing it invites them to think the
            interface is talking to someone else. */}
        {dev && <div className="scat__key">{entry.key}</div>}

        {/* Likewise the note: it exists to explain why a mark is not "connected",
            which is only meaningful once the marks are visible. */}
        {dev && entry.note && (
          <p className={`scat__note scat__note--${entry.consumer}`}>
            <Mark consumer={entry.consumer} />
            <span>{entry.note}</span>
          </p>
        )}

        {control.kind === "radio" && (
          <div
            className="scat__radios"
            role="radiogroup"
            aria-label={entry.label}
          >
            {control.options.map((o) => (
              <label className="scat__radio" key={o.value}>
                <input
                  type="radio"
                  name={`scat-${entry.key}`}
                  value={o.value}
                  disabled={inert}
                  checked={String(value ?? "") === o.value}
                  onChange={() => onChange(entry.key, o.value)}
                />
                <span className="scat__radioBody">
                  <span className="scat__radioLabel">{o.label}</span>
                  {o.hint && <span className="scat__radioHint">{o.hint}</span>}
                </span>
              </label>
            ))}
          </div>
        )}

        {error && (
          <p className="scat__error" id={errId} role="alert">
            <AlertCircle size={13} aria-hidden="true" />
            <span>{error}</span>
          </p>
        )}
      </div>

      {control.kind !== "radio" && (
        <div className="scat__ctl">
          {control.kind === "toggle" && (
            <span
              className={
                inert ? "scat__swWrap scat__swWrap--inert" : "scat__swWrap"
              }
            >
              <Switch
                aria-label={entry.label}
                isSelected={Boolean(value)}
                isDisabled={inert}
                onChange={(v: boolean) => onChange(entry.key, v)}
              >
                <Switch.Content>
                  <Switch.Control>
                    <Switch.Thumb />
                  </Switch.Control>
                </Switch.Content>
              </Switch>
            </span>
          )}

          {control.kind === "select" && (
            <select
              className="native-select scat__field"
              aria-label={entry.label}
              aria-describedby={described}
              disabled={inert}
              value={String(value ?? control.options[0])}
              onChange={(e) => onChange(entry.key, e.target.value)}
            >
              {control.options.map((o) => (
                <option key={o} value={o}>
                  {o}
                </option>
              ))}
            </select>
          )}

          {control.kind === "lookup" &&
            (options === null ? (
              // The registry has not answered yet, or could not be reached.
              // Free text rather than an empty picker: an empty dropdown offers
              // nothing and hides the value that is already set.
              <input
                type="text"
                className="native-input scat__field scat__field--text"
                aria-label={entry.label}
                aria-describedby={described}
                disabled={inert}
                placeholder={control.placeholder}
                value={textValue(value)}
                onChange={(e) => onChange(entry.key, e.target.value)}
              />
            ) : (
              <select
                className="native-select scat__field scat__field--text"
                aria-label={entry.label}
                aria-describedby={described}
                disabled={inert}
                value={String(value ?? "")}
                onChange={(e) => onChange(entry.key, e.target.value)}
              >
                <option value="">{control.placeholder ?? "Not set"}</option>
                {/* A stored value the registry does not list is kept and shown,
                    so opening this page can never silently drop a model that is
                    configured but not currently installed. */}
                {Boolean(value) &&
                  !options.some((o) => o.value === String(value)) && (
                    <option value={String(value)}>
                      {String(value)} — not installed
                    </option>
                  )}
                {options.map((o) => (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </select>
            ))}

          {control.kind === "number" && (
            <span className="scat__num">
              <input
                type="number"
                className="native-input scat__field scat__field--num"
                aria-label={entry.label}
                aria-describedby={described}
                aria-invalid={error ? true : undefined}
                disabled={inert}
                step={control.step}
                min={control.min}
                max={control.max}
                value={value == null ? "" : String(value)}
                // Emptying a number box means "unset", which is not 0 — sending
                // 0 for a blank latitude would move the home to the Gulf of
                // Guinea without anyone asking for it.
                onChange={(e) =>
                  onChange(
                    entry.key,
                    e.target.value === "" ? null : Number(e.target.value),
                  )
                }
              />
              {control.unit && (
                <span className="scat__unit">{control.unit}</span>
              )}
            </span>
          )}

          {control.kind === "text" && (
            <input
              type="text"
              className="native-input scat__field scat__field--text"
              aria-label={entry.label}
              aria-describedby={described}
              aria-invalid={error ? true : undefined}
              disabled={inert}
              placeholder={control.placeholder}
              value={textValue(value)}
              onChange={(e) =>
                onChange(entry.key, parseText(entry.key, e.target.value))
              }
            />
          )}

          {extra}
        </div>
      )}
    </div>
  );
}

// ─── Page ─────────────────────────────────────────────────────────────────

export function SettingsCatalogueView({
  onBack,
}: { onBack?: () => void } = {}) {
  const [settings, setSettings] = useState<Partial<Settings>>({});
  const [baseline, setBaseline] = useState<Partial<Settings>>({});
  const [models, setModels] = useState<ModelEntry[] | null>(null);
  const [zones, setZones] = useState<ZoneChoice[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [categoryId, setCategoryId] = useState(CATALOGUE[0].id);
  const [query, setQuery] = useState("");
  /**
   * Developer view. Off by default, so what a household sees is labels and
   * descriptions; on, it adds field names, the consumer marks and their notes.
   */
  const [dev, setDev] = useState(false);
  /**
   * Re-running setup is destructive enough to deserve a second press — it
   * throws away the answers the wizard collected — but not a modal, because
   * this only exists behind the developer flag in the first place.
   */
  const [armed, setArmed] = useState(false);
  const [onboarding, setOnboarding] = useState(false);
  const [onboardErr, setOnboardErr] = useState<string | null>(null);

  /**
   * Reset on the server, then reload.
   *
   * Onboarding is a whole-app mode, decided in `App.tsx` from the status the
   * backend reports — so the honest way back into it is to re-arm the guard and
   * let the app read that on the next load. Reaching for the app dispatcher
   * from here would couple a settings panel to the shell for no gain.
   */
  const startOnboarding = useCallback(async () => {
    if (!armed) {
      setArmed(true);
      return;
    }
    setOnboarding(true);
    setOnboardErr(null);
    try {
      await api.resetOnboarding();
      window.location.reload();
    } catch (e) {
      setOnboardErr(e instanceof Error ? e.message : String(e));
      setOnboarding(false);
      setArmed(false);
    }
  }, [armed]);

  // Leaving developer view disarms it, so the button is never found half-pressed.
  useEffect(() => {
    if (!dev) {
      setArmed(false);
      setOnboardErr(null);
    }
  }, [dev]);

  const [retitling, setRetitling] = useState(false);
  const [retitleNote, setRetitleNote] = useState<string | null>(null);

  /**
   * Ask the pond to rename conversations on demand.
   *
   * Answers in milliseconds now: the work happens on the inference lane rather
   * than inside the request, so the button reports that a pass has started
   * rather than waiting minutes to report a count and usually timing out first.
   * Nothing on this page shows conversation names, so there is nothing to
   * refresh here.
   */
  const runRetitle = useCallback(async () => {
    setRetitling(true);
    setRetitleNote(null);
    try {
      setRetitleNote(summariseRetitle(await api.retitleSessions()));
    } catch (e) {
      setRetitleNote(e instanceof Error ? e.message : String(e));
    } finally {
      setRetitling(false);
    }
  }, []);

  const load = useCallback(() => {
    setLoading(true);
    setLoadError(null);
    api
      .getSettings()
      .then((s) => {
        // Checked, not trusted. `request` casts its parsed body to `T`, so a
        // reply that is not settings — an empty body, or the SPA's own
        // index.html, which is what `dev:vite` serves for /api when no backend
        // is running — arrives typed as `Settings` and undefined at runtime.
        // The `errors` memo then indexes it by every catalogue key and the
        // whole page dies on the first one, which is `user_name`. The docs
        // promise this case shows "their error state"; it showed a crash.
        if (!s || typeof s !== "object" || Array.isArray(s)) {
          setLoadError(
            "The pond answered, but not with settings. Is the server running?",
          );
          return;
        }
        setSettings(s);
        setBaseline(structuredClone(s));
      })
      .catch((e) => setLoadError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(load, [load]);

  useEffect(() => {
    // The registry only fills pickers. A failure here degrades every lookup to
    // a text box rather than blocking the page, so it is not a load error.
    let cancelled = false;
    api
      .listModels()
      .then((m) => !cancelled && setModels(m))
      .catch(() => !cancelled && setModels(null));
    // Zones, from the server's IANA catalogue. `allZones` falls back to this
    // webview's own `Intl` list and never rejects, so this cannot fail the
    // page — at worst the picker degrades to a text box, same as the rest.
    allZones()
      .then((z) => !cancelled && setZones(z))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  /** Every field whose value fails its own validator. Blocks Save. */
  const errors = useMemo(() => {
    const out: Record<string, string> = {};
    for (const e of allEntries()) {
      if (e.consumer === "none") continue;
      const raw = (settings as Record<string, unknown>)[e.key];

      // ABSENT IS NOT INVALID. A key the server did not send is not something a
      // person can see or fix, and flagging it would block every save on a
      // partial response — an older server, or a field added since. Only a
      // value that is actually present gets judged.
      if (raw === undefined) continue;

      // An emptied number box is a different matter: no numeric setting is
      // `Option<_>` server-side, so sending null earns a 422. Catching it here
      // names the box instead of failing the whole save.
      if (raw === null && e.control.kind === "number") {
        out[e.key] = "Enter a number.";
        continue;
      }

      if (!e.validate) continue;
      const msg = e.validate(raw);
      if (msg) out[e.key] = msg;
    }
    return out;
  }, [settings]);

  const errorCount = Object.keys(errors).length;
  const dirty = useMemo(
    () => Object.keys(diffSettings(baseline, settings)).length,
    [baseline, settings],
  );

  const patch = useCallback((key: keyof Settings, value: unknown) => {
    setSaved(false);
    setSaveError(null);
    setSettings((prev) => ({ ...prev, [key]: value }));
  }, []);

  /**
   * The banner and the search box both start folded on a short screen.
   *
   * A 7-inch panel is 1024x600. The banner is ~180px and the masthead another
   * ~90, so unfolded they take nearly half the height before a single setting
   * is visible. On a desktop there is room for both, so nothing folds.
   */
  const short =
    typeof window !== "undefined" &&
    window.matchMedia?.("(max-height: 720px)").matches === true;
  const [bannerOpen, setBannerOpen] = useState(!short);
  /** Appearance is app-local, so it is a destination rather than a category. */
  const [appearance, setAppearance] = useState(false);
  const [searchOpen, setSearchOpen] = useState(!short);

  /**
   * Training the wake word, not just typing it.
   *
   * The classic view offered this beside the phrase and the hub still does. It
   * has to live here too now — a box you can type a phrase into is not the same
   * capability as teaching the pond to hear it, and losing the second one while
   * keeping the first would look like the setting still worked.
   */
  const [calibrating, setCalibrating] = useState(false);

  const [locating, setLocating] = useState(false);
  const [locationNote, setLocationNote] = useState<string | null>(null);

  /**
   * Fill the place and both coordinates from the device.
   *
   * Staged on purpose rather than saved: this writes into the same draft every
   * other control writes into, so it appears in the change count and can be
   * abandoned. A detection that silently persisted would be the one control on
   * the page that acts before you press Save.
   *
   * One cascade, run on the server, shared with the wizard. This used to ask
   * `navigator.geolocation` directly — which a Tauri webview does not reliably
   * answer, so the button's usual outcome was a refusal and a name guessed
   * from the time zone, with no coordinates and therefore no weather.
   * `detectPlace` still offers this device's coordinates when a real browser
   * provides them; it just no longer depends on that.
   */
  const findLocation = useCallback(async () => {
    setLocating(true);
    setLocationNote(null);
    try {
      // What is already in the box beats anything derived, so a household that
      // typed "Kisumu" gets Kisumu's coordinates rather than the capital's.
      const typed = String(
        (settings as Record<string, unknown>).weather_location_name ?? "",
      ).trim();
      const at = await detectPlace(typed || undefined);

      // Staged, not saved: this writes into the same draft every other control
      // writes into, so it shows in the change count and can be abandoned.
      if (at.timezone) patch("timezone", at.timezone);
      if (at.name) patch("weather_location_name", at.name);
      if (at.has_coordinates) {
        patch("weather_latitude", at.latitude);
        patch("weather_longitude", at.longitude);
      }

      // Phrased by SOURCE, because "you are in Nairobi" and "your time zone
      // suggests Nairobi" are different claims and the old code stated the
      // guess as a fact.
      setLocationNote(
        at.note
          ? at.note
          : at.certain
            ? `Found ${at.name}`
            : `Guessed ${at.name} from your time zone`,
      );
    } catch (e) {
      setLocationNote(e instanceof Error ? e.message : String(e));
    } finally {
      setLocating(false);
    }
  }, [patch, settings]);

  async function save() {
    const body = diffSettings(baseline, settings);
    if (!Object.keys(body).length || saving || errorCount) return;
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await api.updateSettings(body);
      // Fold against the patch we SENT, not the pre-save baseline — the rule
      // the classic Settings panel follows. The server can answer with a value
      // it derived rather than the one we sent (geocoding rewrites the
      // coordinates), and that echo has to be adopted or every later save
      // re-sends a stale value forever.
      setSettings((prev) =>
        foldServerState(prev, { ...baseline, ...body }, updated),
      );
      setBaseline(structuredClone(updated));
      setSaved(true);
    } catch (e) {
      // Shown verbatim, not through `friendlyMessage`: a 422 from this endpoint
      // names the field and the accepted values, and that sentence is the whole
      // reason the request failed.
      setSaveError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  const searching = query.trim().length > 0;
  const category = CATALOGUE.find((c) => c.id === categoryId) ?? CATALOGUE[0];

  /**
   * Mirrors are not offered here. `pond-server` syncs the `active_*` keys from
   * `model_role_assignments` — the join table is the source of truth — so a
   * control on this page would lose to the next sync. The Models page owns them
   * and this page points at it.
   *
   * A setting nothing reads is also withheld, but only outside developer view.
   * It used to render disabled with a note explaining why; once the note moved
   * behind the developer flag that left a dead control and no reason for it,
   * which is worse than either half. So the household sees settings that do
   * something, and the marks live where the field names do.
   */
  const offered = useCallback(
    (es: Entry[]) =>
      es.filter(
        (e) => e.ownedBy === undefined && (dev || e.consumer !== "none"),
      ),
    [dev],
  );

  const groups: Subcategory[] = useMemo(() => {
    if (!searching)
      return category.groups
        .map((g) => ({ ...g, entries: offered(g.entries) }))
        .filter((g) => g.entries.length > 0);
    const q = query.trim().toLowerCase();
    return CATALOGUE.flatMap((c) => c.groups)
      .map((g) => ({
        ...g,
        // Descriptions are in the haystack, so a setting is findable by what it
        // does rather than only by what it is called. Field names join only in
        // developer view — matching on a string the household cannot see gives
        // a result they cannot explain.
        entries: offered(g.entries).filter((e) =>
          `${e.label} ${e.description} ${dev ? `${e.key} ${e.note ?? ""}` : ""}`
            .toLowerCase()
            .includes(q),
        ),
      }))
      .filter((g) => g.entries.length > 0);
  }, [searching, query, category, dev, offered]);

  const resultCount = groups.reduce((n, g) => n + g.entries.length, 0);

  const totals = useMemo(() => {
    const all = allEntries();
    return {
      total: all.length,
      /** What this page actually offers — mirrors are owned by Models. */
      offered: all.filter((e) => e.ownedBy === undefined).length,
      live: all.filter((e) => e.consumer === "live").length,
      app: all.filter((e) => e.consumer === "app").length,
      none: all.filter((e) => e.consumer === "none").length,
      proposed: all.filter((e) => e.proposed).length,
    };
  }, []);

  const goTo = useCallback((id: string) => {
    setCategoryId(id);
    setQuery("");
    setAppearance(false);
  }, []);

  const { clauses, reach, tail } = postureClauses(settings);
  const systemZone = deviceZone();
  const zoneDiffers = systemZone != null && settings.timezone !== systemZone;

  /** The controls with an action beside them. */
  function extraFor(entry: Entry): React.ReactNode {
    if (entry.key === "timezone") {
      if (!zoneDiffers) return undefined;
      return (
        <button
          type="button"
          className="scat__detect reach"
          onClick={() => patch("timezone", systemZone)}
        >
          <Crosshair size={12} aria-hidden="true" />
          Use {systemZone}
        </button>
      );
    }

    if (entry.key === "voice_wake_word") {
      const phrase = String(
        (settings as Record<string, unknown>).voice_wake_word ?? "",
      ).trim();
      return (
        <button
          type="button"
          className="scat__detect reach"
          onClick={() => setCalibrating(true)}
          disabled={!phrase}
          title={phrase ? undefined : "Type a wake phrase first"}
        >
          <Wand2 size={12} aria-hidden="true" />
          Train
        </button>
      );
    }

    // One press fills the name and both coordinates, because they are one
    // answer to one question and nobody thinks of them as three settings.
    if (entry.key === "weather_location_name") {
      return (
        <>
          <button
            type="button"
            className="scat__detect reach"
            onClick={findLocation}
            disabled={locating}
            aria-busy={locating || undefined}
          >
            <Crosshair size={12} aria-hidden="true" />
            {locating ? "Finding…" : "Detect"}
          </button>
          {locationNote && (
            <span className="scat__actionNote" role="status">
              {locationNote}
            </span>
          )}
        </>
      );
    }

    // Renaming on demand, rather than waiting for the pond to go quiet. Offered
    // whatever the toggle says, because the toggle governs what happens
    // unattended and this is not that.
    if (entry.key === "session_titling_enabled") {
      return (
        <>
          <button
            type="button"
            className="scat__detect reach"
            onClick={runRetitle}
            disabled={retitling}
            aria-busy={retitling || undefined}
          >
            <Wand2 size={12} aria-hidden="true" />
            {retitling ? "Asking…" : "Rename now"}
          </button>
          {retitleNote && (
            <span className="scat__actionNote" role="status">
              {retitleNote}
            </span>
          )}
        </>
      );
    }

    return undefined;
  }

  let lastTier: string | null = null;

  const saveLabel = saving
    ? "Saving…"
    : errorCount
      ? `Fix ${errorCount} field${errorCount === 1 ? "" : "s"}`
      : dirty
        ? `Save ${dirty} change${dirty === 1 ? "" : "s"}`
        : saved
          ? "Saved"
          : "No changes";

  return (
    <div className="scat">
      <header className="scat__head">
        <div>
          {onBack && (
            <button
              type="button"
              className="hub-back-btn scat__back"
              onClick={onBack}
            >
              ← Back
            </button>
          )}
          <h1 className="scat__title">Settings</h1>
          <p className="scat__sub">
            Everything this pond is, knows, hears, and is allowed to do.
          </p>
        </div>
        <div className="scat__headActions">
          <div className="scat__search" data-open={searchOpen || searching}>
            {/* Closed, the icon IS the control — one 44px target rather than a
                field that has shrunk to something nobody can hit. */}
            <button
              type="button"
              className="scat__searchBtn"
              aria-label={
                searchOpen || searching ? "Close search" : "Open search"
              }
              aria-expanded={searchOpen || searching}
              onClick={() => {
                if (searchOpen || searching) {
                  setQuery("");
                  setSearchOpen(false);
                } else {
                  setSearchOpen(true);
                  requestAnimationFrame(() =>
                    document.getElementById("scat-q")?.focus(),
                  );
                }
              }}
            >
              {searchOpen || searching ? (
                <X size={15} aria-hidden="true" />
              ) : (
                <Search size={15} aria-hidden="true" />
              )}
            </button>
            <input
              id="scat-q"
              type="search"
              className="scat__searchInput"
              aria-label="Search settings"
              placeholder={`Search ${totals.offered} settings`}
              value={query}
              tabIndex={searchOpen || searching ? 0 : -1}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  setQuery("");
                  setSearchOpen(false);
                }
              }}
            />
          </div>
          <button
            type="button"
            className="scat__dev"
            aria-pressed={dev}
            onClick={() => setDev((d) => !d)}
            title="Show field names, types and which settings are wired up"
          >
            <span className="scat__devDot" aria-hidden="true" />
            Developer view
          </button>
          <Button
            variant="primary"
            isDisabled={!dirty || saving || errorCount > 0}
            onPress={save}
          >
            {saveLabel}
          </Button>
        </div>
      </header>

      {calibrating && (
        <WakeWordCalibration
          phrase={String(
            (settings as Record<string, unknown>).voice_wake_word ?? "",
          )}
          onComplete={() => {
            setCalibrating(false);
            // Calibration writes the learned pronunciations server-side, so the
            // page has to re-read rather than assume. `foldServerState` keeps
            // any other edit in progress.
            void load();
          }}
          onCancel={() => setCalibrating(false)}
        />
      )}

      {dev && (
        <section className="scat__devbar" aria-label="Developer tools">
          <div>
            <b>Run setup again</b>
            <span>
              Clears the answers setup collected and reopens the wizard. Your
              settings are kept.
            </span>
          </div>
          {onboardErr && (
            <span className="scat__devbarErr" role="alert">
              {onboardErr}
            </span>
          )}
          <button
            type="button"
            className={`scat__devbarBtn${armed ? " scat__devbarBtn--armed" : ""}`}
            onClick={startOnboarding}
            disabled={onboarding}
          >
            {onboarding
              ? "Starting…"
              : armed
                ? "Yes, start setup"
                : "Start onboarding"}
          </button>
        </section>
      )}

      {loadError && <ErrorBanner error={loadError} onRetry={load} />}

      {saveError && (
        <p className="scat__saveError" role="alert">
          <AlertCircle size={14} aria-hidden="true" />
          <span>{saveError}</span>
        </p>
      )}

      {loading ? (
        <SkeletonList rows={8} />
      ) : (
        <>
          <section
            className="scat__posture"
            aria-labelledby="scat-now"
            data-open={bannerOpen}
          >
            <button
              type="button"
              className="scat__postureToggle"
              aria-expanded={bannerOpen}
              aria-controls="scat-posture-body"
              onClick={() => setBannerOpen((o) => !o)}
            >
              <span className="scat__eyebrow" id="scat-now">
                Right now
              </span>
              {/* Collapsed, the banner still has to answer the question it
                  exists to answer, so the reach clause comes with it. */}
              {!bannerOpen && (
                <span className="scat__postureGist">
                  {clauses.map((c) => c.text).join(" · ")}
                </span>
              )}
              <ChevronDown
                className="scat__postureChev"
                size={16}
                aria-hidden="true"
              />
            </button>
            <div
              className="scat__postureWrap"
              id="scat-posture-body"
              hidden={!bannerOpen}
            >
              <div className="scat__postureBody">
                <p className="scat__sentence">
                  Your pond{" "}
                  {/* Anchors, not buttons. The engine normalises `display: inline`
                    on a <button> to inline-block, which makes each clause an
                    atomic box — the comma after it then becomes its own
                    line-break opportunity and orphans onto the next line. An
                    <a> is inline natively, so punctuation stays with its
                    clause. These are in-page navigation, so a link is also the
                    honest element, and it is keyboard-reachable for free. */}
                  {clauses.map((c, i) => (
                    <Fragment key={c.category + c.text}>
                      <a
                        href={`#${c.category}`}
                        className="scat__clause"
                        onClick={(e) => {
                          e.preventDefault();
                          goTo(c.category);
                        }}
                      >
                        {c.text}
                      </a>
                      {i < clauses.length - 2
                        ? ", "
                        : i === clauses.length - 2
                          ? ", and "
                          : ". "}
                    </Fragment>
                  ))}
                  {reach ? (
                    <>
                      It can reach{" "}
                      <a
                        href={`#${reach.category}`}
                        className="scat__clause"
                        onClick={(e) => {
                          e.preventDefault();
                          goTo(reach.category);
                        }}
                      >
                        {reach.text}
                      </a>
                      .
                    </>
                  ) : (
                    tail
                  )}
                </p>
                <div className="scat__tally">
                  <span className="scat__tallyItem">
                    <span className="scat__dot scat__dot--live" />
                    <b>{totals.live}</b>
                    <span>connected</span>
                  </span>
                  <span className="scat__tallyItem">
                    <span className="scat__dot scat__dot--app" />
                    <b>{totals.app}</b>
                    <span>app only</span>
                  </span>
                  <span className="scat__tallyItem">
                    <span className="scat__dot scat__dot--none" />
                    <b>{totals.none}</b>
                    <span>not connected</span>
                  </span>
                  <span className="scat__tallyItem">
                    <span />
                    <b>{totals.proposed}</b>
                    <span>new control</span>
                  </span>
                </div>
              </div>
            </div>
          </section>

          <div className="scat__grid">
            <nav className="scat__rail" aria-label="Settings categories">
              {CATALOGUE.map((c: CatalogueCategory) => {
                const newTier = c.tier !== lastTier;
                if (newTier) lastTier = c.tier;
                const inert = inertCount(c);
                const bad = c.groups
                  .flatMap((g) => g.entries)
                  .filter((e) => errors[e.key]).length;
                return (
                  <div key={c.id} className="scat__railItem">
                    {newTier && (
                      <>
                        <div className="scat__tier">{c.tier}</div>
                        <p className="scat__tierNote">{TIER_NOTE[c.tier]}</p>
                      </>
                    )}
                    <button
                      type="button"
                      className="scat__navBtn"
                      aria-current={!searching && categoryId === c.id}
                      onClick={() => goTo(c.id)}
                    >
                      <span>{c.name}</span>
                      <span className="scat__navFlags">
                        {bad > 0 && (
                          <span
                            className="scat__flag scat__flag--bad"
                            title={`${bad} field${bad === 1 ? "" : "s"} to fix here`}
                          >
                            {bad}
                          </span>
                        )}
                        {inert > 0 && (
                          <span
                            className="scat__flag"
                            title={`${inert} setting${inert === 1 ? "" : "s"} here that nothing reads`}
                          >
                            {inert}
                          </span>
                        )}
                      </span>
                    </button>
                  </div>
                );
              })}
              <div className="scat__railItem">
                <div className="scat__tier">This app</div>
                <p className="scat__tierNote">
                  How it looks on this screen. The pond never sees it.
                </p>
                <button
                  type="button"
                  className="scat__navBtn"
                  aria-current={appearance}
                  onClick={() => {
                    setAppearance(true);
                    setQuery("");
                  }}
                >
                  <span>Appearance</span>
                </button>
              </div>
            </nav>

            {appearance ? (
              /* Rendered bare: it brings its own heading, and the pond has no
                 say in any of it. */
              <div className="scat__panel scat__panel--appearance">
                <AppearanceView />
              </div>
            ) : (
              <div className="scat__panel">
                <div className="scat__panelHead">
                  <h2 className="scat__panelTitle">
                    {searching
                      ? `${resultCount} result${resultCount === 1 ? "" : "s"}`
                      : category.name}
                  </h2>
                  <p className="scat__panelSub">
                    {searching
                      ? `Matching “${query.trim()}” across all ${totals.total} settings.`
                      : category.blurb}
                  </p>
                </div>

                {/* The watcher, inside Automations rather than floating above
                  the whole screen.

                  It is not a catalogue entry and cannot be one: every leaf of
                  `CATALOGUE` is exactly one `Settings` field, and two tests in
                  `catalogue.test.ts` enforce that in both directions. So it is
                  rendered here, in the panel body, wearing the same
                  `scat__group` container every real group wears -- which is
                  what makes it read as part of Automations rather than as a
                  third slab bolted on top.

                  Hidden while searching, because a search flattens every
                  category and this belongs to one. That also gives the poll its
                  visibility gate for free: selecting another category or typing
                  in the box unmounts the component and stops the interval,
                  with no `visibilitychange` plumbing needed on this surface. */}
                {!searching && category.id === "automation" && (
                  <section className="scat__group">
                    <div className="scat__groupHead">
                      <h3>Running now</h3>
                    </div>
                    <BackgroundJobs bare />
                  </section>
                )}

                {groups.length === 0 ? (
                  <div className="scat__group scat__empty">
                    <b>Nothing matches “{query.trim()}”</b>
                    <span>
                      Try a setting name, or part of a key like voice_.
                    </span>
                  </div>
                ) : (
                  groups.map((g) => (
                    <section
                      className="scat__group"
                      key={`${category.id}-${g.name}`}
                    >
                      <div className="scat__groupHead">
                        <h3>{g.name}</h3>
                        {g.name === "Where and when" && !searching && (
                          <p className="scat__groupHint">
                            Save a location name and the pond looks up its
                            coordinates for you. Typing a coordinate yourself
                            keeps the one you typed.
                          </p>
                        )}
                      </div>
                      {g.entries.map((e) => (
                        <EntryRow
                          key={e.key}
                          entry={e}
                          value={(settings as Record<string, unknown>)[e.key]}
                          error={errors[e.key] ?? null}
                          options={
                            e.control.kind === "lookup"
                              ? // Zones do not wait on the model registry: they come
                                // from a different call, and gating them on `models`
                                // would leave the zone picker as a text box on any
                                // pond with no models installed.
                                e.control.source === "time-zones"
                                ? zones.length
                                  ? optionsFor(e.control.source, [], zones)
                                  : null
                                : models
                                  ? optionsFor(
                                      e.control.source,
                                      models,
                                      zones,
                                      settings.mesh_enabled,
                                    )
                                  : null
                              : null
                          }
                          onChange={patch}
                          extra={extraFor(e)}
                          dev={dev}
                        />
                      ))}
                    </section>
                  ))
                )}
              </div>
            )}
          </div>

          <div className="scat__legend">
            <div className="scat__legendTitle">
              What the mark beside each setting means
            </div>
            <div className="scat__legendItem">
              <span className="scat__dot scat__dot--live" />
              <span>
                <b>Connected</b>The pond reads this and acts on it.
              </span>
            </div>
            <div className="scat__legendItem">
              <span className="scat__dot scat__dot--app" />
              <span>
                <b>App only</b>This app honours it. The pond never sees it.
              </span>
            </div>
            <div className="scat__legendItem">
              <span className="scat__dot scat__dot--none" />
              <span>
                <b>Not connected</b>Nothing reads this yet, so the control is
                not offered.
              </span>
            </div>
            <div className="scat__legendItem">
              <span className="scat__new">New</span>
              <span>
                <b>New control</b>Reachable only through the API before now.
              </span>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
