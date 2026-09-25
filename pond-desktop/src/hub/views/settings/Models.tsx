import { useState, useEffect, useCallback, useRef } from "react";
import { Download, Check, RefreshCw, Loader2 } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card } from "./controls";
import { api } from "../../../api/PondApiClient";
import { voiceTitle } from "../../../voice/voiceCatalogue";
import type { ModelEntry, ModelActiveRoles } from "../../../api/types";

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

  useEffect(() => {
    loadData();
    return () => {
      if (flashTimer.current) clearTimeout(flashTimer.current);
    };
  }, [loadData]);

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
            onClick={loadData}
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
              const tags: string[] = [];
              if (m.recommended_role === "chat" || (!m.recommended_role && isLlmModel(m))) tags.push("chat");
              if (/gemma.?4|qwen3|qwq|deepseek.?r1/.test(m.name.toLowerCase())) tags.push("think");

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
                    </span>
                  </div>
                  <div className="mrow__tags">
                    {tags.map((t) => (
                      <span key={t} className="mtag">{t}</span>
                    ))}
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
