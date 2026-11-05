import { useState, useEffect, useCallback, useMemo, useRef, type ReactNode } from "react";
import { AlertTriangle, HardDrive, RefreshCw } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useConfirm, ErrorBanner } from "../components/shared";
import type { ModelEngine, ModelEntry } from "../api/types";
import { useModels } from "../hooks/useModels";
import { useModelActions } from "../hooks/useModelActions";
import { EngineMark } from "../components/models/ModelMarks";
import { ENGINE_GROUPS, engineOf, groupByEngine } from "../lib/modelProvider";
import {
  ROLES, type RoleKey, roleHolder, emptyJobText, formatSize, formatBytes, budgetReading,
  downloadedOnly, rolesFor, modelLabel, groupBySection, isInUse, holderEntry, recommendedPicks,
  raisedPick, askingPick, sortModels, carriesPictures, picturesOf, sectionOfRole, usesOf, helpersOf,
} from "./models/modelsView";
import { isInFlight } from "./models/modelDownloads";
import { HelperRow, ModelRow } from "./models/ModelRow";
import { HolderBadge, MemoryModels } from "../components/models/MemoryModels";
import { jobState } from "./models/memoryEmbedder";
import { useMemoryJob } from "../hooks/useMemoryJob";
import { PickCard } from "./models/PickCard";
import { AddBand } from "./models/AddBand";
import { VoicePicker } from "../hub/views/settings/VoicePicker";
import { useVoicePreview } from "../voice/useVoicePreview";
import { clampPace, DEFAULT_VOICE, DEFAULT_PACE, DEFAULT_QUALITY } from "../voice/voiceCatalogue";
import "../styles/models.css";
import "../hub/views/settings/voice-picker.css";

// ─── Jobs band ─────────────────────────────────────────────────────────────

function RoleCard({
  role, holder, state, note, emptyText, onChange,
}: {
  role: (typeof ROLES)[number];
  holder: { title: string; engine: ModelEngine | null } | null;
  /** Where the holder is when it is not running yet, e.g. a Memory model that did not load. */
  state?: ReactNode;
  /** A quiet line under the holder, e.g. that nobody chose it. */
  note?: string | null;
  /** What an unnamed job reads when that is not a gap, e.g. memory's built-in model. */
  emptyText: string | null;
  /** Null when no list below can fill the job, so there is nothing to press. */
  onChange: (() => void) | null;
}) {
  return (
    <article className="mdl-role" data-empty={holder || emptyText ? undefined : "true"}>
      <span className="mdl-role__job">{role.label}</span>
      <span className="mdl-role__blurb">{role.blurb}</span>
      {holder ? (
        <>
          <span className="mdl-role__holder" title={holder.title}>{holder.title}</span>
          {holder.engine && (
            <EngineMark engine={holder.engine} label={holder.engine.label} format={holder.engine.file_format} />
          )}
          {state && <span className="mdl-role__state">{state}</span>}
          {note && <span className="mdl-role__built">{note}</span>}
        </>
      ) : emptyText ? (
        <span className="mdl-role__built">{emptyText}</span>
      ) : (
        <span className="mdl-role__none">
          <AlertTriangle size={14} aria-hidden="true" />
          Nothing chosen yet
        </span>
      )}
      {onChange && (
        <button type="button" className="mm-btn mdl-role__change" onClick={onChange}
          aria-label={`${holder ? "Change" : "Choose"} the model for ${role.label.toLowerCase()}`}>
          {holder ? "Change" : "Choose"}
        </button>
      )}
    </article>
  );
}

// ─── An engine's rows ──────────────────────────────────────────────────────

function EngineHead({ group }: { group: (typeof ENGINE_GROUPS)[number] }) {
  return (
    <header className="mdl-engine">
      <EngineMark engine={{ id: group.key }} label={group.label} format={group.format} />
      <p className="mdl-engine__blurb">{group.blurb}</p>
    </header>
  );
}

/** Scrolls to a band, gently unless the person has asked for no motion. */
function jumpTo(id: string): boolean {
  const el = document.getElementById(id);
  if (!el) return false;
  const calm = typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  el.scrollIntoView?.({ behavior: calm ? "auto" : "smooth", block: "start" });
  return true;
}

// ─── The page ──────────────────────────────────────────────────────────────

export function Models() {
  const confirm = useConfirm();
  const data = useModels({ disk: true });
  const { models, roles, memory, downloads, disk, loading, error, transferFor } = data;

  const [flash, setFlash] = useState<{ text: string; ok: boolean } | null>(null);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const say = useCallback((text: string, ok = true) => {
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlash({ text, ok });
    flashTimer.current = setTimeout(() => setFlash(null), ok ? 4500 : 7000);
  }, []);

  useEffect(() => () => { if (flashTimer.current) clearTimeout(flashTimer.current); }, []);

  const actions = useModelActions(data, say);
  const { busy, withPictures } = actions;

  const job = useMemoryJob(data, actions, say);

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
      data.watchDownloads();
      await api.applyTtsSettings({ voice: id });
      void data.reloadModels();
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
      // Watch before the apply: the fetch happens inside it.
      data.watchDownloads();
      // Fetches a new tier, then drops the session so the next utterance loads it.
      await api.applyTtsSettings({ quality: value });
    } catch {
      setQuality(previous);
    } finally {
      setApplying(false);
    }
  }

  // ── What each band lists ──
  const onDisk = useMemo(() => downloadedOnly(models), [models]);
  const conversation = useMemo(() => models.filter((m) => rolesFor(m).includes("chat")), [models]);
  const picks = useMemo(() => recommendedPicks(models), [models]);
  const pickIds = useMemo(() => new Set(picks.map((m) => m.id)), [picks]);

  const helpers = useMemo(() => roles?.helpers ?? [], [roles]);
  const groups = useMemo(() => groupBySection(models, helpers, roles), [models, helpers, roles]);
  // Memory has its own panel under Jobs, which says which model is in use and offers the rest, so
  // the device list leaves its section out rather than list the memory model twice.
  const deviceGroups = useMemo(() => groups.filter((g) => g.key !== "memory"), [groups]);
  const comingDown = useCallback((m: ModelEntry) => transferFor(m) !== null, [transferFor]);

  /** Rows to fetch: not here, not already a pick above, and with somewhere to fetch them from. */
  const getMoreConversation = useMemo(
    () =>
      sortModels(
        conversation.filter(
          (m) =>
            !m.downloaded &&
            !pickIds.has(m.id) &&
            (comingDown(m) || (m.acquire !== "external" && m.acquire !== "unavailable")),
        ),
      ),
    [conversation, pickIds, comingDown],
  );
  const getMoreListening = useMemo(
    () => models.filter((m) => !m.downloaded && rolesFor(m).includes("asr")),
    [models],
  );

  // Every catalogue voice, installed or not: each is a ~522 KB style table fetched on selection.
  const allVoices = useMemo(
    () => models.filter((m) => (m.category ?? m.provider) === "tts_kokoro").map((m) => m.name),
    [models],
  );
  const installedVoices = useMemo(
    () => new Set(onDisk.filter((m) => (m.category ?? m.provider) === "tts_kokoro").map((m) => m.name)),
    [onDisk],
  );

  // Voice and engine fetches belong to the picker, which shows them beside the voice.
  const voiceTransfers = useMemo(
    () =>
      downloads
        .filter((d) => isInFlight(d) && d.category === "tts_kokoro")
        .map((d) => ({
          filename: d.filename,
          downloaded: d.downloaded_bytes ?? 0,
          total: d.total_bytes ?? null,
        })),
    [downloads],
  );

  // ── Actions ──
  async function remove(model: ModelEntry) {
    const freed = formatSize(model.size_mb);
    const size = freed || "the file";
    // The encoder file is shared per family, so its bytes return only if no other model uses it.
    const pictures = picturesOf(model);
    const removes =
      pictures?.state === "installed"
        ? `This removes ${size}, plus ${formatBytes(pictures.size_bytes)} of picture support if no other model uses it.`
        : `This removes ${size} from this device.`;
    const ok = await confirm(
      `Delete “${modelLabel(model)}”? ${removes}`,
      { title: "Delete model", confirmLabel: "Delete", destructive: true },
    );
    if (!ok) return;
    actions.setBusy(true);
    try {
      await api.deleteModel(model.category ?? model.provider, model.name);
      await data.reload();
      say(`Deleted ${modelLabel(model)}.${freed ? ` ${freed} freed.` : ""}`);
    } catch (e) {
      // The pond refuses a model in use, or one whose file another model uses, in its own words.
      say(e instanceof Error ? e.message : String(e), false);
    } finally { actions.setBusy(false); }
  }

  function change(role: RoleKey) {
    const label = ROLES.find((r) => r.key === role)?.label.toLowerCase();
    if (!jumpTo(role === "embedding" ? "mdl-memory" : `mdl-section-${sectionOfRole(role)}`)) {
      jumpTo(role === "chat" ? "mdl-recommended" : "mdl-get");
    }
    say(role === "tts" ? "Pick a voice under Speaking." : `Press Use on the model you want for ${label}.`);
  }

  const row = (m: ModelEntry) => {
    const transfer = transferFor(m);
    return (
      <ModelRow key={m.id} model={m} memory={memory} inUse={isInUse(m, roles)} roles={rolesFor(m)}
        uses={usesOf(m, roles)} helpers={helpersOf(m, helpers)}
        transfer={transfer} withPictures={withPictures(m)} busy={busy}
        onWithPictures={(next) => actions.setWithPictures(m, next)}
        onUse={(role) => void actions.useFor(m, role)} onDelete={() => void remove(m)}
        onDownload={() => void actions.download(m)} onAddPictures={() => void actions.addPictures(m)}
        onControl={(action) => transfer && void actions.control(m, transfer, action)} />
    );
  };

  const budget = budgetReading(memory);
  const used = disk ? formatBytes(disk.total_bytes) : "—";
  const raised = raisedPick(picks, roles);
  const asking = askingPick(picks, roles, comingDown);

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
            <span className="mdl-stat__num">{used}</span>
            <span className="mdl-stat__of">on disk</span>
          </span>
          <span className="mdl-stat" title={budget.label}>
            <span className="mdl-stat__num">{budget.text}</span>
            <span className="mdl-stat__of">for one model</span>
          </span>
          <button type="button" className="mm-btn mdl-head__refresh" onClick={() => void data.reload()}>
            <RefreshCw size={15} aria-hidden="true" />
            <span>Refresh</span>
          </button>
        </div>
      </header>

      {flash && (
        <p className={flash.ok ? "mdl-flash" : "mdl-flash mdl-flash--bad"} role={flash.ok ? "status" : "alert"}>
          {flash.text}
        </p>
      )}

      <section className="mdl-band" aria-labelledby="mdl-jobs">
        <h2 className="mdl-band__title" id="mdl-jobs">Jobs</h2>
        <p className="mdl-band__sub">Which model does what. This is the only part most ponds ever change.</p>
        <div className="mdl-roles">
          {ROLES.map((role) => {
            const memoryHeld = role.key === "embedding" ? job.holder : null;
            const memoryState = memoryHeld ? jobState(job.line, { checking: job.checking }) : null;
            const entry = role.key === "embedding" ? job.holder?.entry ?? null : holderEntry(models, roles, role.key);
            const name = role.key === "embedding" ? job.holder?.name ?? null : roleHolder(roles, role.key);
            // Something below must be able to fill the job, or there is nothing to press.
            const canChoose =
              role.key === "chat" ||
              role.key === "tts" ||
              (role.key === "embedding" && job.rows.length > 0) ||
              groups.some((g) => g.key === sectionOfRole(role.key)) ||
              (role.key === "asr" && getMoreListening.length > 0);
            return (
              <RoleCard key={role.key} role={role}
                holder={name ? { title: entry ? modelLabel(entry) : name, engine: entry ? engineOf(entry) : null } : null}
                state={memoryState ? <HolderBadge badge={memoryState} /> : null}
                note={memoryHeld?.builtIn ? "Built-in default" : null}
                emptyText={emptyJobText(roles, role.key)}
                onChange={canChoose ? () => change(role.key) : null} />
            );
          })}
        </div>

        {job.rows.length > 0 && (
          <section id="mdl-memory" className="mdl-group" aria-label="Memory models">
            <header className="mdl-group__head">
              <h3 className="mdl-group__title">Memory models</h3>
              <span className="mdl-group__count">{job.rows.length} models</span>
            </header>
            <div className="mdl-group__body">
              <MemoryModels models={models} roles={roles} transferFor={transferFor}
                line={job.line} checking={job.checking} retrying={job.retrying} busy={busy} off={job.off}
                offline={job.offline} room={memory?.embedder_room_mb ?? null}
                onTurnOn={() => void job.turnOn()} onUse={(m) => void job.useModel(m)}
                onControl={(m, transfer, action) => void actions.control(m, transfer, action)}
                onRetry={() => void job.retry()} />
            </div>
          </section>
        )}
      </section>

      {/* After the jobs, GIAP's picks: what a pond changes most is which model does what, then what
          to fetch next. What is on this device follows. */}
      {picks.length > 0 && (
        <section className="mdl-band" id="mdl-recommended" aria-labelledby="mdl-recommended-title">
          <h2 className="mdl-band__title" id="mdl-recommended-title">Recommended for this pond</h2>
          <p className="mdl-band__sub">
            GIAP's picks. Nothing is downloaded or switched on until you choose it.
          </p>
          <div className="mm-picks">
            {picks.map((m) => {
              const transfer = transferFor(m);
              return (
                <PickCard key={m.id} model={m} memory={memory} inUse={isInUse(m, roles)}
                  raised={raised?.id === m.id} asking={asking?.id === m.id}
                  transfer={transfer} withPictures={withPictures(m)} busy={busy}
                  onWithPictures={(next) => actions.setWithPictures(m, next)}
                  onUse={() => void actions.useFor(m, "chat")} onDownload={() => void actions.download(m)}
                  onControl={(action) => transfer && void actions.control(m, transfer, action)} />
              );
            })}
          </div>
        </section>
      )}

      <section className="mdl-band" id="mdl-device" aria-labelledby="mdl-device-title">
        <h2 className="mdl-band__title" id="mdl-device-title">On this device</h2>
        {error && <ErrorBanner error={error} onRetry={() => void data.reloadModels()} />}
        {loading && <p className="mdl-muted">Reading the catalogue…</p>}
        {!loading && !error && deviceGroups.length === 0 && (
          <p className="mdl-empty">Nothing downloaded yet. Pick one above, or add one below.</p>
        )}
        {/* Grouped under the section the pond lists each one in, in the Jobs band's own words, so
            "Listening" means one thing on this page; every row says which jobs it is doing, and
            what the models serving it (picture support, a drafter) add to it.
            Conversation then splits by the engine that runs it: the card is the group and the
            models are rows in it, because a card per model is a wall of purple. */}
        {deviceGroups.map((g) => (
          <section key={g.key} id={`mdl-section-${g.key}`} className="mdl-group" aria-label={g.label}>
            <header className="mdl-group__head">
              <h3 className="mdl-group__title">{g.label}</h3>
              <span className="mdl-group__count">
                {g.key === "speaking"
                  ? // The picker lists every voice, so count installed out of all.
                    `${installedVoices.size} of ${allVoices.length} voices installed`
                  : `${g.models.length + g.helpers.length} ${g.models.length + g.helpers.length === 1 ? "model" : "models"}`}
              </span>
            </header>
            {g.key === "speaking" ? (
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
                loading={loading}
                quality={quality}
                onQualityChange={(v) => void chooseQuality(v)}
                availableMb={memory?.available_for_llm_mb ?? null}
              />
            ) : g.key === "conversation" ? (
              groupByEngine(sortModels(g.models)).map((section) => (
                <div key={section.key} className="mdl-engine-section">
                  <EngineHead group={section} />
                  <div className="mdl-group__rows">{section.models.map(row)}</div>
                </div>
              ))
            ) : (
              <div className="mdl-group__rows">
                {g.models.map(row)}
                {g.helpers.map((h) => <HelperRow key={h.id} helper={h} />)}
              </div>
            )}
          </section>
        ))}
      </section>

      <section className="mdl-band" id="mdl-get" aria-labelledby="mdl-get-title">
        <h2 className="mdl-band__title" id="mdl-get-title">Get more</h2>
        <p className="mdl-band__sub">Other models this pond knows about, and anything on Hugging Face.</p>

        {getMoreConversation.length > 0 && (
          <section className="mdl-group" aria-label="More conversation models">
            <header className="mdl-group__head">
              <h3 className="mdl-group__title">Conversation</h3>
              <span className="mdl-group__count">{getMoreConversation.length}</span>
            </header>
            {groupByEngine(getMoreConversation).map((section) => (
              <div key={section.key} className="mdl-engine-section">
                <EngineHead group={section} />
                <div className="mdl-group__rows">{section.models.map(row)}</div>
              </div>
            ))}
          </section>
        )}

        {getMoreListening.length > 0 && (
          <section className="mdl-group" aria-label="More listening models">
            <header className="mdl-group__head">
              <h3 className="mdl-group__title">Listening</h3>
              <span className="mdl-group__count">{getMoreListening.length}</span>
            </header>
            <div className="mdl-group__rows">{getMoreListening.map(row)}</div>
          </section>
        )}

        <AddBand
          carriesPictures={carriesPictures(models)}
          memory={memory}
          onStarted={(message) => {
            say(message);
            // A file named by URL becomes a row of its own, which the transfer then sits on.
            void data.reloadModels();
            void data.reloadDownloads();
            data.watchDownloads();
          }}
        />
      </section>
    </div>
  );
}
