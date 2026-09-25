import { useState, useEffect, useCallback, useRef } from "react";
import { RefreshCw, Loader2, Check } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card, Chip } from "./controls";
import { api } from "../../../api/PondApiClient";
import { promptTemplateIsOutdated } from "../../../api/types";
import type { PromptTemplate } from "../../../api/types";

const CPU_PATH =
  "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM9 9h6v6H9zM9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3";

/** Known preset display names (lowercase key → display label). */
const PRESET_LABELS: Record<string, string> = {
  balanced:  "Balanced",
  concise:   "Concise",
  warm:      "Warm",
  technical: "Technical",
};

const PRESET_ORDER = ["balanced", "concise", "warm", "technical"];

/**
 * Variables `render_jinja_template` (pond-core prompts.rs) interpolates; display only. No
 * `current_date`/`current_time`: the static prefix blanks them to keep the KV cache stable.
 */
const VARS = [
  "{{assistant_name}}",
  "{{user_name}}",
  "{{personality}}",
  "{{timezone}}",
  "{{location}}",
  "{{device_count}}",
  "{{online_device_names}}",
  "{{has_home_devices}}",
  "{{has_tools}}",
  "{{tools}}",
  "{{native_tools_json}}",
  "{{compact_prompt}}",
  "{{thinking_enabled}}",
  "{{reasoning_budget_words}}",
  "{{voice_mode}}",
  "{{canvas_mode}}",
  "{{atypical_speech}}",
];

/** Mock fallback — used when server is unreachable. */
const MOCK_PROMPTS: PromptTemplate[] = [
  {
    name: "balanced",
    content:
      "You are Goose, a friendly home copilot for {{user_name}}. Speak naturally and conversationally. Help with the home, the calendar, the weather and day-to-day questions.",
    is_system: true,
  },
  {
    name: "concise",
    content:
      "You are Goose, a warm on-device home assistant for {{user_name}}. Keep replies to one or two sentences. Never let data leave the home.",
    is_system: true,
  },
  {
    name: "warm",
    content:
      "You are Goose, a cheerful companion in {{user_name}}'s home. Be encouraging and personable. Offer little suggestions to make the day smoother.",
    is_system: true,
  },
  {
    name: "technical",
    content:
      "You are Goose, a precise home automation agent for {{user_name}}. Favor accuracy. State device states explicitly. Require confirmation for security actions.",
    is_system: true,
  },
];

/** Rough token estimate: ~4 chars per token. */
function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

/** Sort prompts: known presets first in PRESET_ORDER, then alphabetically. */
function sortPrompts(list: PromptTemplate[]): PromptTemplate[] {
  const known = PRESET_ORDER
    .map((k) => list.find((p) => p.name === k))
    .filter((p): p is PromptTemplate => p !== undefined);
  const extras = list
    .filter((p) => !PRESET_ORDER.includes(p.name))
    .sort((a, b) => a.name.localeCompare(b.name));
  return [...known, ...extras];
}

// ─── Skeleton placeholder ─────────────────────────────────────
function SkeletonTextarea() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      {[100, 90, 95, 70, 85].map((w, i) => (
        <span
          key={i}
          style={{
            display: "block",
            height: 12,
            width: `${w}%`,
            background: "#e2e8f0",
            borderRadius: 4,
            opacity: 0.6,
          }}
        />
      ))}
    </div>
  );
}

// ─── Component ───────────────────────────────────────────────
interface PromptsDetailProps {
  go: (route: string) => void;
}

export function PromptsDetail({ go }: PromptsDetailProps) {
  const [prompts, setPrompts]   = useState<PromptTemplate[]>([]);
  const [active, setActive]     = useState<string>("");
  const [bodies, setBodies]     = useState<Record<string, string>>({});
  const [originals, setOriginals] = useState<Record<string, string>>({});
  const [loading, setLoading]   = useState(true);
  const [saving, setSaving]     = useState(false);
  const [resetting, setResetting] = useState(false);
  const [chipLoading, setChipLoading] = useState<string | null>(null);
  const [flash, setFlash]       = useState<{ text: string; ok: boolean } | null>(null);
  const [error, setError]       = useState<string | null>(null);
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
      const [list, settings] = await Promise.all([
        api.listPrompts(),
        api.getSettings(),
      ]);
      const arr = Array.isArray(list) && list.length > 0 ? list : MOCK_PROMPTS;
      const sorted = sortPrompts(arr);
      setPrompts(sorted);

      const bodyMap: Record<string, string> = {};
      const origMap: Record<string, string> = {};
      for (const p of sorted) {
        bodyMap[p.name] = p.content;
        origMap[p.name] = p.content;
      }
      setBodies(bodyMap);
      setOriginals(origMap);

      const preferred = settings?.prompt_style?.toLowerCase();
      const defaultActive =
        preferred && sorted.some((p) => p.name === preferred)
          ? preferred
          : sorted[0]?.name ?? "balanced";
      setActive(defaultActive);
    } catch (e) {
      console.warn("[PromptsDetail] API offline — using mock fallback:", e);
      setError("Could not reach the server. Showing offline view.");
      const sorted = sortPrompts(MOCK_PROMPTS);
      setPrompts(sorted);
      const bodyMap: Record<string, string> = {};
      const origMap: Record<string, string> = {};
      for (const p of sorted) {
        bodyMap[p.name] = p.content;
        origMap[p.name] = p.content;
      }
      setBodies(bodyMap);
      setOriginals(origMap);
      setActive("balanced");
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

  async function pickChip(name: string) {
    // Optimistically switch UI; fetch body if not already loaded
    setActive(name);
    if (bodies[name] !== undefined) return;

    setChipLoading(name);
    try {
      const tpl = await api.getPrompt(name);
      setBodies((prev) => ({ ...prev, [name]: tpl.content }));
      setOriginals((prev) => ({ ...prev, [name]: tpl.content }));
    } catch {
      // Keep whatever mock body we have
    } finally {
      setChipLoading(null);
    }
  }

  async function handleSave() {
    if (!active) return;
    setSaving(true);
    try {
      const updated = await api.updatePrompt(active, bodies[active] ?? "");
      setBodies((prev) => ({ ...prev, [active]: updated.content }));
      setOriginals((prev) => ({ ...prev, [active]: updated.content }));
      setPrompts((prev) => prev.map((p) => (p.name === active ? updated : p)));
      showFlash("Prompt saved.");
    } catch (e) {
      showFlash(`Save failed: ${String(e)}`, false);
    } finally {
      setSaving(false);
    }
  }

  async function handleReset() {
    if (!active) return;
    setResetting(true);
    try {
      const restored = await api.resetPrompt(active);
      setBodies((prev) => ({ ...prev, [active]: restored.content }));
      setOriginals((prev) => ({ ...prev, [active]: restored.content }));
      setPrompts((prev) => prev.map((p) => (p.name === active ? restored : p)));
      showFlash("Prompt reset to default.");
    } catch (e) {
      showFlash(`Reset failed: ${String(e)}`, false);
    } finally {
      setResetting(false);
    }
  }

  const body = bodies[active] ?? "";
  const dirty = body !== (originals[active] ?? "");
  const tokens = estimateTokens(body);
  const isBusy = saving || resetting;
  const activeTemplate = prompts.find((p) => p.name === active);
  const outdated = activeTemplate
    ? promptTemplateIsOutdated(activeTemplate)
    : false;

  return (
    <DetailShell
      title="Prompts"
      subtitle="Goose's personality and system instructions."
      accent="#2563EB"
      onBack={() => go("settings")}
      headRight={
        <button
          className="mrow__btn"
          type="button"
          onClick={loadData}
          aria-label="Refresh prompts"
          style={{ minWidth: 32, display: "flex", alignItems: "center", justifyContent: "center" }}
        >
          <RefreshCw size={13} strokeWidth={2} style={{ opacity: loading ? 0.4 : 1 }} />
        </button>
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
            display: "flex",
            alignItems: "center",
            gap: 6,
          }}
          role="status"
          aria-live="polite"
        >
          {flash.ok && <Check size={13} color="#16a34a" strokeWidth={2.5} />}
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

      {/* Personality preset chips */}
      <Card title="Personality preset">
        <div className="preset-row">
          {loading ? (
            // Skeleton chips
            ["Balanced", "Concise", "Warm", "Technical"].map((label) => (
              <span
                key={label}
                style={{
                  display: "inline-block",
                  height: 28,
                  width: label.length * 8 + 24,
                  background: "#e2e8f0",
                  borderRadius: 20,
                  opacity: 0.5,
                }}
              />
            ))
          ) : (
            prompts.map((p) => {
              const label = PRESET_LABELS[p.name] ?? p.name;
              const isActive = active === p.name;
              const isLoadingChip = chipLoading === p.name;
              return (
                <Chip
                  key={p.name}
                  active={isActive}
                  onClick={() => pickChip(p.name)}
                  aria-pressed={isActive}
                >
                  {isLoadingChip ? (
                    <Loader2 size={11} style={{ animation: "spin 1s linear infinite", display: "inline-block" }} />
                  ) : null}
                  {label}
                </Chip>
              );
            })
          )}
        </div>
      </Card>

      {/* System prompt editor */}
      <Card title="System prompt">
        {loading ? (
          <div style={{ padding: "4px 0 8px" }}>
            <SkeletonTextarea />
          </div>
        ) : (
          <textarea
            className="prompt-ta"
            value={body}
            onChange={(e) =>
              setBodies((prev) => ({ ...prev, [active]: e.target.value }))
            }
            rows={7}
            aria-label="System prompt editor"
            spellCheck={false}
            disabled={isBusy}
            style={{ opacity: isBusy ? 0.6 : 1, transition: "opacity 150ms" }}
          />
        )}
        {outdated && (
          <div className="prompt-outdated" role="status">
            <span>
              A newer built-in version of this prompt has shipped since you
              edited it. Your version is kept — Reset adopts the new one and
              discards your changes.
            </span>
          </div>
        )}
        <div className="prompt-foot">
          <span className="prompt-foot__tok">
            <HubIco d={CPU_PATH} size={13} color="var(--color-text-tertiary)" />
            ~{tokens.toLocaleString()} tokens
            {dirty && !loading && (
              <span
                style={{
                  marginLeft: 6,
                  fontSize: 11,
                  fontWeight: 600,
                  color: "#d97706",
                  background: "#fef3c7",
                  border: "1px solid #fde68a",
                  borderRadius: 4,
                  padding: "1px 5px",
                }}
              >
                Unsaved
              </span>
            )}
          </span>
          <div style={{ display: "flex", gap: 8 }}>
            <button
              className="ghost-btn"
              type="button"
              disabled={isBusy || loading}
              onClick={handleReset}
              aria-label="Reset prompt to default"
            >
              {resetting ? (
                <Loader2 size={12} style={{ animation: "spin 1s linear infinite" }} />
              ) : null}
              {resetting ? "Resetting…" : "Reset"}
            </button>
            <button
              className="primary-btn"
              type="button"
              style={{ padding: "9px 16px" }}
              disabled={isBusy || loading || !dirty}
              onClick={handleSave}
              aria-label="Save prompt"
            >
              {saving ? (
                <Loader2 size={12} color="#fff" style={{ animation: "spin 1s linear infinite" }} />
              ) : null}
              {saving ? "Saving…" : "Save"}
            </button>
          </div>
        </div>
      </Card>

      {/* Variables reference */}
      <Card title="Variables">
        <div className="varchips">
          {VARS.map((v) => (
            <span key={v} className="varchip">{v}</span>
          ))}
        </div>
      </Card>
    </DetailShell>
  );
}
