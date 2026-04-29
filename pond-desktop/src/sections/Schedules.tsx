import { useState, useEffect } from "react";
import {
  Card,
  CardContent,
  Button,
  Switch,
  Chip,
  Separator,
} from "@heroui/react";
import { Plus, Trash2, Play, Pencil, CalendarClock } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState } from "../state/AppContext";
import type { Schedule } from "../api/types";

/* ── Frequency presets for the create form ─────────────────── */
const FREQ_PRESETS: Record<string, string> = {
  "Every minute":  "0 * * * * *",
  "Hourly":        "0 0 * * * *",
  "Daily (8 AM)":  "0 0 8 * * *",
  "Weekly (Mon)":  "0 0 8 * * 1",
  "Custom":        "",
};

/* ── Recipe / prompt presets ───────────────────────────────── */
const RECIPE_PRESETS: Record<string, string> = {
  "Morning briefing":     "Give me a morning briefing: weather, calendar, and top news.",
  "Daily summary":        "Summarize today's key events and tasks.",
  "Sensor check":         "Check all sensor readings and report any anomalies.",
  "Custom":               "",
};

export function Schedules() {
  const state = useAppState();
  const [schedules, setSchedules]     = useState<Schedule[]>([]);
  const [loading, setLoading]         = useState(true);
  const [error, setError]             = useState<string | null>(null);
  const [actionMsg, setActionMsg]     = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  // New schedule form
  const [showForm, setShowForm]       = useState(false);
  const [name, setName]               = useState("");
  const [cron, setCron]               = useState("");
  const [prompt, setPrompt]           = useState("");
  const [freqKey, setFreqKey]         = useState("Custom");
  const [recipeKey, setRecipeKey]     = useState("Custom");
  const [submitting, setSubmitting]   = useState(false);

  function load() {
    setLoading(true);
    api
      .listSchedules()
      .then(setSchedules)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => {
    load();
  }, []);

  function flashMsg(msg: string, isError = false) {
    if (isError) {
      setActionError(msg);
      setTimeout(() => setActionError(null), 4000);
    } else {
      setActionMsg(msg);
      setTimeout(() => setActionMsg(null), 3000);
    }
  }

  async function handleCreate() {
    if (!name.trim() || !cron.trim() || !prompt.trim()) return;
    setSubmitting(true);
    try {
      await api.createSchedule({
        name: name.trim(),
        cron: cron.trim(),
        prompt: prompt.trim(),
        enabled: true,
      });
      setName("");
      setCron("");
      setPrompt("");
      setFreqKey("Custom");
      setRecipeKey("Custom");
      setShowForm(false);
      flashMsg("Schedule created.");
      load();
    } catch (e) {
      flashMsg(String(e), true);
    } finally {
      setSubmitting(false);
    }
  }

  async function handleDelete(id: string, schedName: string) {
    if (!confirm(`Delete schedule "${schedName}"?`)) return;
    try {
      await api.deleteSchedule(id);
      flashMsg("Schedule deleted.");
      load();
    } catch (e) {
      flashMsg(String(e), true);
    }
  }

  async function handleToggle(s: Schedule) {
    try {
      if (s.enabled) {
        await api.pauseSchedule(s.id);
        flashMsg(`"${s.name}" paused.`);
      } else {
        await api.resumeSchedule(s.id);
        flashMsg(`"${s.name}" resumed.`);
      }
      load();
    } catch (e) {
      flashMsg(String(e), true);
    }
  }

  async function handleRunNow(s: Schedule) {
    try {
      await api.runScheduleNow(s.id);
      flashMsg(`"${s.name}" triggered.`);
    } catch (e) {
      flashMsg(String(e), true);
    }
  }

  function handleFreqChange(key: string) {
    setFreqKey(key);
    const preset = FREQ_PRESETS[key];
    if (preset) setCron(preset);
  }

  function handleRecipeChange(key: string) {
    setRecipeKey(key);
    const preset = RECIPE_PRESETS[key];
    if (preset) setPrompt(preset);
  }

  /* ── Create-form modal overlay ──────────────────────────── */
  function renderModal() {
    if (!showForm) return null;
    return (
      <div style={modalStyles.overlay} onClick={() => setShowForm(false)}>
        <div style={modalStyles.dialog} onClick={(e) => e.stopPropagation()}>
          {/* Header */}
          <div style={modalStyles.header}>
            <h2 style={modalStyles.title}>New Schedule</h2>
            <button
              style={modalStyles.closeBtn}
              onClick={() => setShowForm(false)}
              aria-label="Close"
            >
              x
            </button>
          </div>
          <Separator />

          {/* Body */}
          <div style={modalStyles.body}>
            <div style={modalStyles.fieldGroup}>
              <label style={modalStyles.label}>Name</label>
              <input
                style={modalStyles.input}
                placeholder="Daily briefing"
                aria-label="Schedule name"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </div>

            {/* Frequency select */}
            <div style={modalStyles.fieldGroup}>
              <label style={modalStyles.label}>Frequency</label>
              <select
                style={modalStyles.select}
                value={freqKey}
                onChange={(e) => handleFreqChange(e.target.value)}
                aria-label="Schedule frequency"
              >
                {Object.keys(FREQ_PRESETS).map((k) => (
                  <option key={k} value={k}>
                    {k}
                  </option>
                ))}
              </select>
            </div>

            <div style={modalStyles.fieldGroup}>
              <label style={modalStyles.label}>
                Cron expression
                <span style={{ fontSize: "11px", color: "var(--grey-500)", fontWeight: 400 }}>
                  {" "}-- sec min hr dom mon dow
                </span>
              </label>
              <input
                style={modalStyles.input}
                placeholder="0 0 8 * * *"
                aria-label="Cron expression"
                value={cron}
                onChange={(e) => setCron(e.target.value)}
                spellCheck={false}
              />
            </div>

            {/* Recipe select */}
            <div style={modalStyles.fieldGroup}>
              <label style={modalStyles.label}>Recipe</label>
              <select
                style={modalStyles.select}
                value={recipeKey}
                onChange={(e) => handleRecipeChange(e.target.value)}
                aria-label="Schedule recipe"
              >
                {Object.keys(RECIPE_PRESETS).map((k) => (
                  <option key={k} value={k}>
                    {k}
                  </option>
                ))}
              </select>
            </div>

            <div style={modalStyles.fieldGroup}>
              <label style={modalStyles.label}>Prompt</label>
              <textarea
                style={modalStyles.textarea}
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                placeholder="Give me a morning briefing: weather, calendar, and top news."
                aria-label="Schedule prompt"
                rows={3}
              />
            </div>
          </div>

          <Separator />

          {/* Footer */}
          <div style={modalStyles.footer}>
            <Button size="sm" variant="light" onPress={() => setShowForm(false)}>
              Cancel
            </Button>
            <Button
              size="sm"
              color="secondary"
              onPress={handleCreate}
              isDisabled={submitting || !name.trim() || !cron.trim() || !prompt.trim()}
            >
              {submitting ? "Creating..." : "Create Schedule"}
            </Button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="screen">
      {/* ── Page header ─────────────────────────────────────── */}
      <div className="page-header">
        <h1 className="page-header__title">Schedules</h1>
        <div className="page-header__action">
          <Button
            size="sm"
            color="secondary"
            isDisabled={!state.serverOnline}
            onPress={() => setShowForm(true)}
            startContent={<Plus size={14} />}
          >
            New Schedule
          </Button>
        </div>
      </div>

      {/* ── Feedback messages ───────────────────────────────── */}
      {actionMsg && (
        <p style={{ color: "var(--color-success)", fontSize: "var(--text-sm)", margin: 0 }}>
          {actionMsg}
        </p>
      )}
      {actionError && (
        <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 }}>
          {actionError}
        </p>
      )}
      {error && (
        <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 }}>
          {error}
        </p>
      )}

      {/* ── Schedule grid ───────────────────────────────────── */}
      {loading ? (
        <p className="muted-12">Loading...</p>
      ) : schedules.length === 0 ? (
        <div className="empty-state">
          <CalendarClock size={32} />
          <span>No schedules yet. Create one to automate recurring tasks.</span>
        </div>
      ) : (
        <div className="sched-grid">
          {schedules.map((s) => (
            <Card key={s.id} shadow="none" className="giap-card">
              <CardContent>
                {/* Head: icon + name + cron + toggle */}
                <div className="sched-card__head">
                  <div className="sched-card__icon">
                    <CalendarClock size={18} />
                  </div>
                  <div className="sched-card__main">
                    <div className="sched-card__name">{s.name}</div>
                    <code style={cronStyle}>{s.cron}</code>
                  </div>
                  <Switch
                    size="sm"
                    color="success"
                    isSelected={s.enabled}
                    onValueChange={() => handleToggle(s)}
                    isDisabled={!state.serverOnline}
                    aria-label={s.enabled ? "Pause schedule" : "Resume schedule"}
                  >
                    <Switch.Control><Switch.Thumb /></Switch.Control>
                  </Switch>
                </div>

                {/* Meta: recipe + next run */}
                <div className="sched-card__meta">
                  <div>
                    <span className="muted-12">Recipe</span>
                    <p style={{ margin: "2px 0 0", fontSize: "var(--text-sm)" }}>
                      {s.prompt
                        ? s.prompt.length > 80
                          ? s.prompt.slice(0, 80) + "..."
                          : s.prompt
                        : "--"}
                    </p>
                  </div>
                  <div>
                    <span className="muted-12">Status</span>
                    <p style={{ margin: "2px 0 0" }}>
                      <Chip
                        size="sm"
                        variant="flat"
                        color={s.enabled ? "success" : "default"}
                      >
                        {s.enabled ? "Active" : "Paused"}
                      </Chip>
                    </p>
                  </div>
                </div>

                {/* Actions */}
                <div className="sched-card__actions">
                  <Button
                    size="sm"
                    variant="light"
                    onPress={() => handleRunNow(s)}
                    isDisabled={!state.serverOnline}
                    aria-label="Run now"
                    startContent={<Play size={12} />}
                  >
                    Run now
                  </Button>
                  <Button
                    size="sm"
                    variant="light"
                    isDisabled={!state.serverOnline}
                    aria-label="Edit schedule"
                    startContent={<Pencil size={12} />}
                  >
                    Edit
                  </Button>
                  <Button
                    size="sm"
                    variant="light"
                    color="danger"
                    className="sched-card__trash"
                    onPress={() => handleDelete(s.id, s.name)}
                    isDisabled={!state.serverOnline}
                    aria-label="Delete schedule"
                    startContent={<Trash2 size={12} />}
                  >
                    Delete
                  </Button>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      {/* ── Create modal ────────────────────────────────────── */}
      {renderModal()}
    </div>
  );
}

/* ── Inline cron code style ────────────────────────────────── */
const cronStyle: React.CSSProperties = {
  fontFamily: "var(--font-mono)",
  fontSize: "11px",
  color: "var(--grey-600)",
  background: "var(--grey-100)",
  padding: "2px 8px",
  borderRadius: "4px",
  display: "inline-block",
  marginTop: "2px",
};

/* ── Modal styles (matching project pattern) ───────────────── */
const modalStyles: Record<string, React.CSSProperties> = {
  overlay: {
    position: "fixed",
    inset: 0,
    background: "rgba(23, 22, 22, 0.45)",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    zIndex: 1000,
  },
  dialog: {
    background: "#fff",
    borderRadius: "var(--radius-card)",
    boxShadow: "0 12px 40px rgba(28,28,28,0.15)",
    width: "500px",
    maxWidth: "calc(100vw - 32px)",
    maxHeight: "80vh",
    display: "flex",
    flexDirection: "column",
    overflow: "hidden",
  },
  header: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "16px 20px 12px",
    flexShrink: 0,
  },
  title: {
    fontFamily: "var(--font-heading)",
    fontWeight: 700,
    fontSize: "16px",
    margin: 0,
  },
  closeBtn: {
    background: "none",
    border: "none",
    cursor: "pointer",
    fontSize: "var(--text-sm)",
    color: "var(--grey-500)",
    padding: "4px",
    lineHeight: 1,
  },
  body: {
    flex: 1,
    overflowY: "auto",
    padding: "16px 20px",
    display: "flex",
    flexDirection: "column",
    gap: "14px",
  },
  fieldGroup: {
    display: "flex",
    flexDirection: "column",
    gap: "4px",
  },
  label: {
    fontSize: "var(--text-sm)",
    fontWeight: 500,
    color: "var(--grey-700)",
  },
  input: {
    height: "36px",
    border: "1px solid var(--grey-300)",
    borderRadius: "8px",
    padding: "0 10px",
    fontSize: "var(--text-base)",
    fontFamily: "var(--font-body)",
    background: "#fff",
    color: "var(--fg)",
    width: "100%",
    boxSizing: "border-box" as React.CSSProperties["boxSizing"],
    outline: "none",
  },
  select: {
    height: "36px",
    border: "1px solid var(--grey-300)",
    borderRadius: "8px",
    padding: "0 10px",
    fontSize: "var(--text-base)",
    fontFamily: "var(--font-body)",
    background: "#fff",
    color: "var(--fg)",
    cursor: "pointer",
    appearance: "auto" as React.CSSProperties["appearance"],
  },
  textarea: {
    border: "1px solid var(--grey-300)",
    borderRadius: "8px",
    padding: "8px 10px",
    fontSize: "var(--text-base)",
    fontFamily: "var(--font-body)",
    background: "#fff",
    color: "var(--fg)",
    resize: "vertical" as React.CSSProperties["resize"],
    minHeight: "64px",
  },
  footer: {
    display: "flex",
    justifyContent: "flex-end",
    gap: "8px",
    padding: "12px 20px",
    flexShrink: 0,
  },
};
