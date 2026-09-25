import { useState, useEffect } from "react";
import { Button, Card, CardContent, Chip, Tabs } from "@heroui/react";
import { Cpu, Info, RotateCcw, Save } from "lucide-react";
import { api } from "../api/PondApiClient";
import { PageHeader, ErrorBanner, SkeletonList } from "../components/shared";
import { promptTemplateIsOutdated } from "../api/types";
import type { PromptTemplate } from "../api/types";

// ── Preset metadata ───────────────────────────────────────────
const PRESET_META: Record<string, { label: string; desc: string }> = {
  balanced:  { label: "Balanced",  desc: "Natural conversation, medium-length responses" },
  concise:   { label: "Concise",   desc: "Short, direct answers. Minimal explanation" },
  technical: { label: "Technical", desc: "Precise, detailed. Favors accuracy over brevity" },
  warm:      { label: "Warm",      desc: "Friendly, encouraging tone. Conversational style" },
};

/** The four preset keys in display order. */
const PRESET_KEYS = ["balanced", "concise", "technical", "warm"];

/** Rough token estimate: ~4 chars per token. */
function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

/** Variables `render_jinja_template` (crates/pond-core/src/prompts.rs) fills. `current_date` and
 *  `current_time` are left out: the static prefix blanks them for KV-cache reuse, so they render empty. */
const TEMPLATE_VARS = [
  "assistant_name",
  "user_name",
  "personality",
  "timezone",
  "location",
  "device_count",
  "online_device_names",
  "has_home_devices",
  "has_tools",
  "tools",
  "native_tools_json",
  "compact_prompt",
  "thinking_enabled",
  "reasoning_budget_words",
  "voice_mode",
  "canvas_mode",
  "atypical_speech",
];

export function Prompts() {
  const [prompts, setPrompts]     = useState<PromptTemplate[]>([]);
  const [active, setActive]       = useState<string>("balanced");
  const [bodies, setBodies]       = useState<Record<string, string>>({});
  const [originals, setOriginals] = useState<Record<string, string>>({});
  const [loading, setLoading]     = useState(true);
  const [saving, setSaving]       = useState(false);
  const [resetting, setResetting] = useState(false);
  const [error, setError]         = useState<string | null>(null);

  useEffect(() => {
    api
      .listPrompts()
      .then((list) => {
        const arr = Array.isArray(list) ? list : [];
        setPrompts(arr);

        const bodyMap: Record<string, string> = {};
        const origMap: Record<string, string> = {};
        for (const p of arr) {
          bodyMap[p.name] = p.content;
          origMap[p.name] = p.content;
        }
        setBodies(bodyMap);
        setOriginals(origMap);

        const firstPreset = PRESET_KEYS.find((k) => arr.some((p) => p.name === k));
        if (firstPreset) setActive(firstPreset);
        else if (arr.length) setActive(arr[0].name);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  function reload() {
    setError(null);
    setLoading(true);
    api.listPrompts()
      .then((list) => {
        const arr = Array.isArray(list) ? list : [];
        setPrompts(arr);
        const bodyMap: Record<string, string> = {};
        const origMap: Record<string, string> = {};
        for (const p of arr) { bodyMap[p.name] = p.content; origMap[p.name] = p.content; }
        setBodies(bodyMap);
        setOriginals(origMap);
        const firstPreset = PRESET_KEYS.find((k) => arr.some((p) => p.name === k));
        if (firstPreset) setActive(firstPreset);
        else if (arr.length) setActive(arr[0].name);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  const orderedPrompts = [
    ...PRESET_KEYS.map((k) => prompts.find((p) => p.name === k)).filter(Boolean),
    ...prompts.filter((p) => !PRESET_KEYS.includes(p.name)),
  ] as PromptTemplate[];

  const body = bodies[active] ?? "";
  const dirty = body !== (originals[active] ?? "");
  const tokens = estimateTokens(body);
  const activeTemplate = prompts.find((p) => p.name === active);
  const outdated = activeTemplate ? promptTemplateIsOutdated(activeTemplate) : false;

  async function save() {
    if (!active) return;
    setSaving(true);
    try {
      const updated = await api.updatePrompt(active, body);
      setPrompts((prev) => prev.map((p) => (p.name === active ? updated : p)));
      setOriginals((prev) => ({ ...prev, [active]: updated.content }));
      setBodies((prev) => ({ ...prev, [active]: updated.content }));
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  async function reset() {
    if (!active) return;
    setResetting(true);
    try {
      const restored = await api.resetPrompt(active);
      setPrompts((prev) => prev.map((p) => (p.name === active ? restored : p)));
      setOriginals((prev) => ({ ...prev, [active]: restored.content }));
      setBodies((prev) => ({ ...prev, [active]: restored.content }));
    } catch (e) {
      setError(String(e));
    } finally {
      setResetting(false);
    }
  }

  if (loading) return <SkeletonList rows={4} />;
  if (error)
    return <ErrorBanner error={error} onRetry={reload} />;
  if (!prompts.length)
    return (
      <p className="muted-12">
        No prompt templates found. Prompts are created automatically when the agent runs for the
        first time.
      </p>
    );

  return (
    <div className="screen">
      <PageHeader
        title="Prompts"
        action={
          <Chip size="sm" variant="soft" className="header-status-chip">
            {orderedPrompts.length} preset{orderedPrompts.length !== 1 ? "s" : ""}
          </Chip>
        }
      />

      {/* Preset tabs */}
      <Tabs selectedKey={active} onSelectionChange={(k) => setActive(String(k))}>
        <Tabs.ListContainer>
          <Tabs.List aria-label="Prompt presets" className="prompt-tabs">
            {orderedPrompts.map((p) => {
              const meta = PRESET_META[p.name];
              return (
                <Tabs.Tab
                  key={p.name}
                  id={p.name}
                  onClick={() => setActive(p.name)}
                  className="prompt-tab"
                >
                  <Tabs.Indicator />
                  <div className="prompt-tab__title">
                    <span>{meta?.label ?? p.name}</span>
                    <span className="prompt-tab__desc">
                      {meta?.desc ?? "Custom template"}
                    </span>
                  </div>
                </Tabs.Tab>
              );
            })}
          </Tabs.List>
        </Tabs.ListContainer>
      </Tabs>

      {/* Editor card */}
      <Card className="card">
        <CardContent>
          <textarea
            className="prompt-area"
            value={body}
            onChange={(e) =>
              setBodies((prev) => ({ ...prev, [active]: e.target.value }))
            }
            aria-label="Prompt editor"
            rows={16}
            spellCheck={false}
            placeholder={`Write the ${PRESET_META[active]?.label?.toLowerCase() ?? active} system prompt…`}
          />
          {outdated && (
            <div className="prompt-outdated" role="status">
              <Info size={13} />
              <span>
                A newer built-in version of this prompt has shipped since you
                edited it. Your version is kept — Reset adopts the new one and
                discards your changes.
              </span>
            </div>
          )}
          <div className="prompt-foot">
            <div className="prompt-foot__tokens">
              <Cpu size={13} />
              <span>~{tokens.toLocaleString()} tokens</span>
              {dirty && (
                <Chip size="sm" variant="soft" color="warning">
                  Unsaved
                </Chip>
              )}
            </div>
            <div className="prompt-foot__actions">
              <Button
                variant="ghost"
                size="sm"
                onPress={reset}
                isDisabled={resetting || saving}
              >
                <RotateCcw size={13} />
                {resetting ? "Resetting…" : "Reset"}
              </Button>
              <Button
                variant="primary"
                size="sm"
                onPress={save}
                isDisabled={saving || !dirty}
              >
                <Save size={13} />
                {saving ? "Saving…" : "Save"}
              </Button>
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Variables reference card */}
      <Card className="card">
        <CardContent>
          <span className="card__label">Variables you can use</span>
          <div className="var-grid">
            {TEMPLATE_VARS.map((v) => (
              <code key={v} className="var-chip">
                {`{{${v}}}`}
              </code>
            ))}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
