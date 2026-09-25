import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import {
  AlertTriangle, Check, Download, HardDrive, Loader2, Pause, Play,
  RefreshCw, Search, Trash2, X,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { useConfirm, ErrorBanner } from "../components/shared";
import { ApiError } from "../api/types";
import type {
  DiskUsage, DownloadEntry, HfModel, HfModelFile,
  ModelActiveRoles, ModelEntry, ModelMemoryStatus,
} from "../api/types";
import {
  ROLES, type RoleKey, roleHolder, formatSize, formatBytes, downloadPercent,
  isInFlight, fitReading, downloadedOnly, availableToDownload, rolesFor, modelLabel,
  groupByJob, modelFacts,
} from "./models/modelsView";
import { VoicePicker } from "../hub/views/settings/VoicePicker";
import { useVoicePreview } from "../voice/useVoicePreview";
import { clampPace, DEFAULT_VOICE, DEFAULT_PACE, DEFAULT_QUALITY } from "../voice/voiceCatalogue";
import "../styles/models.css";
import "../hub/views/settings/voice-picker.css";

// Fit meter: whether a model fits this device's model budget or spills to the CPU, from
// `models/memory-status`; "unknown" rather than a guess when no budget is reported (dev machines).

// ─── Roles band ────────────────────────────────────────────────────────────

function RoleCard({
  role, holder, onClear,
}: {
  role: (typeof ROLES)[number];
  holder: string | null;
  onClear: () => void;
}) {
  return (
    <article className="mdl-role" data-empty={holder ? undefined : "true"}>
      <span className="mdl-role__job">{role.label}</span>
      <span className="mdl-role__blurb">{role.blurb}</span>
      {holder ? (
        <>
          <span className="mdl-role__holder" title={holder}>{holder}</span>
          <button type="button" className="mdl-role__clear" onClick={onClear}>
            Change
          </button>
        </>
      ) : (
        <span className="mdl-role__none">
          <AlertTriangle size={14} aria-hidden="true" />
          Nothing assigned
        </span>
      )}
    </article>
  );
}

// ─── Downloads in flight ───────────────────────────────────────────────────

function DownloadRow({
  entry, busy, onControl,
}: {
  entry: DownloadEntry;
  busy: boolean;
  onControl: (action: "pause" | "resume" | "cancel") => void;
}) {
  const pct = downloadPercent(entry);
  const paused = entry.status === "paused";

  return (
    <div className="mdl-dl" data-paused={paused ? "true" : undefined}>
      <div className="mdl-dl__head">
        <span className="mdl-dl__name" title={entry.filename}>{entry.filename}</span>
        <span className="mdl-dl__pct">{pct == null ? "…" : `${pct}%`}</span>
      </div>

      <div className="mdl-dl__track" role="progressbar"
        aria-valuenow={pct ?? undefined} aria-valuemin={0} aria-valuemax={100}
        aria-label={`${entry.filename} download progress`}>
        {/* Indeterminate until the server reports a total — a bar pinned at
            zero reads as stalled, which is a different thing. */}
        <span className={pct == null ? "mdl-dl__fill mdl-dl__fill--unknown" : "mdl-dl__fill"}
          style={pct == null ? undefined : { width: `${pct}%` }} />
      </div>

      <div className="mdl-dl__foot">
        <span className="mdl-dl__bytes">
          {formatBytes(entry.downloaded_bytes)}
          {entry.total_bytes ? ` of ${formatBytes(entry.total_bytes)}` : ""}
          {paused ? " · paused" : ""}
        </span>
        <div className="mdl-dl__acts">
          <button type="button" className="mdl-btn mdl-btn--quiet"
            onClick={() => onControl(paused ? "resume" : "pause")} disabled={busy}>
            {paused ? <Play size={15} aria-hidden="true" /> : <Pause size={15} aria-hidden="true" />}
            <span>{paused ? "Resume" : "Pause"}</span>
          </button>
          <button type="button" className="mdl-btn mdl-btn--danger"
            onClick={() => onControl("cancel")} disabled={busy}>
            <X size={15} aria-hidden="true" />
            <span>Stop</span>
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── A model already on disk ───────────────────────────────────────────────

function ModelRow({
  model, memory, activeRoles, busy, onUse, onDelete,
}: {
  model: ModelEntry;
  memory: ModelMemoryStatus | null;
  activeRoles: ModelActiveRoles | null;
  busy: boolean;
  onUse: (role: RoleKey) => void;
  onDelete: () => void;
}) {
  const fit = fitReading(model, memory);
  const roles = rolesFor(model);
  const name = modelLabel(model);
  const facts = modelFacts(model);
  const inUse = roles.some((r) => roleHolder(activeRoles, r) === model.name);

  return (
    <div className="mdl-row" data-fit={fit.verdict} data-inuse={inUse ? "true" : undefined}>
      <div className="mdl-row__id">
        <div className="mdl-row__idline">
          <span className="mdl-row__name" title={name}>{name}</span>
          {inUse && (
            <span className="mdl-row__inuse">
              <Check size={13} aria-hidden="true" />
              In use
            </span>
          )}
        </div>
        {/* What the model file said about itself. For anything discovered on
            disk this is read from its GGUF header, which is the difference
            between a row that is a filename and one that tells you what you
            have. */}
        {facts.length > 0 && (
          <span className="mdl-row__facts">{facts.join(" · ")}</span>
        )}
      </div>

      <span className="mdl-row__size">
        {formatSize(model.size_mb ?? model.ram_estimate_mb) || "—"}
      </span>

      {/* The fit meter, inline. Its width is the share of what this device can
          give one model — the bar IS the claim, not decoration beside it. */}
      <div className="mdl-row__fit" title={fit.label}>
        <div className="mdl-fit__track">
          <span className="mdl-fit__fill" style={{ width: `${Math.min(100, fit.percent ?? 0)}%` }} />
        </div>
        <span className="mdl-row__fitpct">
          {fit.percent == null ? "—" : `${fit.percent}%`}
        </span>
      </div>

      <div className="mdl-row__acts">
        {roles.map((r) => (
          <button key={r} type="button" className="mdl-btn mdl-btn--primary"
            onClick={() => onUse(r)} disabled={busy || inUse}>
            {inUse ? "In use" : "Use"}
          </button>
        ))}
        <button type="button" className="mdl-btn mdl-btn--danger" onClick={onDelete} disabled={busy}
          aria-label={`Delete ${name}`}>
          <Trash2 size={15} aria-hidden="true" />
          <span>Delete</span>
        </button>
      </div>
    </div>
  );
}

// ─── A model the catalogue offers but this device does not have ────────────

function AvailableRow({
  model, memory, busy, onDownload,
}: {
  model: ModelEntry;
  memory: ModelMemoryStatus | null;
  busy: boolean;
  onDownload: () => void;
}) {
  const fit = fitReading(model, memory);
  const facts = modelFacts(model);
  const name = modelLabel(model);

  return (
    <div className="mdl-row" data-fit={fit.verdict}>
      <div className="mdl-row__id">
        <div className="mdl-row__idline">
          <span className="mdl-row__name" title={name}>{name}</span>
        </div>
        {facts.length > 0 && <span className="mdl-row__facts">{facts.join(" · ")}</span>}
      </div>

      <span className="mdl-row__size">{formatSize(model.size_mb) || "—"}</span>

      {/* The fit meter earns its keep most here: this is the moment before
          several gigabytes are spent, which is the only moment the answer can
          still change what you do. */}
      <div className="mdl-row__fit" title={fit.label}>
        <div className="mdl-fit__track">
          <span className="mdl-fit__fill" style={{ width: `${Math.min(100, fit.percent ?? 0)}%` }} />
        </div>
        <span className="mdl-row__fitpct">{fit.percent == null ? "—" : `${fit.percent}%`}</span>
      </div>

      <div className="mdl-row__acts">
        <button type="button" className="mdl-btn mdl-btn--primary" onClick={onDownload}
          disabled={busy} aria-label={`Download ${name}`}>
          <Download size={15} aria-hidden="true" />
          <span>Download</span>
        </button>
      </div>
    </div>
  );
}

// ─── Add from Hugging Face ─────────────────────────────────────────────────

function AddBand({ onStarted }: { onStarted: () => void }) {
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [results, setResults] = useState<HfModel[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [openRepo, setOpenRepo] = useState<string | null>(null);
  const [files, setFiles] = useState<Record<string, HfModelFile[]>>({});
  const [loadingFiles, setLoadingFiles] = useState<string | null>(null);
  const [starting, setStarting] = useState<string | null>(null);

  const search = useCallback(async () => {
    const q = query.trim();
    if (!q) return;
    setSearching(true); setError(null); setOpenRepo(null);
    try { setResults((await api.searchGgufModels(q)).models ?? []); }
    catch (e) { setError(e instanceof Error ? e.message : String(e)); setResults(null); }
    finally { setSearching(false); }
  }, [query]);

  async function openFiles(repoId: string) {
    if (openRepo === repoId) { setOpenRepo(null); return; }
    setOpenRepo(repoId);
    if (files[repoId]) return;
    setLoadingFiles(repoId);
    try {
      // Await outside the updater: an async setState callback would store a promise as the list.
      const list = (await api.listHfModelFiles(repoId)).files ?? [];
      setFiles((f) => ({ ...f, [repoId]: list }));
    } catch {
      setFiles((f) => ({ ...f, [repoId]: [] }));
    } finally {
      setLoadingFiles(null);
    }
  }

  async function download(file: HfModelFile) {
    setStarting(file.filename);
    try {
      await api.downloadModelFromUrl(file.url, "gguf", file.filename);
      onStarted();
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setStarting(null); }
  }

  return (
    <section className="mdl-band">
      <h2 className="mdl-band__title">Add a model</h2>
      <p className="mdl-band__sub">Search Hugging Face. Downloads land on this device and nowhere else.</p>

      <form className="mdl-search" onSubmit={(e) => { e.preventDefault(); void search(); }}>
        <Search size={16} aria-hidden="true" />
        <input className="mdl-search__input" type="search" value={query}
          aria-label="Search Hugging Face for a model"
          placeholder="gemma, qwen, whisper…"
          onChange={(e) => setQuery(e.target.value)} />
        <button type="submit" className="mdl-btn mdl-btn--primary" disabled={searching || !query.trim()}>
          {searching ? <Loader2 size={15} className="mdl-spin" aria-hidden="true" /> : null}
          <span>{searching ? "Searching…" : "Search"}</span>
        </button>
      </form>

      {error && <ErrorBanner error={error} />}

      {results !== null && results.length === 0 && !searching && (
        <p className="mdl-empty">Nothing on Hugging Face matches “{query.trim()}”.</p>
      )}

      {results !== null && results.length > 0 && (
        <ul className="mdl-results">
          {results.map((r) => (
            <li key={r.id} className="mdl-result">
              <button type="button" className="mdl-result__head"
                onClick={() => void openFiles(r.id)} aria-expanded={openRepo === r.id}>
                <span className="mdl-result__name">{r.id}</span>
                <span className="mdl-result__meta">
                  {typeof r.downloads === "number" ? `${r.downloads.toLocaleString()} downloads` : ""}
                </span>
              </button>

              {openRepo === r.id && (
                <div className="mdl-result__files">
                  {loadingFiles === r.id && <span className="mdl-muted">Reading the file list…</span>}
                  {loadingFiles !== r.id && (files[r.id]?.length ?? 0) === 0 && (
                    <span className="mdl-muted">No GGUF files in this repository.</span>
                  )}
                  {files[r.id]?.map((f) => (
                    <div key={f.filename} className="mdl-file">
                      <span className="mdl-file__name" title={f.filename}>{f.filename}</span>
                      <span className="mdl-file__size">{formatSize(f.size_mb)}</span>
                      <button type="button" className="mdl-btn mdl-btn--primary"
                        onClick={() => void download(f)} disabled={starting === f.filename}>
                        <Download size={15} aria-hidden="true" />
                        <span>{starting === f.filename ? "Starting…" : "Download"}</span>
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

// ─── The page ──────────────────────────────────────────────────────────────

export function Models() {
  const confirm = useConfirm();

  const [roles, setRoles] = useState<ModelActiveRoles | null>(null);
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [modelsLoading, setModelsLoading] = useState(true);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [downloads, setDownloads] = useState<DownloadEntry[]>([]);
  const [memory, setMemory] = useState<ModelMemoryStatus | null>(null);
  const [disk, setDisk] = useState<DiskUsage | null>(null);
  const [busy, setBusy] = useState(false);
  const [flash, setFlash] = useState<{ text: string; ok: boolean } | null>(null);
  const pollRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const say = useCallback((text: string, ok = true) => {
    setFlash({ text, ok });
    setTimeout(() => setFlash(null), 3500);
  }, []);

  const loadRoles = useCallback(async () => {
    try { setRoles(await api.getActiveRoles()); } catch { /* non-fatal */ }
  }, []);

  const loadModels = useCallback(async () => {
    setModelsLoading(true); setModelsError(null);
    try { setModels(await api.listModels()); }
    catch (e) { setModelsError(e instanceof Error ? e.message : String(e)); }
    finally { setModelsLoading(false); }
  }, []);

  const loadDownloads = useCallback(async () => {
    try { setDownloads((await api.getDownloadProgress()).downloads ?? []); }
    catch { /* non-fatal */ }
  }, []);

  /** Poll only while something is actually moving. */
  const pollDownloads = useCallback(() => {
    if (pollRef.current) return;
    const tick = async () => {
      pollRef.current = null;
      const list = await api.getDownloadProgress()
        .then((r) => r.downloads ?? [])
        .catch(() => [] as DownloadEntry[]);
      setDownloads(list);
      if (list.some((d) => d.status === "downloading")) {
        pollRef.current = setTimeout(tick, 1500);
      } else {
        // A transfer ended or paused: the model list and disk figure may have moved.
        void loadModels();
        void api.getDiskUsage().then(setDisk).catch(() => {});
      }
    };
    pollRef.current = setTimeout(tick, 1500);
  }, [loadModels]);

  useEffect(() => {
    void loadRoles();
    void loadModels();
    void loadDownloads();
    api.getMemoryStatus().then(setMemory).catch(() => {});
    api.getDiskUsage().then(setDisk).catch(() => {});
    return () => { if (pollRef.current) clearTimeout(pollRef.current); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Resume polling across a remount while a transfer is still going.
  useEffect(() => {
    if (downloads.some((d) => d.status === "downloading")) pollDownloads();
  }, [downloads, pollDownloads]);

  // ── Voice, for the Speaking group ──
  // Choosing a voice is not managing a model file, so Speaking gets a picker, not a row list.
  const preview = useVoicePreview();
  const [voice, setVoice] = useState<string>(DEFAULT_VOICE);
  const [pace, setPace] = useState<number>(DEFAULT_PACE);
  const [quality, setQuality] = useState<string>(DEFAULT_QUALITY);
  /** True while the engine is being fetched/reconfigured, so the UI can say so. */
  const [applying, setApplying] = useState(false);
  const paceTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    api
      .getSettings()
      .then((s) => {
        if (!s || typeof s !== "object") return;
        if (s.voice_tts_voice) setVoice(s.voice_tts_voice);
        if (typeof s.voice_tts_speed === "number") setPace(clampPace(s.voice_tts_speed));
        if (s.voice_tts_quality) setQuality(s.voice_tts_quality);
      })
      .catch(() => {
        /* offline: the picker still works against whatever is on disk */
      });
    return () => { if (paceTimer.current) clearTimeout(paceTimer.current); };
  }, []);

  async function chooseVoice(id: string) {
    const previous = voice;
    setVoice(id);
    setApplying(true);
    try {
      await api.updateSettings({ voice_tts_voice: id });
      // Apply to the running engine (fetching the voice if new); otherwise it waits for a restart.
      pollDownloads();
      await api.applyTtsSettings({ voice: id });
      void loadModels();
      // Speak straight away: a list of names is a guess until you hear it.
      void preview.play();
    } catch {
      setVoice(previous);
    } finally {
      setApplying(false);
    }
  }

  // Debounced: a slider drag emits a value per pixel, each a settings write and a synthesis.
  function choosePace(next: number) {
    const value = clampPace(next);
    setPace(value);
    if (paceTimer.current) clearTimeout(paceTimer.current);
    paceTimer.current = setTimeout(() => {
      api
        .updateSettings({ voice_tts_speed: value })
        .then(() => api.applyTtsSettings({ speed: value }))
        .then(() => preview.play())
        .catch(() => {});
    }, 500);
  }

  async function chooseQuality(value: string) {
    const previous = quality;
    setQuality(value);
    setApplying(true);
    try {
      await api.updateSettings({ voice_tts_quality: value });
      // Poll before the apply: the fetch happens inside it.
      pollDownloads();
      // Fetches a new tier, then drops the session so the next utterance loads it.
      await api.applyTtsSettings({ quality: value });
    } catch {
      setQuality(previous);
    } finally {
      setApplying(false);
    }
  }

  const onDisk = useMemo(() => downloadedOnly(models), [models]);
  const groups = useMemo(() => groupByJob(onDisk), [onDisk]);

  // Every catalogue voice, installed or not: each is a ~522 KB style table fetched on selection.
  const allVoices = useMemo(
    () => models.filter((m) => (m.category ?? m.provider) === "tts_kokoro").map((m) => m.name),
    [models],
  );
  const installedVoices = useMemo(
    () => new Set(onDisk.filter((m) => (m.category ?? m.provider) === "tts_kokoro").map((m) => m.name)),
    [onDisk],
  );
  // No voices in "Ready to download": the picker offers and fetches every one.
  const availableGroups = useMemo(
    () => groupByJob(availableToDownload(models)).filter((g) => g.key !== "tts"),
    [models],
  );
  // "Coming down" excludes voice fetches; the picker shows those beside the voice.
  const inFlight = useMemo(
    () => downloads.filter(isInFlight).filter((d) => d.category !== "tts_kokoro"),
    [downloads],
  );
  const voiceInFlight = useMemo(
    () => downloads.filter(isInFlight).filter((d) => d.category === "tts_kokoro"),
    [downloads],
  );

  // Voice/engine fetches for the picker's progress row, from the same feed as `inFlight`.
  const voiceTransfers = useMemo(
    () =>
      voiceInFlight
        .map((d) => ({
          filename: d.filename,
          downloaded: d.downloaded_bytes ?? 0,
          total: d.total_bytes ?? null,
        })),
    [voiceInFlight],
  );

  async function useFor(model: ModelEntry, role: RoleKey) {
    setBusy(true);
    try {
      // `category` (e.g. "tts_kokoro"), not `provider`: the server builds the lookup id from it,
      // and `provider` is only the list endpoint's group key ("tts").
      await api.activateModel(model.category ?? model.provider, model.name, role);
      await loadRoles();
      say(`${modelLabel(model)} now handles ${ROLES.find((r) => r.key === role)?.label.toLowerCase()}.`);
    } catch (e) { say(e instanceof Error ? e.message : String(e), false); }
    finally { setBusy(false); }
  }

  async function remove(model: ModelEntry) {
    const ok = await confirm(
      `Delete “${modelLabel(model)}”? This removes ${formatSize(model.size_mb) || "the file"} from this device.`,
      { title: "Delete model", confirmLabel: "Delete", destructive: true },
    );
    if (!ok) return;
    setBusy(true);
    try {
      await api.deleteModel(model.category ?? model.provider, model.name);
      await loadModels();
      void api.getDiskUsage().then(setDisk).catch(() => {});
      say(`${modelLabel(model)} deleted.`);
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        say(`${modelLabel(model)} is doing a job right now. Give that job to another model first.`, false);
      } else {
        say(e instanceof Error ? e.message : String(e), false);
      }
    } finally { setBusy(false); }
  }

  /** Start a download; for TTS voices the server also fetches the companion .json. */
  async function fetchModel(model: ModelEntry) {
    setBusy(true);
    try {
      await api.downloadModel(model.category ?? model.provider, model.name);
      say(`${modelLabel(model)} is downloading.`);
      await loadDownloads();
      pollDownloads();
    } catch (e) { say(e instanceof Error ? e.message : String(e), false); }
    finally { setBusy(false); }
  }

  async function control(entry: DownloadEntry, action: "pause" | "resume" | "cancel") {
    setBusy(true);
    try {
      await api.controlDownload(entry.filename, action);
      await loadDownloads();
      if (action === "resume") pollDownloads();
    } catch (e) { say(e instanceof Error ? e.message : String(e), false); }
    finally { setBusy(false); }
  }

  const budget = memory ? formatSize(memory.available_for_llm_mb) : null;
  const used = disk ? formatBytes(disk.total_bytes) : null;

  return (
    <div className="mdl">
      <header className="mdl-head">
        <div>
          <h1 className="mdl-head__title">Models</h1>
          <p className="mdl-head__sub">What this pond runs on, and what it costs.</p>
        </div>
        {/* Live, from the pond: the two numbers that decide every choice below. */}
        <div className="mdl-head__stats">
          <span className="mdl-stat">
            <HardDrive size={15} aria-hidden="true" />
            <span className="mdl-stat__num">{used ?? "—"}</span>
            <span className="mdl-stat__of">on disk</span>
          </span>
          <span className="mdl-stat">
            <span className="mdl-stat__num">{budget ?? "—"}</span>
            <span className="mdl-stat__of">for models</span>
          </span>
          {/* Re-reads the memory budget too, not just the lists. Left out, a
              memory read that failed at mount — the server is often still
              starting when this page first loads — left every fit meter stuck
              on "unknown" with no way back short of leaving the section. */}
          <button type="button" className="mdl-btn mdl-btn--quiet" onClick={() => {
            void loadModels();
            void loadRoles();
            void loadDownloads();
            api.getMemoryStatus().then(setMemory).catch(() => {});
            api.getDiskUsage().then(setDisk).catch(() => {});
          }}>
            <RefreshCw size={15} aria-hidden="true" />
            <span>Refresh</span>
          </button>
        </div>
      </header>

      {flash && (
        <p className={flash.ok ? "mdl-flash" : "mdl-flash mdl-flash--bad"} role="status">
          {flash.text}
        </p>
      )}

      <section className="mdl-band">
        <h2 className="mdl-band__title">Jobs</h2>
        <p className="mdl-band__sub">Which model does what. This is the only part most ponds ever change.</p>
        <div className="mdl-roles">
          {ROLES.map((role) => (
            <RoleCard key={role.key} role={role} holder={roleHolder(roles, role.key)}
              onClear={() => say(`Pick a model below and choose “Use for ${role.label.toLowerCase()}”.`)} />
          ))}
        </div>
      </section>

      {inFlight.length > 0 && (
        <section className="mdl-band">
          <h2 className="mdl-band__title">Coming down</h2>
          <div className="mdl-dls">
            {inFlight.map((d) => (
              <DownloadRow key={d.filename} entry={d} busy={busy}
                onControl={(a) => void control(d, a)} />
            ))}
          </div>
        </section>
      )}

      <section className="mdl-band">
        <h2 className="mdl-band__title">On this device</h2>
        {modelsError && <ErrorBanner error={modelsError} onRetry={() => void loadModels()} />}
        {modelsLoading && <p className="mdl-muted">Reading the catalogue…</p>}
        {!modelsLoading && !modelsError && onDisk.length === 0 && (
          <p className="mdl-empty">Nothing downloaded yet. Search below to add one.</p>
        )}
        {/* Grouped under the job each one can do, using the same four words as
            the Jobs band above — so "Listening" means one thing on this page
            rather than "ASR" at the top and "Whisper" further down.

            One ink card per group rather than per model: fourteen of the pond's
            loudest treatment is a wall of purple, and the grouping is the
            structure worth drawing. */}
        {groups.map((g) => (
          <section key={g.key} className="mdl-group">
            <header className="mdl-group__head">
              <h3 className="mdl-group__title">{g.label}</h3>
              <span className="mdl-group__count">
                {g.key === "tts"
                  ? // The picker lists every voice, so count installed out of all.
                    `${installedVoices.size} of ${allVoices.length} voices installed`
                  : `${g.models.length} ${g.models.length === 1 ? "model" : "models"}`}
              </span>
            </header>
            {g.key === "tts" ? (
              <VoicePicker
                voices={allVoices.length ? allVoices : g.models.map((m) => m.name)}
                installed={installedVoices}
                applying={applying}
                transfers={voiceTransfers}
                selected={voice}
                onSelect={(id) => void chooseVoice(id)}
                pace={pace}
                onPaceChange={choosePace}
                preview={preview}
                loading={modelsLoading}
                quality={quality}
                onQualityChange={(v) => void chooseQuality(v)}
                availableMb={memory?.available_for_llm_mb ?? null}
              />
            ) : (
              <div className="mdl-group__rows">
                {g.models.map((m) => (
                  <ModelRow key={`${m.provider}/${m.name}`} model={m} memory={memory}
                    activeRoles={roles} busy={busy}
                    onUse={(role) => void useFor(m, role)} onDelete={() => void remove(m)} />
                ))}
              </div>
            )}
          </section>
        ))}
      </section>

      {/* The catalogue's own offerings, grouped the same way. This is where a
          second voice or a better transcriber comes from: whisper builds and
          piper voices ship with download URLs already attached, and the page
          used to drop every one of them by showing only what was downloaded. */}
      {availableGroups.length > 0 && (
        <section className="mdl-band">
          <h2 className="mdl-band__title">Ready to download</h2>
          <p className="mdl-band__sub">
            Known to this pond and not here yet. Voices are chosen above.
          </p>
          {availableGroups.map((g) => (
            <section key={g.key} className="mdl-group">
              <header className="mdl-group__head">
                <h3 className="mdl-group__title">{g.label}</h3>
                <span className="mdl-group__count">
                  {g.models.length} {g.models.length === 1 ? "model" : "models"}
                </span>
              </header>
              <div className="mdl-group__rows">
                {g.models.map((m) => (
                  <AvailableRow key={`${m.provider}/${m.name}`} model={m} memory={memory}
                    busy={busy} onDownload={() => void fetchModel(m)} />
                ))}
              </div>
            </section>
          ))}
        </section>
      )}

      <AddBand onStarted={() => { void loadDownloads(); pollDownloads(); }} />
    </div>
  );
}
