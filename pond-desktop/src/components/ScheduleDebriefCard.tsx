import { useState, useEffect, useRef, useCallback, useMemo } from "react";
import { AnimatePresence, motion } from "framer-motion";
import {
  Sun,
  BarChart2,
  Sparkles,
  X,
  Calendar,
  CheckCircle,
  CheckCircle2,
  Circle,
  Clock,
  TrendingUp,
  Zap,
  Mail,
  Target,
  XCircle,
} from "lucide-react";
import { Chip, Button } from "@heroui/react";
import type { ScheduleRunNotification } from "../api/types";

// ── Custom hooks ────────────────────────────────────────────────

function useTypewriter(text: string, speed = 14): { displayed: string; done: boolean } {
  const [displayed, setDisplayed] = useState("");
  const [done, setDone] = useState(false);
  const indexRef = useRef(0);
  const rafRef = useRef<number | null>(null);
  const lastTimeRef = useRef<number | null>(null);

  useEffect(() => {
    if (!text) {
      setDisplayed("");
      setDone(true);
      return;
    }
    indexRef.current = 0;
    setDisplayed("");
    setDone(false);

    const tick = (now: number) => {
      if (lastTimeRef.current === null) lastTimeRef.current = now;
      const elapsed = now - lastTimeRef.current;
      const charsToAdd = Math.floor(elapsed / speed);
      if (charsToAdd > 0) {
        lastTimeRef.current = now;
        indexRef.current = Math.min(indexRef.current + charsToAdd, text.length);
        setDisplayed(text.slice(0, indexRef.current));
        if (indexRef.current >= text.length) {
          setDone(true);
          return;
        }
      }
      rafRef.current = requestAnimationFrame(tick);
    };

    rafRef.current = requestAnimationFrame(tick);
    return () => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      lastTimeRef.current = null;
    };
  }, [text, speed]);

  return { displayed, done };
}

function useCountUp(target: number, duration = 900, delay = 0): number {
  const [value, setValue] = useState(0);
  const rafRef = useRef<number | null>(null);

  useEffect(() => {
    if (target === 0) {
      setValue(0);
      return;
    }
    let startTime: number | null = null;
    let timeoutId: ReturnType<typeof setTimeout> | null = null;

    const easeOutCubic = (t: number) => 1 - Math.pow(1 - t, 3);

    const animate = (now: number) => {
      if (startTime === null) startTime = now;
      const elapsed = now - startTime;
      const progress = Math.min(elapsed / duration, 1);
      setValue(Math.round(easeOutCubic(progress) * target));
      if (progress < 1) {
        rafRef.current = requestAnimationFrame(animate);
      }
    };

    timeoutId = setTimeout(() => {
      rafRef.current = requestAnimationFrame(animate);
    }, delay);

    return () => {
      if (timeoutId !== null) clearTimeout(timeoutId);
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
    };
  }, [target, duration, delay]);

  return value;
}

// ── Helpers ─────────────────────────────────────────────────────

function formatDate(iso: string): string {
  try {
    return new Date(iso).toLocaleDateString("en-US", {
      weekday: "long",
      month: "long",
      day: "numeric",
    });
  } catch {
    return iso;
  }
}

function tryParseJson(text: string | null): Record<string, unknown> | null {
  if (!text) return null;
  try {
    const parsed = JSON.parse(text);
    if (typeof parsed === "object" && parsed !== null) return parsed as Record<string, unknown>;
  } catch {
    /* not JSON */
  }
  return null;
}

function extractNumber(obj: Record<string, unknown>, ...keys: string[]): number {
  for (const k of keys) {
    const v = obj[k];
    if (typeof v === "number") return v;
    if (typeof v === "string") {
      const n = parseInt(v, 10);
      if (!isNaN(n)) return n;
    }
  }
  return 0;
}

// ── Router ───────────────────────────────────────────────────────

export function ScheduleDebriefCard({
  run,
  onClose,
}: {
  run: ScheduleRunNotification;
  onClose?: () => void;
}) {
  switch (run.recipe) {
    case "daily-summary":
      return <DailyBriefingCard run={run} onClose={onClose} />;
    case "weekly-report":
      return <WeeklyReportCard run={run} onClose={onClose} />;
    case "compact-memory":
      return <MemoryCompactionCard run={run} onClose={onClose} />;
    case "routine":
      return <RoutineDebriefCard run={run} onClose={onClose} />;
    default:
      return <GenericDebriefCard run={run} onClose={onClose} />;
  }
}

// ── RoutineDebriefCard ───────────────────────────────────────────

function RoutineDebriefCard({
  run,
  onClose,
}: {
  run: ScheduleRunNotification;
  onClose?: () => void;
}) {
  const actions = useMemo<string[]>(() => {
    if (!run.result) return [];
    try {
      const parsed = JSON.parse(run.result);
      if (Array.isArray(parsed)) return parsed as string[];
    } catch { /* fall through */ }
    return run.result.split(" · ").filter(Boolean);
  }, [run.result]);

  const [doneCount, setDoneCount] = useState(0);

  useEffect(() => {
    if (actions.length === 0) return;
    const timers = actions.map((_, i) =>
      setTimeout(() => setDoneCount((n) => n + 1), 400 + i * 420),
    );
    return () => timers.forEach(clearTimeout);
  }, [actions]);

  const failed = run.status === "failed";

  return (
    <div className="debrief-card debrief-card--routine">
      <div className="debrief-hd">
        <div className="debrief-hd__left">
          <Sparkles size={18} className="debrief-hd__icon" />
          <div>
            <div className="debrief-hd__title">{run.scheduleName}</div>
            <div className="debrief-hd__meta">{formatDate(run.startedAt)}</div>
          </div>
        </div>
        <div className="debrief-hd__right">
          <Chip size="sm" className="debrief-chip-label">routine</Chip>
          {onClose && (
            <button className="debrief-close" onClick={onClose} aria-label="Close">
              <X size={14} />
            </button>
          )}
        </div>
      </div>

      <div className="debrief-body">
        {failed ? (
          <div className="routine-action routine-action--failed">
            <XCircle size={16} />
            <span>{run.error ?? "Routine failed"}</span>
          </div>
        ) : (
          <div className="routine-actions">
            {actions.map((action, i) => {
              const done = i < doneCount;
              return (
                <motion.div
                  key={action}
                  className={`routine-action${done ? " is-done" : ""}`}
                  initial={{ opacity: 0, x: -8 }}
                  animate={{ opacity: 1, x: 0 }}
                  transition={{ delay: i * 0.07, duration: 0.2 }}
                >
                  <span className="routine-action__icon">
                    {done ? <CheckCircle2 size={15} /> : <Circle size={15} />}
                  </span>
                  <span className="routine-action__label">{action}</span>
                  {done && <span className="routine-action__tick">done</span>}
                </motion.div>
              );
            })}
            {doneCount >= actions.length && actions.length > 0 && (
              <motion.div
                className="routine-all-done"
                initial={{ opacity: 0, y: 4 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ delay: 0.15 }}
              >
                <CheckCircle size={13} />
                All actions completed
              </motion.div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

// ── StatCount — animated stat cell ──────────────────────────────

function StatCount({
  value,
  label,
  icon: IconComp,
  suffix = "",
  delay = 0,
}: {
  value: number;
  label: string;
  icon: React.ComponentType<{ size?: number }>;
  suffix?: string;
  delay?: number;
}) {
  const displayed = useCountUp(value, 900, delay);
  return (
    <div className="debrief-stat">
      <div className="debrief-stat__icon">
        <IconComp size={14} />
      </div>
      <div className="debrief-stat__num">
        {displayed}
        {suffix && <span className="debrief-stat__unit">{suffix}</span>}
      </div>
      <div className="debrief-stat__label">{label}</div>
    </div>
  );
}

// ── DailyBriefingCard ────────────────────────────────────────────

function DailyBriefingCard({
  run,
  onClose,
}: {
  run: ScheduleRunNotification;
  onClose?: () => void;
}) {
  const text = run.result ?? run.excerpt ?? "No result available.";
  const { displayed, done } = useTypewriter(text, 10);
  const [focusStarted, setFocusStarted] = useState(false);
  const [bodyVisible, setBodyVisible] = useState(false);

  useEffect(() => {
    if (done) {
      const t = setTimeout(() => setBodyVisible(true), 300);
      return () => clearTimeout(t);
    }
  }, [done]);

  const parsed = tryParseJson(run.result);
  const hasParsedStats = parsed != null && (
    parsed["meetings"] != null || parsed["tasks"] != null || parsed["emails"] != null
  );
  const meetings = hasParsedStats ? extractNumber(parsed!, "meetings", "meeting_count") : 0;
  const tasks = hasParsedStats ? extractNumber(parsed!, "tasks", "task_count", "open_tasks") : 0;
  const emails = hasParsedStats ? extractNumber(parsed!, "emails", "email_count") : 0;
  const productivity = hasParsedStats ? extractNumber(parsed!, "productivity", "productivity_score") : 0;

  const priority = parsed
    ? (parsed["priority"] as string | undefined) ??
      (parsed["top_priority"] as string | undefined) ??
      null
    : null;

  return (
    <div className="debrief-card debrief-card--daily">
      <div className="debrief-hd debrief-hd--daily">
        <div className="debrief-hd__left">
          <Sun size={18} className="debrief-hd__icon" />
          <div>
            <div className="debrief-hd__title">Morning Briefing</div>
            <div className="debrief-hd__meta">{formatDate(run.startedAt)}</div>
          </div>
        </div>
        <div className="debrief-hd__right">
          <Chip size="sm" className="debrief-chip-label">
            daily-summary
          </Chip>
          {onClose && (
            <button className="debrief-close" onClick={onClose} aria-label="Close">
              <X size={14} />
            </button>
          )}
        </div>
      </div>

      <div className="debrief-tw-zone">
        <pre className="debrief-tw">
          {displayed}
          {!done && <span className="debrief-cursor" aria-hidden="true" />}
        </pre>
      </div>

      <AnimatePresence>
        {bodyVisible && (
          <motion.div
            className="debrief-body"
            initial={{ opacity: 0, y: 8 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.25 }}
          >
            {hasParsedStats && (
              <div className="debrief-stats">
                {meetings > 0 && (
                  <StatCount value={meetings} label="Meetings" icon={Calendar} delay={0} />
                )}
                {tasks > 0 && (
                  <StatCount value={tasks} label="Open Tasks" icon={CheckCircle} delay={70} />
                )}
                {emails > 0 && (
                  <StatCount value={emails} label="Emails" icon={Mail} delay={140} />
                )}
                {productivity > 0 && (
                  <StatCount
                    value={productivity}
                    label="Productivity"
                    icon={TrendingUp}
                    suffix="/100"
                    delay={210}
                  />
                )}
              </div>
            )}

            {priority && (
              <div className="debrief-priority">
                <Target size={13} className="debrief-priority__icon" />
                <div>
                  <div className="debrief-priority__label">Top Priority</div>
                  <div className="debrief-priority__text">{priority}</div>
                </div>
              </div>
            )}

            <div className="debrief-focus">
              <div className="debrief-focus__text">
                <Zap size={14} />
                <span>Ready to enter focus mode?</span>
              </div>
              <Button
                size="sm"
                variant={focusStarted ? "ghost" : "primary"}
                onPress={() => setFocusStarted(true)}
                isDisabled={focusStarted}
              >
                {focusStarted ? "Focus started" : "Start focus block"}
              </Button>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

// ── WeeklyReportCard ─────────────────────────────────────────────

function BigNum({
  value,
  label,
  suffix = "",
  delay = 0,
}: {
  value: number;
  label: string;
  suffix?: string;
  delay?: number;
}) {
  const displayed = useCountUp(value, 900, delay);
  return (
    <div className="debrief-bignum">
      <div className="debrief-bignum__num">
        {displayed}
        {suffix}
      </div>
      <div className="debrief-bignum__label">{label}</div>
    </div>
  );
}

function WeeklyBarChart({ data }: { data: number[] }) {
  const max = Math.max(...data, 1);
  const days = ["M", "T", "W", "T", "F", "S", "S"];
  return (
    <div className="debrief-bars" role="img" aria-label="Weekly activity bar chart">
      {data.map((v, i) => (
        <div key={i} className="debrief-bar">
          <div
            className="debrief-bar-fill"
            style={
              {
                "--bar-height": `${Math.round((v / max) * 100)}%`,
                "--bar-index": i,
              } as React.CSSProperties
            }
          />
          <div className="debrief-bar-label">{days[i]}</div>
        </div>
      ))}
    </div>
  );
}

function WeeklyReportCard({
  run,
  onClose,
}: {
  run: ScheduleRunNotification;
  onClose?: () => void;
}) {
  const text = run.result ?? run.excerpt ?? "No result available.";
  const { displayed, done } = useTypewriter(text, 10);
  const [bodyVisible, setBodyVisible] = useState(false);

  useEffect(() => {
    if (done) {
      const t = setTimeout(() => setBodyVisible(true), 300);
      return () => clearTimeout(t);
    }
  }, [done]);

  const parsed = tryParseJson(run.result);
  const hasParsedStats = parsed != null && (
    parsed["tasks"] != null || parsed["tasks_completed"] != null || parsed["tokens"] != null
  );
  const tasks = hasParsedStats
    ? extractNumber(parsed!, "tasks_completed", "tasks", "completed_tasks")
    : 0;
  const tokensK = hasParsedStats
    ? Math.round(extractNumber(parsed!, "tokens", "token_count", "tokens_used") / 1000)
    : 0;
  const saved = hasParsedStats
    ? extractNumber(parsed!, "cost_saved", "saved", "dollars_saved")
    : 0;
  const voiceSessions = hasParsedStats
    ? extractNumber(parsed!, "voice_sessions", "voice", "voice_count")
    : 0;

  const rawTopics = parsed
    ? (parsed["topics"] as string[] | undefined) ??
      (parsed["tags"] as string[] | undefined) ??
      null
    : null;
  const topics: string[] | null = rawTopics;

  const rawDays = parsed
    ? (parsed["daily_activity"] as number[] | undefined) ??
      (parsed["days"] as number[] | undefined) ??
      null
    : null;
  const dayData: number[] | null = rawDays;

  const startDate = run.startedAt ? new Date(run.startedAt) : new Date();
  const endDate = new Date(startDate);
  endDate.setDate(startDate.getDate() - 6);
  const dateRange = `${endDate.toLocaleDateString("en-US", { month: "short", day: "numeric" })} – ${startDate.toLocaleDateString("en-US", { month: "short", day: "numeric" })}`;

  return (
    <div className="debrief-card debrief-card--weekly">
      <div className="debrief-hd debrief-hd--weekly">
        <div className="debrief-hd__left">
          <BarChart2 size={18} className="debrief-hd__icon" />
          <div>
            <div className="debrief-hd__title">Weekly Report</div>
            <div className="debrief-hd__meta">{dateRange}</div>
          </div>
        </div>
        <div className="debrief-hd__right">
          <Chip size="sm" className="debrief-chip-label">
            weekly-report
          </Chip>
          {onClose && (
            <button className="debrief-close" onClick={onClose} aria-label="Close">
              <X size={14} />
            </button>
          )}
        </div>
      </div>

      <div className="debrief-tw-zone">
        <pre className="debrief-tw">
          {displayed}
          {!done && <span className="debrief-cursor" aria-hidden="true" />}
        </pre>
      </div>

      <AnimatePresence>
        {bodyVisible && (
          <motion.div
            className="debrief-body"
            initial={{ opacity: 0, y: 8 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.25 }}
          >
            {hasParsedStats && (
              <div className="debrief-bignums">
                {tasks > 0 && <BigNum value={tasks} label="Tasks done" delay={0} />}
                {tokensK > 0 && <BigNum value={tokensK} label="Tokens (k)" suffix="k" delay={70} />}
                {saved > 0 && <BigNum value={saved} label="Est. saved ($)" suffix="$" delay={140} />}
                {voiceSessions > 0 && (
                  <BigNum value={voiceSessions} label="Voice sessions" delay={210} />
                )}
              </div>
            )}

            {dayData && (
              <div className="debrief-section">
                <div className="debrief-section__label">Tokens by day</div>
                <WeeklyBarChart data={dayData} />
              </div>
            )}

            {topics && topics.length > 0 && (
              <div className="debrief-tags">
                {topics.map((t, i) => (
                  <span key={t} className="debrief-tag" style={{ animationDelay: `${i * 60}ms` }}>
                    {t}
                  </span>
                ))}
              </div>
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

// ── MemoryCompactionCard ─────────────────────────────────────────

type CompactionPhase = "before" | "removing" | "after";

interface MemoryChipEntry {
  id: string;
  label: string;
  kept: boolean;
}

function MemoryCompactionCard({
  run,
  onClose,
}: {
  run: ScheduleRunNotification;
  onClose?: () => void;
}) {
  const [phase, setPhase] = useState<CompactionPhase>("before");
  const parsed = tryParseJson(run.result);

  const hasParsedData =
    parsed != null &&
    (parsed["before_count"] != null || parsed["total_before"] != null);
  const beforeCount = hasParsedData
    ? extractNumber(parsed!, "before_count", "total_before", "fragments_before")
    : 0;
  const afterCount = hasParsedData
    ? extractNumber(parsed!, "after_count", "total_after", "fragments_after")
    : 0;
  const dedupPct = hasParsedData
    ? extractNumber(parsed!, "dedup_pct", "dedup_percent", "deduplication_pct")
    : beforeCount > 0
      ? Math.round(((beforeCount - afterCount) / beforeCount) * 100)
      : 0;

  const rawEntries = parsed
    ? (parsed["entries"] as MemoryChipEntry[] | undefined) ??
      (parsed["memory_entries"] as MemoryChipEntry[] | undefined) ??
      null
    : null;

  const entries: MemoryChipEntry[] = rawEntries ?? [];

  const counterValue = useCountUp(
    phase === "before" ? beforeCount : phase === "removing" ? beforeCount - afterCount : afterCount,
    700,
    0,
  );

  const advancePhase = useCallback(() => {
    setPhase((p) => (p === "before" ? "removing" : p === "removing" ? "after" : "after"));
  }, []);

  useEffect(() => {
    const t1 = setTimeout(() => setPhase("removing"), 800);
    const t2 = setTimeout(() => setPhase("after"), 1800);
    return () => {
      clearTimeout(t1);
      clearTimeout(t2);
    };
  }, []);

  const phaseLabel: Record<CompactionPhase, string> = {
    before: "before",
    removing: "removing duplicates…",
    after: "after",
  };

  return (
    <div className="debrief-card debrief-card--memory">
      <div className="debrief-hd debrief-hd--memory">
        <div className="debrief-hd__left">
          <Sparkles size={18} className="debrief-hd__icon" />
          <div>
            <div className="debrief-hd__title">Memory Compaction</div>
            <div className="debrief-hd__meta">{formatDate(run.startedAt)}</div>
          </div>
        </div>
        <div className="debrief-hd__right">
          <Chip size="sm" className="debrief-chip-label">
            compact-memory
          </Chip>
          {onClose && (
            <button className="debrief-close" onClick={onClose} aria-label="Close">
              <X size={14} />
            </button>
          )}
        </div>
      </div>

      <div className="debrief-body">
        <div className="debrief-counter">
          <div className="debrief-counter__num">{counterValue}</div>
          <div className="debrief-counter__phase" data-phase={phase}>
            {phaseLabel[phase]}
          </div>
          {phase !== "after" && (
            <button className="debrief-counter__skip" onClick={advancePhase}>
              skip
            </button>
          )}
        </div>

        {entries.length > 0 && (
          <div className="debrief-chipgrid">
            {entries.map((e, i) => {
              const chipClass = e.kept
                ? "debrief-chip--kept"
                : phase === "after"
                  ? "debrief-chip--removed"
                  : phase === "removing"
                    ? "debrief-chip--removing"
                    : "";
              return (
                <span
                  key={e.id}
                  className={`debrief-chip ${chipClass}`}
                  style={{ animationDelay: `${i * 38}ms` }}
                >
                  {e.label}
                </span>
              );
            })}
          </div>
        )}

        <div className="debrief-dedup">
          <div className="debrief-dedup__labels">
            <span>Deduplication</span>
            <span>{dedupPct}%</span>
          </div>
          <div className="debrief-dedup__track">
            <div
              className="debrief-dedup__fill"
              style={{ width: `${phase === "after" ? dedupPct : 0}%`, transition: "width 1s ease-out" }}
            />
          </div>
        </div>
      </div>
    </div>
  );
}

// ── GenericDebriefCard ───────────────────────────────────────────

function GenericDebriefCard({
  run,
  onClose,
}: {
  run: ScheduleRunNotification;
  onClose?: () => void;
}) {
  const text = run.result ?? run.excerpt ?? "No result available.";
  const { displayed, done } = useTypewriter(text, 12);

  return (
    <div className="debrief-card">
      <div className="debrief-hd">
        <div className="debrief-hd__left">
          <Clock size={18} className="debrief-hd__icon" />
          <div>
            <div className="debrief-hd__title">{run.scheduleName || "Schedule Result"}</div>
            <div className="debrief-hd__meta">{formatDate(run.startedAt)}</div>
          </div>
        </div>
        <div className="debrief-hd__right">
          {run.recipe && (
            <Chip size="sm" className="debrief-chip-label">
              {run.recipe}
            </Chip>
          )}
          {onClose && (
            <button className="debrief-close" onClick={onClose} aria-label="Close">
              <X size={14} />
            </button>
          )}
        </div>
      </div>

      <div className="debrief-tw-zone">
        <pre className="debrief-tw">
          {displayed}
          {!done && <span className="debrief-cursor" aria-hidden="true" />}
        </pre>
      </div>
    </div>
  );
}
