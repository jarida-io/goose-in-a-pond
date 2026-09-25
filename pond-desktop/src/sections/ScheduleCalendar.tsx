import { useState, useEffect, useRef, useCallback } from "react";
import { Clock, Pencil } from "lucide-react";
import type { Schedule } from "../api/types";
import {
  cronToSlots,
  frequencyLabel,
  type ScheduleSlot,
} from "./calendarUtils";

interface Props {
  schedules: Schedule[];
  onEdit?: (schedule: Schedule) => void;
}

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
// Column → JS getDay() index (Sun=0).
const DAY_JS_INDEX = [1, 2, 3, 4, 5, 6, 0];

const HOURS = Array.from({ length: 24 }, (_, i) => i);

interface PopoverState {
  slot: ScheduleSlot;
  x: number;
  y: number;
}

export function ScheduleCalendar({ schedules, onEdit }: Props) {
  const [popover, setPopover] = useState<PopoverState | null>(null);
  const [nowHour, setNowHour] = useState(() => new Date().getHours());
  const [nowMinute, setNowMinute] = useState(() => new Date().getMinutes());
  const [todayDow, setTodayDow] = useState(() => new Date().getDay());
  const containerRef = useRef<HTMLDivElement>(null);

  // Tick every minute to keep the current-time marker accurate.
  useEffect(() => {
    const tick = () => {
      const now = new Date();
      setNowHour(now.getHours());
      setNowMinute(now.getMinutes());
      setTodayDow(now.getDay());
    };
    const msToNextMinute = (60 - new Date().getSeconds()) * 1000;
    const timeout = setTimeout(() => {
      tick();
      const interval = setInterval(tick, 60_000);
      return () => clearInterval(interval);
    }, msToNextMinute);
    return () => clearTimeout(timeout);
  }, []);

  useEffect(() => {
    if (!popover) return;
    const handler = (e: MouseEvent) => {
      const target = e.target as Element;
      if (!target.closest(".sched-cal__popover") && !target.closest(".sched-cal__pill") && !target.closest(".sched-cal__dot")) {
        setPopover(null);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [popover]);

  // Scroll the body so the current hour is roughly centered on mount.
  const bodyRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!bodyRef.current) return;
    const ROW_H = 48;
    const visibleRows = Math.floor(520 / ROW_H);
    const targetRow = Math.max(0, nowHour - Math.floor(visibleRows / 2));
    bodyRef.current.scrollTop = targetRow * ROW_H;
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Skip one-shots (`fire_at` set, `cron` = "@once"): not recurring, and cronToSlots can't parse "@once".
  const slots: ScheduleSlot[] = schedules
    .filter((s) => !s.fire_at)
    .map((s, i) => cronToSlots(s, i));

  // Group slots by hour × col (col = 0-6, Mon–Sun).
  type CellKey = `${number}-${number}`;
  const cellSlots = new Map<CellKey, ScheduleSlot[]>();

  for (const slot of slots) {
    if (slot.frequency === "hourly") {
      for (let h = 0; h < 24; h++) {
        for (let col = 0; col < 7; col++) {
          const key: CellKey = `${h}-${col}`;
          const list = cellSlots.get(key) ?? [];
          list.push(slot);
          cellSlots.set(key, list);
        }
      }
    } else if (slot.frequency === "weekly" && slot.daysOfWeek.length > 0) {
      for (const dow of slot.daysOfWeek) {
        const col = DAY_JS_INDEX.indexOf(dow);
        if (col === -1) continue;
        const key: CellKey = `${slot.hour}-${col}`;
        const list = cellSlots.get(key) ?? [];
        list.push(slot);
        cellSlots.set(key, list);
      }
    } else {
      // daily / monthly / custom — show in all columns.
      for (let col = 0; col < 7; col++) {
        const key: CellKey = `${slot.hour}-${col}`;
        const list = cellSlots.get(key) ?? [];
        list.push(slot);
        cellSlots.set(key, list);
      }
    }
  }

  const handlePillClick = useCallback(
    (e: React.MouseEvent, slot: ScheduleSlot) => {
      e.stopPropagation();
      const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
      setPopover({
        slot,
        x: Math.min(rect.right + 8, window.innerWidth - 296),
        y: Math.min(rect.top, window.innerHeight - 180),
      });
    },
    [],
  );

  // Marker offset within the hour's cell (48 px = ROW_H).
  const nowOffsetPx = (nowMinute / 60) * 48;

  return (
    <div ref={containerRef} className="sched-cal__wrap">
      <div className="sched-cal">
        {/* ── Header row ─────────────────────────────────── */}
        <div className="sched-cal__header-spacer" />
        {DAYS.map((day, col) => {
          const jsDay = DAY_JS_INDEX[col];
          return (
            <div
              key={day}
              className={`sched-cal__day-header${jsDay === todayDow ? " is-today" : ""}`}
            >
              {day}
            </div>
          );
        })}

        {/* ── Scrollable body ─────────────────────────────── */}
        <div className="sched-cal__body" ref={bodyRef}>
          {HOURS.map((h) => (
            <div key={h} className="sched-cal__hour-row">
              {/* Time label */}
              <div className="sched-cal__time-label">
                {String(h).padStart(2, "0")}:00
                {/* Current-time marker on the time label column */}
                {h === nowHour && (
                  <div
                    className="sched-cal__time-marker"
                    style={{ top: nowOffsetPx }}
                  />
                )}
              </div>

              {/* 7 day cells */}
              {DAYS.map((_, col) => {
                const key: CellKey = `${h}-${col}`;
                const cellItems = cellSlots.get(key) ?? [];
                const jsDay = DAY_JS_INDEX[col];
                const isToday = jsDay === todayDow;

                return (
                  <div
                    key={col}
                    className={`sched-cal__cell${isToday ? " is-today" : ""}`}
                  >
                    {/* Current-time marker */}
                    {h === nowHour && isToday && (
                      <div
                        className="sched-cal__time-marker"
                        style={{ top: nowOffsetPx }}
                      />
                    )}

                    {/* Pills / dots for this cell — side by side when 2+ */}
                    <div
                      className="sched-cal__cell-items"
                      style={{ flexDirection: cellItems.length <= 2 ? "row" : "column" }}
                    >
                      {cellItems.map((slot) =>
                        slot.frequency === "hourly" ? (
                          <button
                            key={slot.scheduleId}
                            className="sched-cal__dot"
                            style={{ background: slot.color }}
                            title={slot.label}
                            onClick={(e) => handlePillClick(e, slot)}
                            aria-label={slot.label}
                          />
                        ) : (
                          <button
                            key={slot.scheduleId}
                            className="sched-cal__pill"
                            style={{
                              background: slot.color,
                              flex: cellItems.length === 2 ? "1 1 0" : undefined,
                              minWidth: cellItems.length === 2 ? 0 : undefined,
                            }}
                            onClick={(e) => handlePillClick(e, slot)}
                            aria-label={slot.label}
                          >
                            <Clock size={8} />
                            {slot.label}
                          </button>
                        )
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      </div>

      {/* ── Pill popover ─────────────────────────────────── */}
      {popover && (() => {
        const s = schedules.find((sc) => sc.id === popover.slot.scheduleId);
        return (
          <div
            className="sched-cal__popover"
            style={{ top: popover.y, left: popover.x }}
          >
            <div className="sched-cal__popover-title">{popover.slot.label}</div>
            <code className="sched-cal__popover-cron">{s?.cron ?? ""}</code>
            <div className="sched-cal__popover-freq">
              {frequencyLabel(popover.slot)}
            </div>
            {s?.prompt && (
              <div className="sched-cal__popover-result">
                {s.prompt.slice(0, 120)}
                {s.prompt.length > 120 ? "..." : ""}
              </div>
            )}
            {onEdit && s && (
              <button
                className="sched-cal__popover-edit"
                aria-label={`Edit ${s.name}`}
                onClick={() => {
                  onEdit(s);
                  setPopover(null);
                }}
              >
                <Pencil size={11} /> Edit
              </button>
            )}
          </div>
        );
      })()}
    </div>
  );
}
