import { useState, useEffect, useCallback, useRef } from "react";
import { Download, Check, RefreshCw, Loader2, Image } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
// Row and Toggle are used only by the commented-out Speed card below.
import { Card /* , Row, Toggle */ } from "./controls";
import { api } from "../../../api/PondApiClient";
import { voiceTitle } from "../../../voice/voiceCatalogue";
import { useVisionStatus } from "../../../api/useVisionStatus";
import type { ModelEntry, ModelActiveRoles /* , Settings */ } from "../../../api/types";

// ─── Icon path strings for this view ─────────────────────────
const SICN = {
  chat:    "M21 12a8 8 0 0 1-11.5 7.2L4 21l1.8-5.4A8 8 0 1 1 21 12z",
  spark:   "M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z",
  bolt:    "M13 2L3 14h7l-1 8 11-12h-7z",
  ear:     "M6 8a6 6 0 0 1 12 0c0 3-1.5 4-3 5s-2 2-2 4-1 3-3 3-3-2-3-4M9 12a3 3 0 0 1 6 0",
  speaker: "M11 5L6 9H2v6h4l5 4zM19 5a10 10 0 0 1 0 14M15.5 8.5a5 5 0 0 1 0 7",
  cpu:     "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM9 9h6v6H9zM9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3",
} as const;

// ─── Role tile definitions ────────────────────────────────────
interface RoleTile {
  role: string;
  /** Key in ModelActiveRoles */
  key: keyof ModelActiveRoles;
  icon: string;
  c: string;
  bg: string;
}

const ROLE_TILES: RoleTile[] = [
  { role: "Chat",           key: "chat",  icon: SICN.chat,    c: "#7C3AED", bg: "#EDE9FE" },
  { role: "Think",          key: "chat",  icon: SICN.spark,   c: "#D97706", bg: "#FEF3C7" },
  { role: "Task",           key: "chat",  icon: SICN.bolt,    c: "#16A34A", bg: "#DCFCE7" },
  { role: "Speech-to-text", key: "asr",   icon: SICN.ear,     c: "#2563EB", bg: "#DBEAFE" },
  { role: "Text-to-speech", key: "tts",   icon: SICN.speaker, c: "#DB2777", bg: "#FCE7F3" },
];

// ─── Mock fallback (used offline so screen still renders) ─────
const MOCK_ROLES: ModelActiveRoles = {
  chat:      null,
  tool:      null,
  asr:       null,
  tts:       null,
  embedding: null,
};

const NON_LLM_PROVIDERS = new Set(["whisper", "tts", "tts_piper", "tts_kokoro", "tts_http", "embedding"]);

function isLlmModel(m: ModelEntry): boolean {
  return !NON_LLM_PROVIDERS.has(m.provider) && m.category !== "embedding";
}

function isAsrModel(m: ModelEntry): boolean {
  return m.provider === "whisper" || m.category === "asr";
}

function isTtsModel(m: ModelEntry): boolean {
  return m.provider === "tts" || m.provider === "tts_piper" || m.provider === "tts_kokoro" || m.provider === "tts_http" || m.category === "tts";
}

// ─── Skeleton row ─────────────────────────────────────────────
function SkeletonRow() {
  return (
    <div className="mrow" style={{ opacity: 0.5 }}>
      <span className="mrow__icon" style={{ background: "#f1f5f9", borderRadius: 6, width: 28, height: 28 }} />
      <div className="mrow__text" style={{ gap: 4 }}>
        <span style={{ display: "block", height: 12, width: 160, background: "#e2e8f0", borderRadius: 4 }} />
        <span style={{ display: "block", height: 10, width: 100, background: "#f1f5f9", borderRadius: 4 }} />
      </div>
    </div>
  );
}

// ─── Component ───────────────────────────────────────────────
interface ModelsDetailProps {
  go: (route: string) => void;
}

export function ModelsDetail({ go }: ModelsDetailProps) {
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [activeRoles, setActiveRoles] = useState<ModelActiveRoles>(MOCK_ROLES);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activating, setActivating] = useState<string | null>(null);
  const [flash, setFlash] = useState<{ text: string; ok: boolean } | null>(null);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Picture support for the active chat row only — every other row's fact is
  // the static "pictures" tag, derived from `reads_images` on the list.
  const { status: visionStatus } = useVisionStatus();

  // Speculative decoding was taken out of the llama.cpp engine on 2026-09-24 (goose 743649d98),
  // so the Speed card and its setting are commented out rather than deleted; restore them together.
  // // Settings, loaded SEPARATELY from the model list above (Voice.tsx's
  // // guard): a settings failure must not blank the whole page into the
  // // offline view, and a settings success must not wait on — or block — the
  // // model scan.
  // const [settings, setSettings] = useState<Settings | null>(null);
  // const [settingsLoaded, setSettingsLoaded] = useState(false);
  // const [draftAheadPending, setDraftAheadPending] = useState(false);

  function showFlash(text: string, ok = true) {
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlash({ text, ok });
    flashTimer.current = setTimeout(() => setFlash(null), 3000);
  }

  const loadData = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [fetchedModels, fetchedRoles] = await Promise.all([
        api.listModels(),
        api.getActiveRoles(),
      ]);
      setModels(fetchedModels);
      setActiveRoles(fetchedRoles);
    } catch (e) {
      console.warn("[ModelsDetail] API offline — using mock fallback:", e);
      setError("Could not reach the server. Showing offline view.");
    } finally {
      setLoading(false);
    }
  }, []);

  // const loadSettings = useCallback(async () => {
  //   try {
  //     const s = await api.getSettings();
  //     if (s && typeof s === "object") {
  //       setSettings(s);
  //       setSettingsLoaded(true);
  //     } else {
  //       throw new Error("settings response was empty");
  //     }
  //   } catch (e) {
  //     console.warn("[ModelsDetail] could not load settings:", e);
  //     setSettingsLoaded(false);
  //   }
  // }, []);

  useEffect(() => {
    loadData();
    // loadSettings();
    return () => {
      if (flashTimer.current) clearTimeout(flashTimer.current);
    };
  }, [loadData /* , loadSettings */]);

  // // Defaults ON, so an absent key (a settings row saved before this field
  // // existed) reads as ON — `!== false`, not `?? false`.
  // const draftAhead = settings?.speculative_decoding_enabled !== false;
  //
  // async function handleDraftAheadChange(on: boolean) {
  //   if (draftAheadPending || !settings) return;
  //   setDraftAheadPending(true);
  //   const previous = settings.speculative_decoding_enabled;
  //   setSettings((prev) => (prev ? { ...prev, speculative_decoding_enabled: on } : prev));
  //   try {
  //     // Patch only the changed key — the key SET is what marks user intent,
  //     // and the server echo is deliberately NOT adopted below (it can be
  //     // stale against a fast second click).
  //     await api.updateSettings({ speculative_decoding_enabled: on });
  //     showFlash(
  //       on
  //         ? "Guessing ahead is on. The model is reloading, so the next reply waits for it."
  //         : "Guessing ahead is off. The model is reloading, so the next reply waits for it.",
  //     );
  //   } catch (e) {
  //     setSettings((prev) =>
  //       prev ? { ...prev, speculative_decoding_enabled: previous } : prev,
  //     );
  //     const reason = e instanceof Error ? e.message : String(e);
  //     showFlash(
  //       `Could not turn guessing ahead ${on ? "on" : "off"}: ${reason}. It is still ${on ? "off" : "on"}; try again.`,
  //       false,
  //     );
  //   } finally {
  //     setDraftAheadPending(false);
  //   }
  // }

  async function handleActivate(provider: string, name: string, role: string) {
    const key = `${provider}/${name}/${role}`;
    setActivating(key);
    try {
      await api.activateModel(provider, name, role);
      const freshRoles = await api.getActiveRoles();
      setActiveRoles(freshRoles);
      showFlash(`${name} set as ${role} model.`);
    } catch (e) {
      showFlash(`Failed to activate ${name}: ${String(e)}`, false);
    } finally {
      setActivating(null);
    }
  }

  // ── Derived lists ──────────────────────────────────────────
  const llmModels = models.filter(isLlmModel);
  const asrModels = models.filter(isAsrModel);
  const ttsModels = models.filter(isTtsModel);

  // ── Role tile model label helper ───────────────────────────
  function roleModel(tile: RoleTile): string {
    const assignment = activeRoles[tile.key];
    if (!assignment || !("model" in assignment) || !assignment.model) return "—";
    return assignment.model;
  }

  // ── Chat role active check ─────────────────────────────────
  function isChatActive(m: ModelEntry): boolean {
    const a = activeRoles.chat;
    if (!a) return false;
    return a.provider === m.provider && a.model === m.name;
  }

  function isAsrActive(m: ModelEntry): boolean {
    const a = activeRoles.asr;
    if (!a) return false;
    return a.provider === m.provider && a.model === m.name;
  }

  function isTtsActive(m: ModelEntry): boolean {
    const a = activeRoles.tts;
    if (!a) return false;
    return a.provider === m.provider && a.model === m.name;
  }

  return (
    <DetailShell
      title="Models"
      subtitle="Local language & speech models powering Goose. Everything runs on-device."
      accent="#7C3AED"
      onBack={() => go("settings")}
      headRight={
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <button
            className="mrow__btn"
            type="button"
            onClick={() => { loadData(); /* loadSettings(); */ }}
            aria-label="Refresh models"
            style={{ minWidth: 32, display: "flex", alignItems: "center", justifyContent: "center" }}
          >
            <RefreshCw size={13} strokeWidth={2} style={{ opacity: loading ? 0.4 : 1 }} />
          </button>
          {/* TODO Phase 8 wave 2: open download modal — api.downloadModel(category, name) */}
          <button className="primary-btn" type="button" disabled title="Model download browser coming in Phase 8 wave 2">
            <Download size={15} color="#fff" strokeWidth={2.2} /> Download
          </button>
        </div>
      }
    >
      {/* Flash feedback */}
      {flash && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: flash.ok ? "#f0fdf4" : "#fef2f2",
            color: flash.ok ? "#16a34a" : "#dc2626",
            border: `1px solid ${flash.ok ? "#bbf7d0" : "#fecaca"}`,
          }}
          role="status"
          aria-live="polite"
        >
          {flash.text}
        </div>
      )}

      {/* Offline error banner */}
      {error && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: "#fffbeb",
            color: "#92400e",
            border: "1px solid #fde68a",
          }}
        >
          {error}
        </div>
      )}

      {/* Active roles */}
      <Card title="Active roles">
        <div className="roles-grid">
          {ROLE_TILES.map((r) => (
            <div key={r.role} className="role2">
              <span className="role2__icon" style={{ background: r.bg }}>
                <HubIco d={r.icon} size={16} color={r.c} />
              </span>
              <div className="role2__text">
                <span className="role2__role">{r.role}</span>
                <span className="role2__model">
                  {loading ? (
                    <span style={{ display: "inline-block", height: 10, width: 80, background: "#e2e8f0", borderRadius: 4, verticalAlign: "middle" }} />
                  ) : (
                    roleModel(r)
                  )}
                </span>
              </div>
            </div>
          ))}
        </div>
        {/* TODO Phase 8 wave 2: clicking a role tile navigates to the relevant model list */}
      </Card>

      {/* Language models */}
      <Card title="Language models">
        <div className="mlist">
          {loading ? (
            <>
              <SkeletonRow />
              <SkeletonRow />
              <SkeletonRow />
            </>
          ) : llmModels.length === 0 ? (
            <p style={{ margin: 0, fontSize: 13, color: "var(--color-text-tertiary)", padding: "8px 0" }}>
              No language models found. Download one to get started.
            </p>
          ) : (
            llmModels.map((m) => {
              const active = isChatActive(m);
              const activateKey = `${m.provider}/${m.name}/chat`;
              const isActivating = activating === activateKey;
              // Derive tags from recommended_role and model name
              const tags: string[] = [];
              if (m.recommended_role === "chat" || (!m.recommended_role && isLlmModel(m))) tags.push("chat");
              if (/gemma.?4|qwen3|qwq|deepseek.?r1/.test(m.name.toLowerCase())) tags.push("think");
              if (m.reads_images === true) tags.push("pictures");
              // Picture support's OWN lifecycle (downloading, verifying, a
              // device that declines the encoder) only means anything for the
              // model actually in use — every other row's fact is the static
              // "pictures" tag above.
              const liveVision = active && visionStatus?.message ? visionStatus.message : null;

              return (
                <div key={m.id} className={`mrow${active ? " mrow--active" : ""}`}>
                  <span className="mrow__icon">
                    <HubIco d={SICN.cpu} size={16} color={active ? "#7C3AED" : "var(--color-text-tertiary)"} />
                  </span>
                  <div className="mrow__text">
                    <span className="mrow__name">{m.display_name ?? (m.provider === "tts_kokoro" ? voiceTitle(m.name) : m.name)}</span>
                    <span className="mrow__file">
                      {m.provider} / {m.name}
                      {m.size_mb != null ? ` · ${(m.size_mb / 1024).toFixed(1)} GB` : ""}
                      {m.ram_estimate_mb != null ? ` · ${m.ram_estimate_mb} MB RAM` : ""}
                      {liveVision ? ` · ${liveVision}` : ""}
                    </span>
                  </div>
                  <div className="mrow__tags">
                    {tags.map((t) =>
                      t === "pictures" ? (
                        <span key={t} className="mtag">
                          <Image size={11} aria-hidden="true" /> pictures
                        </span>
                      ) : (
                        <span key={t} className="mtag">{t}</span>
                      ),
                    )}
                  </div>
                  {active ? (
                    <span className="mrow__loaded">
                      <Check size={12} color="#16A34A" strokeWidth={3} /> Loaded
                    </span>
                  ) : (
                    <button
                      className="mrow__btn"
                      type="button"
                      disabled={isActivating}
                      onClick={() => handleActivate(m.provider, m.name, "chat")}
                      aria-label={`Load ${m.name} as chat model`}
                    >
                      {isActivating ? <Loader2 size={12} style={{ animation: "spin 1s linear infinite" }} /> : "Load"}
                    </button>
                  )}
                </div>
              );
            })
          )}
        </div>
      </Card>

      {/* Speculative decoding was taken out of the llama.cpp engine on 2026-09-24 (goose
          743649d98), so this card is commented out rather than deleted; restore it with the
          setting.
      Speed: the one knob this page owned. Between the language
          models it applies to and Speech, so it reads as "how the model
          above answers" rather than a stray setting.
      <Card title="Speed">
        {settingsLoaded ? (
          <Row
            label="Guess ahead with a helper model"
            sub="Answers stay the same; only the speed changes. Faster on a Jetson, can be slower on a Mac. Only Gemma 4 E2B and E4B have a helper."
            control={
              // `key` on purpose — see Voice.tsx's thinking-tone toggle: the
              // hub Toggle seeds its own state from `on` via useState and
              // never re-reads the prop, and settings arrive a render after
              // mount. Without the remount key a stored `false` draws ON.
              <Toggle
                key={`draft-ahead-${draftAhead}`}
                on={draftAhead}
                onChange={handleDraftAheadChange}
                label="Guess ahead with a helper model"
              />
            }
          />
        ) : (
          <Row
            label="Guess ahead with a helper model"
            sub="Could not read this setting. Use Refresh above to try again."
          />
        )}
      </Card>
      */}

      {/* Speech models */}
      <Card title="Speech">
        <div className="mlist">
          {loading ? (
            <>
              <SkeletonRow />
              <SkeletonRow />
            </>
          ) : asrModels.length === 0 && ttsModels.length === 0 ? (
            <p style={{ margin: 0, fontSize: 13, color: "var(--color-text-tertiary)", padding: "8px 0" }}>
              No speech models found.
            </p>
          ) : (
            <>
              {asrModels.map((m) => {
                const active = isAsrActive(m);
                const activateKey = `${m.provider}/${m.name}/asr`;
                const isActivating = activating === activateKey;
                return (
                  <div key={m.id} className={`mrow${active ? " mrow--active" : ""}`}>
                    <span className="mrow__icon">
                      <HubIco d={SICN.ear} size={16} color={active ? "#7C3AED" : "var(--color-text-tertiary)"} />
                    </span>
                    <div className="mrow__text">
                      <span className="mrow__name">{m.display_name ?? (m.provider === "tts_kokoro" ? voiceTitle(m.name) : m.name)}</span>
                      <span className="mrow__file">
                        Speech-to-text · {m.provider} / {m.name}
                      </span>
                    </div>
                    {active ? (
                      <span className="mrow__loaded">
                        <Check size={12} color="#16A34A" strokeWidth={3} /> Active
                      </span>
                    ) : (
                      <button
                        className="mrow__btn"
                        type="button"
                        disabled={isActivating}
                        onClick={() => handleActivate(m.category ?? m.provider, m.name, "asr")}
                        aria-label={`Use ${m.name} as speech-to-text model`}
                      >
                        {isActivating ? <Loader2 size={12} style={{ animation: "spin 1s linear infinite" }} /> : "Use"}
                      </button>
                    )}
                  </div>
                );
              })}

              {ttsModels.map((m) => {
                const active = isTtsActive(m);
                const activateKey = `${m.provider}/${m.name}/tts`;
                const isActivating = activating === activateKey;
                return (
                  <div key={m.id} className={`mrow${active ? " mrow--active" : ""}`}>
                    <span className="mrow__icon">
                      <HubIco d={SICN.speaker} size={16} color={active ? "#7C3AED" : "var(--color-text-tertiary)"} />
                    </span>
                    <div className="mrow__text">
                      <span className="mrow__name">{m.display_name ?? (m.provider === "tts_kokoro" ? voiceTitle(m.name) : m.name)}</span>
                      <span className="mrow__file">
                        Text-to-speech · {m.provider} / {m.name}
                      </span>
                    </div>
                    {active ? (
                      <span className="mrow__loaded">
                        <Check size={12} color="#16A34A" strokeWidth={3} /> Active
                      </span>
                    ) : (
                      <button
                        className="mrow__btn"
                        type="button"
                        disabled={isActivating}
                        onClick={() => handleActivate(m.category ?? m.provider, m.name, "tts")}
                        aria-label={`Use ${m.name} as text-to-speech voice`}
                      >
                        {isActivating ? <Loader2 size={12} style={{ animation: "spin 1s linear infinite" }} /> : "Use"}
                      </button>
                    )}
                  </div>
                );
              })}
            </>
          )}
        </div>
      </Card>
    </DetailShell>
  );
}
