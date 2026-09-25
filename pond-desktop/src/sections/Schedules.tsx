import { useState, useEffect, useCallback, useMemo } from "react";
import { Button, Separator } from "@heroui/react";
import { Plus, Trash2, Play, Pencil, CalendarClock, ChevronDown, ChevronUp, Clock, List, Calendar, Repeat } from "lucide-react";
import { api } from "../api/PondApiClient";
import { PageHeader, useConfirm, SkeletonList } from "../components/shared";
import { useAppState, useAppDispatch } from "../state/AppContext";
import type { Schedule, ScheduleRun, ChatEvent } from "../api/types";
import { ApiError } from "../api/types";
import { ScheduleCalendar } from "./ScheduleCalendar";
import { NewRoutineModal } from "./NewRoutineModal";
import { useRoutines } from "../hub/state/hubDataStore";
import { HubIco } from "../hub/primitives/HubIco";
import { HP_PATHS } from "../hub/primitives/icons";
import { type RoutineId, ROUTINES } from "../hub/data/routines";
import "../hub/views/routines.css";
import { ZonePicker } from "../components/ZonePicker";

/* ── Repeat patterns ──────────────────────────────────────── */
type RepeatPattern = "once" | "hourly" | "daily" | "weekly" | "monthly" | "custom";

const REPEAT_PATTERNS: { key: RepeatPattern; label: string }[] = [
  { key: "once",    label: "Once"    },
  { key: "hourly",  label: "Hourly"  },
  { key: "daily",   label: "Daily"   },
  { key: "weekly",  label: "Weekly"  },
  { key: "monthly", label: "Monthly" },
  { key: "custom",  label: "Custom"  },
];

const DAY_LABELS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
// cron day-of-week: 1=Mon … 7=Sun (or 0=Sun in some engines; use 1-7)
const DAY_VALUES = [1, 2, 3, 4, 5, 6, 7];

interface ScheduleConfig {
  // hourly
  everyNHours: number;
  startMinute: number;
  // daily / weekly / monthly
  hour: number;
  minute: number;
  // weekly
  daysOfWeek: number[];
  // monthly
  dayOfMonth: number;
  // custom
  customCron: string;
}

function defaultConfig(): ScheduleConfig {
  return {
    everyNHours: 1,
    startMinute: 0,
    hour: 8,
    minute: 0,
    daysOfWeek: [1],   // Monday
    dayOfMonth: 1,
    customCron: "0 0 8 * * *",
  };
}

function buildCron(repeat: RepeatPattern, cfg: ScheduleConfig): string {
  switch (repeat) {
    case "once":
      // A daily cron at that time; `once: true` makes the server fire only its next occurrence.
      return `0 ${cfg.minute} ${cfg.hour} * * *`;
    case "hourly":
      return `0 ${cfg.startMinute} */${cfg.everyNHours} * * *`;
    case "daily":
      return `0 ${cfg.minute} ${cfg.hour} * * *`;
    case "weekly": {
      const days = cfg.daysOfWeek.length ? cfg.daysOfWeek.join(",") : "1";
      return `0 ${cfg.minute} ${cfg.hour} * * ${days}`;
    }
    case "monthly":
      return `0 ${cfg.minute} ${cfg.hour} ${cfg.dayOfMonth} * *`;
    default:
      return cfg.customCron;
  }
}

function ordinal(n: number): string {
  const s = ["th", "st", "nd", "rd"];
  const v = n % 100;
  return n + (s[(v - 20) % 10] || s[v] || s[0]);
}

function humanPreview(repeat: RepeatPattern, cfg: ScheduleConfig, timezone: string): string {
  const timeStr = `${String(cfg.hour).padStart(2, "0")}:${String(cfg.minute).padStart(2, "0")}`;
  const tz = timezone !== "UTC" ? ` (${timezone})` : "";
  switch (repeat) {
    case "once":
      return `Runs once at ${timeStr}${tz}`;
    case "hourly":
      return `Runs every ${cfg.everyNHours === 1 ? "hour" : `${cfg.everyNHours} hours`} at :${String(cfg.startMinute).padStart(2, "0")}${tz}`;
    case "daily":
      return `Runs every day at ${timeStr}${tz}`;
    case "weekly": {
      const days = cfg.daysOfWeek
        .slice()
        .sort((a, b) => a - b)
        .map((d) => DAY_LABELS[d - 1])
        .join(", ");
      return `Runs every ${days || "Mon"} at ${timeStr}${tz}`;
    }
    case "monthly":
      return `Runs on the ${ordinal(cfg.dayOfMonth)} of every month at ${timeStr}${tz}`;
    case "custom":
      return cfg.customCron ? `Cron: ${cfg.customCron}${tz}` : "Enter a cron expression above";
    default:
      return "";
  }
}

function timeAgo(dateStr: string): string {
  const diff = Date.now() - new Date(dateStr).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

function parseCronToConfig(cron: string): { repeat: RepeatPattern; config: ScheduleConfig } {
  const cfg = defaultConfig();
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 6) return { repeat: "custom", config: { ...cfg, customCron: cron } };

  const [_sec, min, hour, dom, _month, dow] = parts;

  // Hourly: "0 M */N * * *"
  if (hour.startsWith("*/") && dom === "*" && dow === "*") {
    cfg.everyNHours = parseInt(hour.slice(2)) || 1;
    cfg.startMinute = parseInt(min) || 0;
    return { repeat: "hourly", config: cfg };
  }

  const h = parseInt(hour);
  const m = parseInt(min);
  if (isNaN(h) || isNaN(m)) return { repeat: "custom", config: { ...cfg, customCron: cron } };
  cfg.hour = h;
  cfg.minute = m;

  // Monthly: "0 M H D * *"
  if (dom !== "*" && dow === "*") {
    cfg.dayOfMonth = parseInt(dom) || 1;
    return { repeat: "monthly", config: cfg };
  }

  // Weekly: "0 M H * * d,d,d"
  if (dom === "*" && dow !== "*") {
    cfg.daysOfWeek = dow.split(",").map((d) => parseInt(d)).filter((d) => !isNaN(d));
    if (cfg.daysOfWeek.length === 0) cfg.daysOfWeek = [1];
    return { repeat: "weekly", config: cfg };
  }

  // Daily: "0 M H * * *"
  if (dom === "*" && dow === "*") {
    return { repeat: "daily", config: cfg };
  }

  return { repeat: "custom", config: { ...cfg, customCron: cron } };
}

/** Picker state for a schedule; one-shots come from `fire_at`, as their cron is the "@once" sentinel. */
function scheduleToConfig(s: Schedule): { repeat: RepeatPattern; config: ScheduleConfig } {
  if (!s.fire_at) return parseCronToConfig(s.cron);
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone: s.timezone || "UTC",
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
  }).formatToParts(new Date(s.fire_at));
  const hour = parseInt(parts.find((p) => p.type === "hour")?.value ?? "0", 10) % 24;
  const minute = parseInt(parts.find((p) => p.type === "minute")?.value ?? "0", 10);
  return { repeat: "once", config: { ...defaultConfig(), hour, minute } };
}

/* ── Recipe / prompt presets ───────────────────────────────── */
const RECIPE_PRESETS: Record<string, string> = {
  "Morning briefing":     "Give me a morning briefing: weather, calendar, and top news.",
  "Daily summary":        "Summarize today's key events and tasks.",
  "Sensor check":         "Check all sensor readings and report any anomalies.",
  "Custom":               "",
};

export function Schedules() {
  const state = useAppState();
  const dispatch = useAppDispatch();
  const SCHED_PAGE = 10;
  const confirm = useConfirm();

  // Routines (one-tap macros from hub)
  const routines = useRoutines();
  const [runningRoutine, setRunningRoutine] = useState<RoutineId | null>(null);

  function handleRunRoutine(id: RoutineId, name: string) {
    setRunningRoutine(id);
    const startedAt = new Date().toISOString();
    const t0 = Date.now();
    dispatch({
      type: "SCHEDULE_RESULT",
      payload: { id: `routine-${id}`, schedule_id: id, schedule_label: name, status: "running", timestamp: t0 },
    });
    void (async () => {
      async function drain(gen: AsyncGenerator<ChatEvent>): Promise<string> {
        let text = "";
        for await (const ev of gen) {
          if (ev.type === "text") text += ev.content ?? ev.token ?? "";
        }
        return text.trim();
      }
      try {
        const detail = ROUTINES.find((r) => r.id === id);
        // Run the recipe in the background; seed it first if it doesn't exist yet
        try {
          await drain(api.runRecipe(name));
        } catch (e) {
          if (e instanceof ApiError && e.status === 404) {
            if (detail) {
              await api.createRecipe({
                name,
                description: detail.does.join(", "),
                yaml: `prompt: "${detail.prompt.replace(/"/g, "'")}"`,
              });
              await drain(api.runRecipe(name));
            } else throw e;
          } else throw e;
        }
        // Shows the routine's does[]: the LLM output is discarded until MCP tool integrations exist.
        const result = detail ? JSON.stringify(detail.does) : name;
        dispatch({
          type: "SCHEDULE_RESULT",
          payload: { id: `routine-${id}`, schedule_id: id, schedule_label: name, status: "completed", result, timestamp: Date.now() },
        });
        dispatch({
          type: "ADD_SCHEDULE_RUN",
          payload: {
            id: `routine-${id}-${t0}`,
            scheduleId: id,
            scheduleName: name,
            status: "completed",
            result,
            error: null,
            startedAt,
            finishedAt: new Date().toISOString(),
            durationMs: Date.now() - t0,
            read: false,
            excerpt: result.slice(0, 80),
            recipe: "routine",
          },
        });
      } catch (e) {
        const error = String(e);
        dispatch({
          type: "SCHEDULE_RESULT",
          payload: { id: `routine-${id}`, schedule_id: id, schedule_label: name, status: "failed", error, timestamp: Date.now() },
        });
        dispatch({
          type: "ADD_SCHEDULE_RUN",
          payload: {
            id: `routine-${id}-${t0}`,
            scheduleId: id,
            scheduleName: name,
            status: "failed",
            result: null,
            error,
            startedAt,
            finishedAt: new Date().toISOString(),
            durationMs: Date.now() - t0,
            read: false,
            excerpt: error.slice(0, 80),
            recipe: "routine",
          },
        });
      } finally {
        setRunningRoutine(null);
      }
    })();
  }
  const [schedules, setSchedules]         = useState<Schedule[]>([]);
  const [visibleCount, setVisibleCount]   = useState(SCHED_PAGE);
  const [loading, setLoading]             = useState(true);
  const [error, setError]                 = useState<string | null>(null);
  const [actionMsg, setActionMsg]         = useState<string | null>(null);
  const [actionError, setActionError]     = useState<string | null>(null);
  const [editingSchedule, setEditingSchedule] = useState<Schedule | null>(null);
  const [runningIds, setRunningIds]       = useState<Set<string>>(new Set());
  const [expandedRunId, setExpandedRunId] = useState<string | null>(null);

  const [view, setView] = useState<"list" | "calendar">(() => {
    try {
      return (localStorage.getItem("schedules-view") as "list" | "calendar") || "list";
    } catch {
      return "list";
    }
  });

  function setViewPersisted(v: "list" | "calendar") {
    try { localStorage.setItem("schedules-view", v); } catch { /* ignore */ }
    setView(v);
  }

  const [showRoutineForm, setShowRoutineForm] = useState(false);

  // New schedule form
  const [showForm, setShowForm]       = useState(false);
  const [name, setName]               = useState("");
  const [cron, setCron]               = useState("");
  const [prompt, setPrompt]           = useState("");
  const [timezone, setTimezone]       = useState("UTC");
  const [freqKey, setFreqKey]         = useState("Custom");   // kept for compat
  const [recipeKey, setRecipeKey]     = useState("Custom");
  const [submitting, setSubmitting]   = useState(false);

  // Human-friendly repeat picker state
  const [repeat, setRepeat]           = useState<RepeatPattern>("daily");
  const [schedCfg, setSchedCfg]       = useState<ScheduleConfig>(defaultConfig());

  // Run history per schedule (expanded state + cached runs)
  const [expandedRuns, setExpandedRuns] = useState<string | null>(null);
  const [runsCache, setRunsCache]       = useState<Record<string, ScheduleRun[]>>({});

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

  // Track running schedules and refresh on completion via global SSE listener.
  const latestScheduleResult = state.latestScheduleResult;
  useEffect(() => {
    if (!latestScheduleResult) return;
    if (latestScheduleResult.status === "running") {
      setRunningIds((prev) => {
        const next = new Set(prev);
        next.add(latestScheduleResult.schedule_id);
        return next;
      });
    } else {
      setRunningIds((prev) => {
        const next = new Set(prev);
        next.delete(latestScheduleResult.schedule_id);
        return next;
      });
      // Refresh schedule list when a run completes to update last_run
      load();
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [latestScheduleResult]);

  function flashMsg(msg: string, isError = false) {
    if (isError) {
      setActionError(msg);
      setTimeout(() => setActionError(null), 4000);
    } else {
      setActionMsg(msg);
      setTimeout(() => setActionMsg(null), 3000);
    }
  }

  const pickerCron = useMemo(() => buildCron(repeat, schedCfg), [repeat, schedCfg]);

  // Mirror the picker into the legacy `cron` state the API calls read.
  useEffect(() => {
    setCron(pickerCron);
  }, [pickerCron]);

  function updateCfg(patch: Partial<ScheduleConfig>) {
    setSchedCfg((prev) => ({ ...prev, ...patch }));
  }

  function toggleDay(d: number) {
    setSchedCfg((prev) => {
      const already = prev.daysOfWeek.includes(d);
      const next = already
        ? prev.daysOfWeek.filter((x) => x !== d)
        : [...prev.daysOfWeek, d];
      // Always keep at least one day selected
      return { ...prev, daysOfWeek: next.length ? next : [d] };
    });
  }

  async function handleCreate() {
    const finalCron = pickerCron.trim();
    if (!name.trim() || !finalCron || !prompt.trim()) return;
    setSubmitting(true);
    try {
      await api.createSchedule({
        name: name.trim(),
        cron: finalCron,
        prompt: prompt.trim(),
        timezone,
        enabled: true,
        once: repeat === "once",
      });
      setName("");
      setCron("");
      setPrompt("");
      setTimezone("UTC");
      setFreqKey("Custom");
      setRecipeKey("Custom");
      setRepeat("daily");
      setSchedCfg(defaultConfig());
      setShowForm(false);
      flashMsg("Schedule created.");
      load();
    } catch (e) {
      flashMsg(String(e), true);
    } finally {
      setSubmitting(false);
    }
  }

  async function handleUpdate() {
    if (!editingSchedule) return;
    setSubmitting(true);
    try {
      const patch: { name?: string; cron?: string; prompt?: string; timezone?: string; once?: boolean } = {};
      if (name.trim()) patch.name = name.trim();
      if (pickerCron.trim()) patch.cron = pickerCron.trim();
      if (prompt.trim()) patch.prompt = prompt.trim();
      if (timezone) patch.timezone = timezone;
      patch.once = repeat === "once";
      await api.updateSchedule(editingSchedule.id, patch);
      setEditingSchedule(null);
      setShowForm(false);
      flashMsg("Schedule updated.");
      load();
    } catch (e) {
      flashMsg(String(e), true);
    } finally {
      setSubmitting(false);
    }
  }

  async function handleDelete(id: string, schedName: string) {
    if (!await confirm(`Delete schedule "${schedName}"?`, { title: "Delete Schedule", confirmLabel: "Delete", destructive: true })) return;
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
    dispatch({
      type: "SCHEDULE_RESULT",
      payload: { id: `manual-${s.id}`, schedule_id: s.id, schedule_label: s.name, status: "running", timestamp: Date.now() },
    });
    try {
      await api.runScheduleNow(s.id);
      dispatch({
        type: "SCHEDULE_RESULT",
        payload: { id: `manual-${s.id}`, schedule_id: s.id, schedule_label: s.name, status: "completed", result: "Schedule triggered successfully", timestamp: Date.now() },
      });
    } catch (e) {
      dispatch({
        type: "SCHEDULE_RESULT",
        payload: { id: `manual-${s.id}`, schedule_id: s.id, schedule_label: s.name, status: "failed", error: String(e), timestamp: Date.now() },
      });
    }
  }

  const toggleRuns = useCallback(async (id: string) => {
    if (expandedRuns === id) {
      setExpandedRuns(null);
      return;
    }
    setExpandedRuns(id);
    try {
      const runs = await api.getScheduleRuns(id, 5);
      setRunsCache((prev) => ({ ...prev, [id]: runs }));
    } catch {
      setRunsCache((prev) => ({ ...prev, [id]: [] }));
    }
  }, [expandedRuns]);

  function handleRecipeChange(key: string) {
    setRecipeKey(key);
    const preset = RECIPE_PRESETS[key];
    if (preset) setPrompt(preset);
  }

  function closeModal() {
    setShowForm(false);
    setEditingSchedule(null);
  }

  function startEdit(s: Schedule) {
    const parsed = scheduleToConfig(s);
    setName(s.name);
    setPrompt(s.prompt);
    setTimezone(s.timezone ?? "UTC");
    setRepeat(parsed.repeat);
    setSchedCfg(parsed.config);
    setEditingSchedule(s);
    setShowForm(true);
  }

  /* ── Create / Edit form modal overlay ───────────────────── */
  function renderModal() {
    if (!showForm) return null;
    return (
      <div className="sched-modal__overlay" onClick={closeModal}>
        <div className="sched-modal__dialog" onClick={(e) => e.stopPropagation()}>
          {/* Header */}
          <div className="sched-modal__header">
            <h2 className="sched-modal__title">{editingSchedule ? "Edit Schedule" : "New Schedule"}</h2>
            <button className="sched-modal__close" onClick={closeModal} aria-label="Close">
              x
            </button>
          </div>
          <Separator />

          {/* Body */}
          <div className="sched-modal__body">
            {/* Name */}
            <div className="sched-modal__field">
              <label className="sched-modal__label">Name</label>
              <input
                className="sched-modal__input"
                placeholder="Daily briefing"
                aria-label="Schedule name"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </div>

            {/* Repeat pattern — pill segmented control */}
            <div className="sched-modal__field">
              <label className="sched-modal__label--row">
                <Repeat size={13} style={{ color: "var(--color-accent)" }} />
                Repeat
              </label>
              <div className="sched-picker__pill-row" role="group" aria-label="Repeat pattern">
                {REPEAT_PATTERNS.map(({ key, label }) => (
                  <button
                    key={key}
                    type="button"
                    className={`sched-picker__pill${repeat === key ? " is-active" : ""}`}
                    onClick={() => setRepeat(key)}
                    aria-pressed={repeat === key}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </div>

            {/* Conditional controls per repeat pattern */}
            {repeat === "hourly" && (
              <div className="sched-picker__inline-row">
                <div className="sched-modal__field">
                  <label className="sched-modal__label">Every</label>
                  <div className="sched-picker__input-group">
                    <select
                      className="sched-modal__select sched-modal__select--w80"
                      value={schedCfg.everyNHours}
                      onChange={(e) => updateCfg({ everyNHours: Number(e.target.value) })}
                      aria-label="Every N hours"
                    >
                      {[1, 2, 3, 4, 6, 8, 12].map((n) => (
                        <option key={n} value={n}>{n}</option>
                      ))}
                    </select>
                    <span className="sched-picker__unit">hours</span>
                  </div>
                </div>
                <div className="sched-modal__field">
                  <label className="sched-modal__label">At minute</label>
                  <input
                    type="number"
                    min={0}
                    max={59}
                    className="sched-modal__input sched-modal__input--w80"
                    value={schedCfg.startMinute}
                    onChange={(e) => updateCfg({ startMinute: Math.min(59, Math.max(0, Number(e.target.value))) })}
                    aria-label="Starting at minute"
                  />
                </div>
              </div>
            )}

            {(repeat === "daily" || repeat === "weekly" || repeat === "monthly" || repeat === "once") && (
              <div className="sched-modal__field">
                <label className="sched-modal__label--row">
                  <Clock size={12} style={{ color: "var(--grey-500)" }} />
                  Time
                </label>
                <div className="sched-picker__inline-row">
                  <div className="sched-picker__input-group">
                    <select
                      className="sched-modal__select sched-modal__select--w78"
                      value={schedCfg.hour}
                      onChange={(e) => updateCfg({ hour: Number(e.target.value) })}
                      aria-label="Hour"
                    >
                      {Array.from({ length: 24 }, (_, i) => (
                        <option key={i} value={i}>{String(i).padStart(2, "0")}</option>
                      ))}
                    </select>
                    <span className="sched-picker__time-sep">:</span>
                    <select
                      className="sched-modal__select sched-modal__select--w78"
                      value={schedCfg.minute}
                      onChange={(e) => updateCfg({ minute: Number(e.target.value) })}
                      aria-label="Minute"
                    >
                      {[0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55].map((m) => (
                        <option key={m} value={m}>{String(m).padStart(2, "0")}</option>
                      ))}
                    </select>
                  </div>
                </div>
              </div>
            )}

            {repeat === "weekly" && (
              <div className="sched-modal__field">
                <label className="sched-modal__label">Days</label>
                <div className="sched-picker__pill-row" role="group" aria-label="Days of week">
                  {DAY_LABELS.map((day, i) => {
                    const val = DAY_VALUES[i];
                    const active = schedCfg.daysOfWeek.includes(val);
                    return (
                      <button
                        key={day}
                        type="button"
                        className={`sched-picker__pill${active ? " is-active" : ""}`}
                        onClick={() => toggleDay(val)}
                        aria-pressed={active}
                      >
                        {day}
                      </button>
                    );
                  })}
                </div>
              </div>
            )}

            {repeat === "monthly" && (
              <div className="sched-modal__field">
                <label className="sched-modal__label">Day of month</label>
                <div className="sched-picker__input-group">
                  <input
                    type="number"
                    min={1}
                    max={31}
                    className="sched-modal__input sched-modal__input--w80"
                    value={schedCfg.dayOfMonth}
                    onChange={(e) => updateCfg({ dayOfMonth: Math.min(31, Math.max(1, Number(e.target.value))) })}
                    aria-label="Day of month"
                  />
                  <span className="sched-picker__unit">of every month</span>
                </div>
              </div>
            )}

            {repeat === "custom" && (
              <div className="sched-modal__field">
                <label className="sched-modal__label">
                  Cron expression
                  <span className="sched-modal__cron-hint">sec min hr dom mon dow</span>
                </label>
                <input
                  className="sched-modal__input"
                  placeholder="0 0 8 * * *"
                  aria-label="Cron expression"
                  value={schedCfg.customCron}
                  onChange={(e) => updateCfg({ customCron: e.target.value })}
                  spellCheck={false}
                />
              </div>
            )}

            {/* Preview line */}
            <div className="sched-picker__preview">
              <Clock size={12} style={{ color: "var(--color-accent)", flexShrink: 0 }} />
              <span>{humanPreview(repeat, schedCfg, timezone)}</span>
            </div>

            {/* Timezone */}
            <div className="sched-modal__field">
              <label className="sched-modal__label--row">
                <Calendar size={13} style={{ color: "var(--grey-500)" }} />
                Timezone
              </label>
              <ZonePicker
                className="sched-modal__select"
                value={timezone}
                onChange={setTimezone}
                aria-label="Schedule timezone"
              />
            </div>

            {/* Separator between timing and prompt */}
            <Separator />

            {/* Recipe select */}
            <div className="sched-modal__field">
              <label className="sched-modal__label">Recipe</label>
              <select
                className="sched-modal__select"
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

            <div className="sched-modal__field">
              <label className="sched-modal__label">Prompt</label>
              <textarea
                className="sched-modal__textarea"
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
          <div className="sched-modal__footer">
            <Button size="sm" variant="ghost" onPress={closeModal}>
              Cancel
            </Button>
            <Button
              size="sm"
              variant="primary"
              onPress={editingSchedule ? handleUpdate : handleCreate}
              isDisabled={submitting || !name.trim() || !pickerCron.trim() || !prompt.trim()}
            >
              {submitting
                ? editingSchedule ? "Updating..." : "Creating..."
                : editingSchedule ? "Update Schedule" : "Create Schedule"}
            </Button>
          </div>
        </div>
      </div>
    );
  }

  const routinesEl = routines.length > 0 ? (
    <section className="sched-routines">
      <div className="sched-routines__head">
        <h2 className="sched-section__title">Routines</h2>
        <span className="sched-section__sub">One tap · runs instantly</span>
      </div>
      <div className="rt__grid">
        {routines.map((r) => {
          const isRunning = runningRoutine === r.id;
          return (
            <div key={r.id} className="rt-card">
              <div className="rt-card__top">
                <span className="rt-card__icon" style={{ background: r.bg }}>
                  <HubIco d={r.iconPath} size={22} color="#fff" sw={2} />
                </span>
                <div>
                  <div className="rt-card__name">{r.name}</div>
                  <div className="rt-card__time">{r.time}</div>
                </div>
              </div>
              <div className="rt-card__does">
                {r.does.map((chip) => (
                  <span key={chip} className="rt-card__chip">{chip}</span>
                ))}
              </div>
              <button
                className="rt-card__run"
                data-running={isRunning}
                style={isRunning
                  ? { background: r.color, borderColor: r.color, color: "#fff" }
                  : { color: r.color }}
                onClick={() => handleRunRoutine(r.id, r.name)}
                disabled={!state.serverOnline}
                type="button"
              >
                {isRunning ? (
                  <><HubIco d={HP_PATHS.check} size={15} color="#fff" sw={3} /> Running…</>
                ) : (
                  <><HubIco d={HP_PATHS.play} size={14} color={r.color} fill={r.color} /> Run now</>
                )}
              </button>
            </div>
          );
        })}
      </div>
    </section>
  ) : null;

  return (
    <div className="screen screen--schedules">
      {/* ── Page header ─────────────────────────────────────── */}
      <div className="page-header">
        <div>
          <h1 className="page-header__title">Routines & Schedules</h1>
          <p className="sched-header__sub">One-tap scenes and recurring automations.</p>
        </div>
        <div className="page-header__action">
          <div className="view-toggle" role="group" aria-label="View mode">
            <button
              className={`view-toggle__btn${view === "list" ? " is-active" : ""}`}
              onClick={() => setViewPersisted("list")}
              aria-pressed={view === "list"}
              aria-label="List view"
              title="List view"
            >
              <List size={16} />
            </button>
            <button
              className={`view-toggle__btn${view === "calendar" ? " is-active" : ""}`}
              onClick={() => setViewPersisted("calendar")}
              aria-pressed={view === "calendar"}
              aria-label="Calendar view"
              title="Calendar view"
            >
              <Calendar size={16} />
            </button>
          </div>
          <Button
            size="sm"
            variant="secondary"
            className="page-header-btn"
            onPress={() => setShowRoutineForm(true)}
          >
            <Plus size={14} /> New Routine
          </Button>
          <Button
            size="sm"
            variant="primary"
            className="page-header-btn"
            isDisabled={!state.serverOnline}
            onPress={() => setShowForm(true)}
          >
            <Plus size={14} /> New Schedule
          </Button>
        </div>
      </div>

      {/* ── Feedback messages ───────────────────────────────── */}
      {actionMsg   && <p className="sched-msg--ok">{actionMsg}</p>}
      {actionError && <p className="sched-msg--err">{actionError}</p>}
      {error       && <p className="sched-msg--err">{error}</p>}

      {/* ── Schedule content ────────────────────────────────── */}
      {loading ? (
        <SkeletonList rows={4} />
      ) : schedules.length === 0 ? (
        <>
          {routinesEl}
          <div className="empty-state">
            <CalendarClock size={32} />
            <span>No schedules yet. Automate recurring tasks by creating your first one.</span>
            <button className="empty-state__cta" onClick={() => setShowForm(true)}>
              <Plus size={14} /> New Schedule
            </button>
          </div>
        </>
      ) : view === "calendar" ? (
        <>
          {routinesEl}
          <div className="sched-routines__head">
            <h2 className="sched-section__title">Schedules</h2>
            <span className="sched-section__sub">Time-triggered · runs automatically</span>
          </div>
          <ScheduleCalendar schedules={schedules} onEdit={startEdit} />
        </>
      ) : (
        <>
        {routinesEl}
        <div className="sched-routines__head">
          <h2 className="sched-section__title">Schedules</h2>
          <span className="sched-section__sub">Time-triggered · runs automatically</span>
        </div>
        <div className="sched-grid">
          {schedules.slice(0, visibleCount).map((s) => {
            const isRunning = runningIds.has(s.id);
            const parsed = scheduleToConfig(s);
            const timePreview = humanPreview(parsed.repeat, parsed.config, s.timezone ?? "UTC");
            const statusClass = isRunning ? "sched-card__chip--running" : s.enabled ? "sched-card__chip--active" : "sched-card__chip--paused";
            const statusLabel = isRunning ? "Running" : s.enabled ? "Active" : "Paused";
            return (
              <div key={s.id} className={`sched-card${!s.enabled ? " sched-card--paused" : ""}`}>
                {/* Top: icon + info + toggle */}
                <div className="sched-card__top">
                  <span className="sched-card__icon">
                    <CalendarClock size={22} />
                  </span>
                  <div className="sched-card__info">
                    <div className="sched-card__name">{s.name}</div>
                    <div className="sched-card__time" title={s.cron}>
                      {timePreview}
                      {s.timezone && s.timezone !== "UTC" && ` · ${s.timezone}`}
                    </div>
                  </div>
                  <label className="toggle-wrap" aria-label={s.enabled ? "Pause schedule" : "Resume schedule"}>
                    <input
                      type="checkbox"
                      checked={s.enabled}
                      disabled={!state.serverOnline}
                      onChange={() => handleToggle(s)}
                    />
                    <span className="toggle-track" />
                    <span className="toggle-thumb" />
                  </label>
                </div>

                {/* Chips: prompt snippet + status */}
                <div className="sched-card__chips">
                  {s.prompt && (
                    <span className="sched-card__chip">
                      {s.prompt.length > 40 ? s.prompt.slice(0, 40) + "…" : s.prompt}
                    </span>
                  )}
                  <span className={`sched-card__chip ${statusClass}`}>{statusLabel}</span>
                </div>

                {/* Run button */}
                <button
                  className="sched-card__run"
                  data-running={isRunning}
                  disabled={!state.serverOnline}
                  onClick={() => handleRunNow(s)}
                >
                  {isRunning ? (
                    <><Clock size={14} /> Running…</>
                  ) : (
                    <><Play size={14} fill="currentColor" /> Run now</>
                  )}
                </button>

                {/* Action row: edit + delete + history */}
                <div className="sched-card__actions">
                  <button
                    className="sched-card__action-btn"
                    disabled={!state.serverOnline}
                    aria-label="Edit schedule"
                    onClick={() => startEdit(s)}
                  >
                    <Pencil size={11} /> Edit
                  </button>
                  <button
                    className="sched-card__action-btn sched-card__action-btn--danger"
                    disabled={!state.serverOnline}
                    aria-label="Delete schedule"
                    onClick={() => handleDelete(s.id, s.name)}
                  >
                    <Trash2 size={11} /> Delete
                  </button>
                  <button
                    className="sched-card__action-btn sched-card__action-btn--right"
                    onClick={() => toggleRuns(s.id)}
                  >
                    {expandedRuns === s.id ? <ChevronUp size={12} /> : <ChevronDown size={12} />}
                    History
                  </button>
                </div>

                {/* Run history (collapsible) */}
                {expandedRuns === s.id && (
                  <div className="sched-history__list">
                    {(runsCache[s.id] ?? []).length > 0 && (
                      <div className="sched-health-bar">
                        {(runsCache[s.id] ?? []).slice(0, 5).map((r) => (
                          <span
                            key={r.id}
                            className="sched-health-dot"
                            data-status={r.status}
                            title={`${r.status} — ${new Date(r.started_at).toLocaleString()}`}
                          />
                        ))}
                      </div>
                    )}
                    {(runsCache[s.id] ?? []).length === 0 ? (
                      <span className="sched-history__empty">No runs yet.</span>
                    ) : (
                      (runsCache[s.id] ?? []).map((r) => (
                        <div key={r.id} className="sched-run-row">
                          <span className={`sched-card__chip sched-card__chip--${r.status === "completed" ? "active" : r.status === "failed" ? "paused" : "running"}`}>
                            {r.status}
                          </span>
                          <span className="sched-run-row__time" title={new Date(r.started_at).toLocaleString()}>
                            {timeAgo(r.started_at)}
                          </span>
                          {r.duration_ms != null && (
                            <span className="sched-run-row__duration">{(r.duration_ms / 1000).toFixed(1)}s</span>
                          )}
                          {(r.result || r.error) && (
                            <span
                              onClick={(e) => {
                                e.stopPropagation();
                                setExpandedRunId(expandedRunId === r.id ? null : r.id);
                              }}
                              className={[
                                "sched-run-row__result",
                                expandedRunId === r.id && "is-expanded",
                                r.error && "sched-run-row__result--error",
                              ].filter(Boolean).join(" ")}
                            >
                              {expandedRunId === r.id
                                ? (r.result ?? r.error ?? "")
                                : (r.result ?? r.error ?? "").slice(0, 120)}
                            </span>
                          )}
                        </div>
                      ))
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
        {schedules.length > visibleCount && (
          <button className="empty-state__cta sched-show-more" onClick={() => setVisibleCount((c) => c + SCHED_PAGE)}>
            Show {Math.min(SCHED_PAGE, schedules.length - visibleCount)} more
            <span className="muted"> · {schedules.length - visibleCount} remaining</span>
          </button>
        )}
        </>
      )}

      {/* ── Create modal ────────────────────────────────────── */}
      {renderModal()}

      {/* ── New Routine modal ───────────────────────────────── */}
      {showRoutineForm && (
        <NewRoutineModal
          onClose={() => setShowRoutineForm(false)}
          onCreated={() => setShowRoutineForm(false)}
        />
      )}
    </div>
  );
}

