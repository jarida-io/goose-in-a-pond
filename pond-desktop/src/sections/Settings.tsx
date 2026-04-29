import { useState, useEffect, lazy, Suspense } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Tabs,
  Switch,
  Button,
  Card,
  CardContent,
  Chip,
  RadioGroup,
  Radio,
} from "@heroui/react";
import { Trash2, Plus } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppDispatch, useAppState } from "../state/AppContext";
import type { Settings as SettingsType, Extension } from "../api/types";
import { ModelPickerModal, type ModelRole } from "../components/ModelPickerModal";
const WakeWordCalibration = lazy(() => import("../components/WakeWordCalibration").then(m => ({ default: m.WakeWordCalibration })));

// ── Tab definitions ───────────────────────────────────────────

type SettingsTab = "identity" | "voice" | "models" | "prompts" | "location" | "agent" | "data" | "tools";

const TABS: Array<{ id: SettingsTab; label: string }> = [
  { id: "identity", label: "Identity" },
  { id: "voice",    label: "Voice" },
  { id: "models",   label: "Models" },
  { id: "prompts",  label: "Prompts" },
  { id: "location", label: "Location" },
  { id: "agent",    label: "Agent" },
  { id: "data",     label: "Data" },
  { id: "tools",    label: "Tools" },
];

// ── Shared form primitives ────────────────────────────────────

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <Card shadow="none" className="giap-card">
      <CardContent>
        <h4 className="section__title">{title}</h4>
        <div className="section__rows">{children}</div>
      </CardContent>
    </Card>
  );
}

function FormRow({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="settings-row">
      <div className="settings-row__label">
        <div className="settings-row__name">{label}</div>
        {hint && <div className="settings-row__hint">{hint}</div>}
      </div>
      <div>{children}</div>
    </div>
  );
}

// Hardcoded common IANA timezone list (offline-first, no API needed)
const TIMEZONES = [
  "UTC",
  "America/New_York",
  "America/Chicago",
  "America/Denver",
  "America/Los_Angeles",
  "America/Anchorage",
  "Pacific/Honolulu",
  "America/Toronto",
  "America/Vancouver",
  "Europe/London",
  "Europe/Paris",
  "Europe/Berlin",
  "Europe/Rome",
  "Europe/Moscow",
  "Asia/Tokyo",
  "Asia/Shanghai",
  "Asia/Kolkata",
  "Asia/Dubai",
  "Australia/Sydney",
];

// ── Tab Panels ────────────────────────────────────────────────

function IdentityTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  return (
    <div className="settings-body">
      <Section title="Personal">
        <FormRow label="Your name" hint="How Goose addresses you">
          <input
            style={nativeInput}
            value={s.user_name ?? ""}
            onChange={(e) => patch("user_name", e.target.value)}
            placeholder="Friend"
          />
        </FormRow>
        <FormRow label="Assistant name" hint="What you call your assistant">
          <input
            style={nativeInput}
            value={s.assistant_name ?? ""}
            onChange={(e) => patch("assistant_name", e.target.value)}
            placeholder="Goose"
          />
        </FormRow>
        <FormRow label="Timezone">
          <select
            style={selectFallback}
            value={s.timezone ?? "UTC"}
            onChange={(e) => patch("timezone", e.target.value)}
          >
            {TIMEZONES.map((tz) => (
              <option key={tz} value={tz}>{tz}</option>
            ))}
          </select>
        </FormRow>
      </Section>

      <Section title="Personality">
        <FormRow label="Personality" hint="Describe how your assistant should behave">
          <textarea
            style={{ ...nativeInput, height: "80px", resize: "vertical" as const }}
            value={s.assistant_personality ?? s.personality ?? ""}
            onChange={(e) => patch("assistant_personality", e.target.value)}
            placeholder="Friendly, concise, and helpful"
          />
        </FormRow>
      </Section>
    </div>
  );
}

function VoiceTab({
  s,
  patch,
  hotkey,
  setHotkey,
  applyHotkey,
  refreshSettings,
}: {
  s: Partial<SettingsType>;
  patch: (k: keyof SettingsType, v: unknown) => void;
  hotkey: string;
  setHotkey: (v: string) => void;
  applyHotkey: () => void;
  refreshSettings: () => Promise<void>;
}) {
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [calibrating, setCalibrating] = useState(false);
  const recDur = s.voice_recording_duration_secs ?? 30;

  const wakePhrase = (s.voice_wake_word ?? s.wake_word ?? "").trim();
  const transcriptions = s.voice_wake_word_transcriptions ?? [];
  const isCalibrated = transcriptions.length > 0;

  async function startCalibration() {
    if (wakePhrase) {
      try {
        await api.updateSettings({ voice_wake_word: wakePhrase });
        await api.resetWakeWordCalibration();
      } catch { /* ignore -- calibration component handles errors */ }
    }
    setCalibrating(true);
  }

  async function handleCalibrationComplete() {
    setCalibrating(false);
    await refreshSettings();
  }

  async function clearCalibration() {
    try {
      await api.resetWakeWordCalibration();
      patch("voice_wake_word_transcriptions", []);
    } catch { /* ignore */ }
  }

  return (
    <div className="settings-body">
      {/* Activation */}
      <Section title="Activation">
        <FormRow label="Keyboard shortcut" hint="Press this to activate voice from anywhere">
          <div className="shortcut-row">
            <input
              style={{ ...nativeInput, flex: 1 }}
              value={hotkey}
              onChange={(e) => setHotkey(e.target.value)}
              placeholder="CmdOrCtrl+Shift+V"
            />
            <Button variant="outline" onPress={applyHotkey}>Apply</Button>
          </div>
        </FormRow>
        <FormRow label="Wake phrase" hint="Say this phrase to activate voice mode">
          <input
            style={nativeInput}
            value={s.voice_wake_word ?? s.wake_word ?? ""}
            onChange={(e) => patch("voice_wake_word", e.target.value)}
            placeholder="goose"
            disabled={calibrating}
          />
        </FormRow>

        {/* Calibration status */}
        {!calibrating && wakePhrase && (
          <div style={calibrationRow}>
            <span
              style={{
                ...calibrationDot,
                background: isCalibrated ? "var(--color-success)" : "var(--color-warning)",
              }}
            />
            <span style={calibrationLabel}>
              {isCalibrated
                ? `Calibrated (${transcriptions.length} variant${transcriptions.length !== 1 ? "s" : ""})`
                : "Not calibrated"}
            </span>
            <div style={{ marginLeft: "auto", display: "flex", gap: "var(--space-2)" }}>
              {isCalibrated && (
                <Button variant="outline" size="sm" onPress={clearCalibration}>Clear</Button>
              )}
              <Button variant="outline" size="sm" onPress={startCalibration}>
                {isCalibrated ? "Re-calibrate" : "Calibrate"}
              </Button>
            </div>
          </div>
        )}

        {/* Inline calibration component */}
        {calibrating && wakePhrase && (
          <Suspense fallback={<p className="muted-12">Loading calibration...</p>}>
            <WakeWordCalibration
              phrase={wakePhrase}
              onComplete={handleCalibrationComplete}
              onCancel={() => setCalibrating(false)}
            />
          </Suspense>
        )}

        {/* Wake word info */}
        {!calibrating && (
          <div style={wakeWordNote}>
            <span style={wakeWordNoteIcon}>i</span>
            <span>
              {wakePhrase
                ? "Calibrating improves detection accuracy by learning how Whisper transcribes your voice. Record 3-5 samples for best results."
                : "When set, the app listens passively while Voice mode is open and activates automatically when the phrase is heard. Leave blank to use the keyboard shortcut only."}
            </span>
          </div>
        )}
      </Section>

      {/* Recording */}
      <Section title="Recording">
        <FormRow label={`Max listen time: ${recDur}s`} hint="Auto-stops recording after this duration">
          <input
            type="range"
            min={5}
            max={120}
            step={5}
            value={recDur}
            onChange={(e) => patch("voice_recording_duration_secs", Number(e.target.value))}
            style={{ width: "100%" }}
          />
        </FormRow>
      </Section>

      {/* Advanced toggle */}
      <button
        style={advancedToggleStyle}
        onClick={() => setShowAdvanced((v) => !v)}
        aria-expanded={showAdvanced}
      >
        {showAdvanced ? "\u25BE" : "\u25B8"} Advanced voice settings
      </button>

      {showAdvanced && (
        <>
          <Section title="Transcription">
            <FormRow label="Server address" hint="Where the speech-to-text server is running">
              <input
                style={nativeInput}
                value={s.voice_whisper_url ?? ""}
                onChange={(e) => patch("voice_whisper_url", e.target.value)}
                placeholder="http://127.0.0.1:9000"
              />
            </FormRow>
            <FormRow label="Model file" hint="Speech recognition model (e.g. ggml-base.bin)">
              <input
                style={nativeInput}
                value={s.active_whisper_model ?? ""}
                onChange={(e) => patch("active_whisper_model", e.target.value)}
                placeholder="ggml-base.bin"
              />
            </FormRow>
          </Section>

          <Section title="Speech Synthesis">
            <FormRow label="Voice model" hint="Piper voice model file (.onnx)">
              <input
                style={nativeInput}
                value={s.active_tts_model ?? ""}
                onChange={(e) => patch("active_tts_model", e.target.value)}
                placeholder="en_US-lessac-medium.onnx"
              />
            </FormRow>
            <FormRow label="Voice name">
              <div style={{ display: "flex", gap: "var(--space-2)", alignItems: "center" }}>
                <input
                  style={{ ...nativeInput, flex: 1 }}
                  value={s.voice_tts_voice ?? ""}
                  onChange={(e) => patch("voice_tts_voice", e.target.value)}
                  placeholder="en_US-lessac-medium.onnx"
                />
                <Button variant="outline" isDisabled title="Coming soon">Preview</Button>
              </div>
            </FormRow>
          </Section>
        </>
      )}
    </div>
  );
}

// Helper that shows the current model assignment and a "Change{"\u2026"}" button
function ModelRoleRow({
  provider,
  model,
  onPick,
}: {
  provider?: string | null;
  model?: string | null;
  onPick: () => void;
}) {
  const label = provider && model ? `${provider} / ${model}` : "Not set";
  return (
    <div className="model-picker">
      <span className="model-picker__current" style={{
        fontFamily: "var(--font-mono)",
        fontSize: "var(--text-sm)",
        color: provider ? "var(--fg)" : "var(--grey-500)",
        overflow: "hidden",
        textOverflow: "ellipsis",
        whiteSpace: "nowrap",
      }}>
        {label}
      </span>
      <Button variant="outline" onPress={onPick}>Change{"\u2026"}</Button>
    </div>
  );
}

function ModelsTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const [pickerOpen, setPickerOpen] = useState(false);
  const [toolModels, setToolModels] = useState<string[]>([]);

  // Seed model fields from live active roles if settings don't already have them
  useEffect(() => {
    api.getActiveRoles().then((roles) => {
      if (!s.chat_provider && roles.chat) {
        patch("chat_provider", roles.chat.provider);
        patch("chat_model", roles.chat.model);
      }
    }).catch(() => {/* non-fatal */});

    // Fetch available GGUF models for the tool-caller dropdown
    api.listModels().then((models) => {
      const gguf = models
        .filter((m) => m.provider === "gguf" || m.provider === "local")
        .map((m) => m.name);
      setToolModels(gguf);
    }).catch(() => {/* non-fatal */});
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const temp = s.llm_temperature ?? 0.7;
  const maxTokenOpts = [128, 256, 512, 1024, 2048, 4096];

  return (
    <div className="settings-body">
      <Section title="AI Models">
        <FormRow label="Main LLM" hint="Handles all conversation, reasoning, and response generation">
          <ModelRoleRow
            provider={s.chat_provider}
            model={s.chat_model}
            onPick={() => setPickerOpen(true)}
          />
        </FormRow>

        <FormRow label="Tool Caller" hint="Small specialist model for structured tool-call arguments (optional)">
          <select
            value={s.tool_model ?? ""}
            onChange={(e) => patch("tool_model", e.target.value || null)}
            style={{ width: "100%", padding: "var(--space-2)", borderRadius: "var(--radius-2)", border: "1px solid var(--border)", background: "var(--surface)" }}
          >
            <option value="">None (use main LLM for tool calls)</option>
            {toolModels.map((name) => (
              <option key={name} value={name}>{name}</option>
            ))}
          </select>
          {(!s.tool_model || s.tool_model === s.chat_model) && (
            <span style={{ fontSize: "0.75rem", color: "var(--text-secondary)", marginTop: 4, display: "block" }}>
              Same model as main LLM — zero swap overhead
            </span>
          )}
        </FormRow>
      </Section>

      <Section title="Response Quality">
        <FormRow label="Thinking Mode" hint="Enable internal reasoning for better analysis, planning, and complex answers">
          <select
            style={selectFallback}
            value={s.thinking_mode ?? "auto"}
            onChange={(e) => patch("thinking_mode", e.target.value)}
          >
            <option value="auto">Auto (enable for capable models)</option>
            <option value="on">Always On</option>
            <option value="off">Off</option>
          </select>
        </FormRow>
        <FormRow label="Answer Review" hint="Adversarial critic reviews answers for completeness, accuracy, and depth before delivery">
          <select
            style={selectFallback}
            value={s.review_mode ?? "off"}
            onChange={(e) => patch("review_mode", e.target.value)}
          >
            <option value="off">Off</option>
            <option value="auto">Auto (review factual/analytical questions only)</option>
            <option value="on">Always On (review every answer)</option>
          </select>
        </FormRow>
        <FormRow label={`Creativity: ${temp.toFixed(1)}`} hint="Higher = more creative; lower = more focused and consistent">
          <input
            type="range"
            min={0}
            max={2}
            step={0.1}
            value={temp}
            onChange={(e) => patch("llm_temperature", Number(e.target.value))}
            style={{ width: "100%" }}
          />
        </FormRow>
        <FormRow label="Response length" hint="Maximum length of each response">
          <select
            style={selectFallback}
            value={s.llm_max_tokens ?? 1024}
            onChange={(e) => patch("llm_max_tokens", Number(e.target.value))}
          >
            {maxTokenOpts.map((n) => <option key={n} value={n}>{n.toLocaleString()} tokens</option>)}
          </select>
        </FormRow>
      </Section>

      {pickerOpen && (
        <ModelPickerModal
          role="chat"
          currentProvider={s.chat_provider}
          currentModel={s.chat_model}
          onSelect={(provider, model) => {
            patch("chat_provider", provider);
            patch("chat_model", model);
            setPickerOpen(false);
          }}
          onClose={() => setPickerOpen(false)}
        />
      )}
    </div>
  );
}

function PromptsTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const [customEnabled, setCustomEnabled] = useState(!!s.custom_system_prompt);
  const addendum = s.prompt_addendum ?? "";
  const customPrompt = s.custom_system_prompt ?? "";

  return (
    <div className="settings-body">
      <Section title="Prompt Style">
        <RadioGroup
          aria-label="Prompt style"
          value={s.prompt_style ?? "balanced"}
          onChange={(v) => patch("prompt_style", v)}
        >
          <Radio value="balanced">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Balanced</div>
                <div className="settings-row__hint">Natural conversation, medium length responses</div>
              </div>
            </Radio.Content>
          </Radio>
          <Radio value="concise">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Concise</div>
                <div className="settings-row__hint">Short, direct answers. Minimal explanation</div>
              </div>
            </Radio.Content>
          </Radio>
          <Radio value="technical">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Technical</div>
                <div className="settings-row__hint">Precise, detailed. Favors accuracy over brevity</div>
              </div>
            </Radio.Content>
          </Radio>
          <Radio value="warm">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Warm</div>
                <div className="settings-row__hint">Friendly, encouraging tone. Conversational style</div>
              </div>
            </Radio.Content>
          </Radio>
        </RadioGroup>
      </Section>

      <Section title="Prompt Addendum">
        <FormRow label="Additional context" hint="Appended to every system prompt">
          <div style={{ position: "relative" }}>
            <textarea
              style={{ ...nativeInput, height: "80px", resize: "vertical" as const, width: "100%" }}
              value={addendum}
              maxLength={500}
              onChange={(e) => patch("prompt_addendum", e.target.value)}
              placeholder="Extra instructions appended to every request..."
            />
            <span style={charCounter}>{addendum.length}/500</span>
          </div>
        </FormRow>
      </Section>

      <Section title="Custom System Prompt">
        <FormRow label="Enable custom prompt">
          <Switch
            isSelected={customEnabled}
            onChange={(v) => {
              setCustomEnabled(v);
              if (!v) patch("custom_system_prompt", null);
            }}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </FormRow>
        <FormRow label="System prompt" hint="Replaces the built-in system prompt entirely">
          <div style={{ position: "relative" }}>
            <textarea
              style={{ ...nativeInput, height: "140px", resize: "vertical" as const, width: "100%", opacity: customEnabled ? 1 : 0.45 }}
              disabled={!customEnabled}
              value={customPrompt}
              maxLength={4000}
              onChange={(e) => patch("custom_system_prompt", e.target.value)}
              placeholder="You are a helpful AI assistant..."
            />
            {customEnabled && <span style={charCounter}>{customPrompt.length}/4000</span>}
          </div>
        </FormRow>
      </Section>
    </div>
  );
}

function LocationTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const enabled = s.weather_enabled ?? false;

  return (
    <div className="settings-body">
      <Section title="Weather">
        <FormRow label="Enable weather" hint="Allow the assistant to fetch current weather data">
          <Switch
            isSelected={enabled}
            onChange={(v) => patch("weather_enabled", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Enable weather
          </Switch>
        </FormRow>
        <FormRow label="Location name" hint="Human-readable name (e.g. Nairobi, Kenya)">
          <input
            style={{ ...nativeInput, opacity: enabled ? 1 : 0.45 }}
            disabled={!enabled}
            value={s.weather_location_name ?? ""}
            onChange={(e) => patch("weather_location_name", e.target.value)}
            placeholder="Nairobi, Kenya"
          />
        </FormRow>
        <FormRow label="Latitude">
          <input
            type="number"
            role="spinbutton"
            step={0.0001}
            style={{ ...nativeInput, opacity: enabled ? 1 : 0.45 }}
            disabled={!enabled}
            value={s.weather_latitude ?? ""}
            onChange={(e) => patch("weather_latitude", Number(e.target.value))}
            placeholder="-1.2921"
          />
        </FormRow>
        <FormRow label="Longitude">
          <input
            type="number"
            role="spinbutton"
            step={0.0001}
            style={{ ...nativeInput, opacity: enabled ? 1 : 0.45 }}
            disabled={!enabled}
            value={s.weather_longitude ?? ""}
            onChange={(e) => patch("weather_longitude", Number(e.target.value))}
            placeholder="36.8219"
          />
        </FormRow>
      </Section>
    </div>
  );
}

function AgentTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const memInject = s.agent_memory_inject ?? false;

  return (
    <div className="settings-body">
      <Section title="How thorough should Pond be?">
        <RadioGroup
          aria-label="Agent mode"
          value={s.agent_goose_mode ?? "auto"}
          onChange={(v) => patch("agent_goose_mode", v)}
        >
          <Radio value="auto">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Smart (recommended)</div>
                <div className="settings-row__hint">Pond decides when to look things up or take actions</div>
              </div>
            </Radio.Content>
          </Radio>
          <Radio value="chat">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Chat only</div>
                <div className="settings-row__hint">Conversation only -- Pond won't use any tools</div>
              </div>
            </Radio.Content>
          </Radio>
          <Radio value="smart">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Proactive</div>
                <div className="settings-row__hint">Pond actively uses tools to give more detailed answers</div>
              </div>
            </Radio.Content>
          </Radio>
        </RadioGroup>
      </Section>

      <Section title="Behaviour">
        <FormRow label="How thorough" hint="How many steps Pond will take to answer a question (1-50)">
          <input
            type="number"
            role="spinbutton"
            style={nativeInput}
            min={1}
            max={50}
            value={s.agent_max_turns ?? 20}
            onChange={(e) => patch("agent_max_turns", Number(e.target.value))}
          />
        </FormRow>
      </Section>

      <Section title="Memory">
        <FormRow label="Remember context" hint="Pond recalls facts from past conversations to give better answers">
          <Switch
            isSelected={memInject}
            onChange={(v) => patch("agent_memory_inject", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Use conversation memory
          </Switch>
        </FormRow>
        <FormRow label="How much to recall" hint="Number of past memories to include (1-20)">
          <input
            type="number"
            role="spinbutton"
            style={{ ...nativeInput, opacity: memInject ? 1 : 0.45 }}
            disabled={!memInject}
            min={1}
            max={20}
            value={s.agent_memory_limit ?? 5}
            onChange={(e) => patch("agent_memory_limit", Number(e.target.value))}
          />
        </FormRow>
      </Section>
    </div>
  );
}

function DataTab({
  s,
  patch,
  serverUrl,
  onServerUrlChange,
}: {
  s: Partial<SettingsType>;
  patch: (k: keyof SettingsType, v: unknown) => void;
  serverUrl: string;
  onServerUrlChange: (v: string) => void;
}) {
  return (
    <div className="settings-body">
      <Section title="Data Retention">
        <FormRow label="Event logs" hint="How many days to keep event log entries">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              role="spinbutton"
              style={{ ...nativeInput, width: "80px" }}
              min={1}
              max={365}
              value={s.retention_event_log_days ?? 30}
              onChange={(e) => patch("retention_event_log_days", Number(e.target.value))}
            />
            <span className="muted-12">days</span>
          </div>
        </FormRow>
        <FormRow label="Sensor readings" hint="How many days to keep sensor data">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              role="spinbutton"
              style={{ ...nativeInput, width: "80px" }}
              min={1}
              max={365}
              value={s.retention_sensor_days ?? 7}
              onChange={(e) => patch("retention_sensor_days", Number(e.target.value))}
            />
            <span className="muted-12">days</span>
          </div>
        </FormRow>
        <FormRow label="Session messages" hint="Maximum messages to keep per session">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              role="spinbutton"
              style={{ ...nativeInput, width: "100px" }}
              min={10}
              max={10000}
              value={s.retention_session_messages_keep ?? 500}
              onChange={(e) => patch("retention_session_messages_keep", Number(e.target.value))}
            />
            <span className="muted-12">messages</span>
          </div>
        </FormRow>
      </Section>

      <Section title="Desktop">
        <FormRow label="Server URL" hint="pond-server base URL">
          <input
            style={nativeInput}
            value={serverUrl}
            onChange={(e) => onServerUrlChange(e.target.value)}
            placeholder="http://127.0.0.1:4000"
          />
        </FormRow>
      </Section>
    </div>
  );
}

// ── Main Settings Component ───────────────────────────────────

export function Settings() {
  const state    = useAppState();
  const dispatch = useAppDispatch();
  const [tab, setTab]           = useState<SettingsTab>("identity");
  const [settings, setSettings] = useState<Partial<SettingsType>>({});
  const [loading, setLoading]   = useState(true);
  const [saving, setSaving]     = useState(false);
  const [saved, setSaved]       = useState(false);
  const [error, setError]       = useState<string | null>(null);
  const [hotkey, setHotkey]     = useState("CmdOrCtrl+Shift+V");

  useEffect(() => {
    api.getSettings()
      .then((s) => setSettings(s))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  async function save() {
    setSaving(true);
    setSaved(false);
    setError(null);
    try {
      const updated = await api.updateSettings(settings);
      setSettings(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) { setError(String(e)); }
    finally { setSaving(false); }
  }

  async function applyHotkey() {
    const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    if (!isTauri) return;
    try { await invoke("set_hotkey", { hotkey }); }
    catch (e) { setError(String(e)); }
  }

  function patch(key: keyof SettingsType, value: unknown) {
    setSettings((prev) => ({ ...prev, [key]: value }));
  }

  return (
    <div className="screen" style={{ height: "100%", display: "flex", flexDirection: "column" }}>
      {/* Page header */}
      <div className="page-header">
        <h2 className="page-header__title">Settings</h2>
        <div className="page-header__action">
          <Button variant="primary" onPress={save} isDisabled={saving || loading}>
            {saving ? "Saving..." : saved ? "Saved" : "Save Settings"}
          </Button>
        </div>
      </div>

      {/* Tab bar */}
      <Tabs
        selectedKey={tab}
        onSelectionChange={(k) => setTab(k as SettingsTab)}
      >
        <Tabs.ListContainer>
          <Tabs.List aria-label="Settings sections" className="settings-tabs">
            {TABS.map((t) => (
              <Tabs.Tab key={t.id} id={t.id} onClick={() => setTab(t.id)}>
                <Tabs.Indicator />
                {t.label}
              </Tabs.Tab>
            ))}
          </Tabs.List>
        </Tabs.ListContainer>
      </Tabs>

      {/* Error banner */}
      {error && <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0, flexShrink: 0 }}>{error}</p>}

      {/* Panel area */}
      <div style={{ flex: 1, overflowY: "auto", marginTop: 4 }}>
        {loading ? (
          <p className="muted-12">Loading settings...</p>
        ) : (
          <>
            {tab === "identity"  && <IdentityTab  s={settings} patch={patch} />}
            {tab === "voice"     && <VoiceTab s={settings} patch={patch} hotkey={hotkey} setHotkey={setHotkey} applyHotkey={applyHotkey} refreshSettings={async () => { try { const u = await api.getSettings(); setSettings(u); } catch { /* ignore */ } }} />}
            {tab === "models"    && <ModelsTab    s={settings} patch={patch} />}
            {tab === "prompts"   && <PromptsTab   s={settings} patch={patch} />}
            {tab === "location"  && <LocationTab  s={settings} patch={patch} />}
            {tab === "agent"     && <AgentTab     s={settings} patch={patch} />}
            {tab === "data"      && <DataTab s={settings} patch={patch} serverUrl={state.serverUrl} onServerUrlChange={(v) => dispatch({ type: "SET_SERVER_URL", payload: v })} />}
            {tab === "tools"     && <ToolsTab />}
          </>
        )}
      </div>
    </div>
  );
}

// ── Tools Tab ─────────────────────────────────────────────────

function ToolsTab() {
  const [extensions, setExtensions] = useState<Extension[]>([]);
  const [loading, setLoading]       = useState(true);
  const [error, setError]           = useState<string | null>(null);
  const [showForm, setShowForm]     = useState(false);
  const [addName, setAddName]       = useState("");
  const [addKind, setAddKind]       = useState<"stdio" | "sse">("stdio");
  const [addCmd, setAddCmd]         = useState("");
  const [adding, setAdding]         = useState(false);

  function load() {
    setLoading(true);
    api.listExtensions()
      .then((r) => setExtensions(r.extensions))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  async function toggle(name: string, enabled: boolean) {
    try {
      await api.toggleExtension(name, enabled);
      setExtensions((prev) => prev.map((e) => e.name === name ? { ...e, enabled } : e));
    } catch (e) { setError(String(e)); }
  }

  async function remove(name: string) {
    try { await api.removeExtension(name); load(); } catch (e) { setError(String(e)); }
  }

  async function add() {
    if (!addName.trim() || !addCmd.trim()) return;
    setAdding(true);
    try {
      await api.addExtension({ name: addName.trim(), kind: addKind, command: addCmd.trim() });
      setAddName(""); setAddCmd(""); setShowForm(false); load();
    } catch (e) { setError(String(e)); } finally { setAdding(false); }
  }

  return (
    <div className="settings-body">
      <div style={{ display: "flex", gap: "var(--space-2)" }}>
        <Button variant="outline" onPress={() => setShowForm((v) => !v)}>
          <Plus size={14} /> Add Extension
        </Button>
      </div>

      {showForm && (
        <Card shadow="none" className="giap-card">
          <CardContent style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
            <input
              style={nativeInput}
              value={addName}
              onChange={(e) => setAddName(e.target.value)}
              placeholder="Name (e.g. developer)"
              aria-label="Extension name"
            />
            <select
              style={selectFallback}
              value={addKind}
              onChange={(e) => setAddKind(e.target.value as "stdio" | "sse")}
            >
              <option value="stdio">stdio</option>
              <option value="sse">SSE</option>
            </select>
            <input
              style={nativeInput}
              value={addCmd}
              onChange={(e) => setAddCmd(e.target.value)}
              placeholder="Command or URI"
              aria-label="Extension command or URI"
            />
            <Button variant="primary" onPress={add} isDisabled={adding || !addName.trim() || !addCmd.trim()}>
              {adding ? "Adding..." : "Add"}
            </Button>
          </CardContent>
        </Card>
      )}

      {error && <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 }}>{error}</p>}

      {loading ? (
        <p className="muted-12">Loading extensions...</p>
      ) : extensions.length === 0 ? (
        <p className="muted-12">No extensions configured. Add one above to enable tool use.</p>
      ) : (
        <div className="ext-list">
          {extensions.map((ext) => (
            <div key={ext.name} className="ext-row">
              <Switch isSelected={ext.enabled} onChange={() => toggle(ext.name, !ext.enabled)} aria-label={`Toggle ${ext.name}`}><Switch.Control><Switch.Thumb /></Switch.Control></Switch>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)", flexWrap: "wrap" as const }}>
                  <span className="ext-row__name">{ext.name}</span>
                  <Chip size="sm" variant="soft">{ext.kind}</Chip>
                  {!ext.enabled && <Chip size="sm" variant="soft" color="warning">Disabled</Chip>}
                </div>
                {ext.description && <p style={{ margin: "2px 0 4px", fontSize: "var(--text-xs)", color: "var(--grey-500)" }}>{ext.description}</p>}
                {ext.tools.length > 0 && (
                  <div style={{ display: "flex", flexWrap: "wrap" as const, gap: "4px", marginTop: "4px" }}>
                    {ext.tools.slice(0, 8).map((t) => (
                      <code key={t} style={{ fontSize: "11px", background: "var(--grey-50)", border: "1px solid var(--grey-200)", borderRadius: "4px", padding: "1px 5px", fontFamily: "var(--font-mono)" }}>
                        {t.replace(`${ext.name}__`, "")}
                      </code>
                    ))}
                    {ext.tools.length > 8 && <span className="muted-12">+{ext.tools.length - 8} more</span>}
                  </div>
                )}
              </div>
              <Button variant="danger-soft" onPress={() => remove(ext.name)} aria-label={`Remove ${ext.name}`}>
                <Trash2 size={14} />
              </Button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ── Styles ────────────────────────────────────────────────────

/**
 * Native <input> and <select> styling for cases where HeroUI components
 * don't fit (e.g. type="number" needing role="spinbutton" for tests,
 * <select> with <option> children).
 */
const nativeInput: React.CSSProperties = {
  height: "36px",
  border: "1px solid var(--grey-200)",
  borderRadius: "var(--radius-card)",
  padding: "0 var(--space-3)",
  fontSize: "var(--text-base)",
  fontFamily: "var(--font-body)",
  background: "#fff",
  color: "var(--fg)",
  width: "100%",
  userSelect: "text",
};

const selectFallback: React.CSSProperties = {
  ...nativeInput,
  cursor: "pointer",
};

const charCounter: React.CSSProperties = {
  position: "absolute",
  bottom: "6px",
  right: "8px",
  fontSize: "var(--text-xs)",
  color: "var(--grey-500)",
  pointerEvents: "none",
};

const wakeWordNote: React.CSSProperties = {
  display: "flex",
  alignItems: "flex-start",
  gap: "6px",
  padding: "8px 10px",
  background: "rgba(255,149,0,0.08)",
  border: "1px solid rgba(255,149,0,0.25)",
  borderRadius: "8px",
  fontSize: "var(--text-xs)",
  color: "var(--grey-600)",
  lineHeight: "1.5",
};

const wakeWordNoteIcon: React.CSSProperties = {
  color: "#FF9500",
  flexShrink: 0,
  marginTop: "1px",
  fontWeight: 700,
  fontStyle: "italic",
};

const calibrationRow: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-2)",
  padding: "6px 0",
};

const calibrationDot: React.CSSProperties = {
  width: "8px",
  height: "8px",
  borderRadius: "50%",
  flexShrink: 0,
};

const calibrationLabel: React.CSSProperties = {
  fontSize: "var(--text-sm)",
  fontFamily: "var(--font-body)",
  color: "var(--grey-600)",
};

const advancedToggleStyle: React.CSSProperties = {
  background: "none",
  border: "none",
  cursor: "pointer",
  fontSize: "var(--text-sm)",
  color: "var(--grey-600)",
  padding: "0",
  textAlign: "left",
  fontFamily: "var(--font-body)",
  display: "flex",
  alignItems: "center",
  gap: "var(--space-1)",
};
