import type { Schedule } from "../api/types";

export interface ScheduleSlot {
  scheduleId: string;
  label: string;
  hour: number;
  minute: number;
  daysOfWeek: number[]; // 0=Sun … 6=Sat; empty = every day
  frequency: "hourly" | "daily" | "weekly" | "monthly" | "custom";
  color: string;
}

export const SCHEDULE_COLORS = [
  "#9333ea",
  "#3b82f6",
  "#22c55e",
  "#f97316",
  "#ef4444",
  "#14b8a6",
  "#ec4899",
];

/** Parse a 6-field cron (sec min hr dom mon dow) into a display slot; common GIAP patterns only. */
export function cronToSlots(schedule: Schedule, colorIndex: number): ScheduleSlot {
  const color = SCHEDULE_COLORS[colorIndex % SCHEDULE_COLORS.length];
  const cron = schedule.cron.trim();
  const parts = cron.split(/\s+/);

  // Some callers send 5-field cron (no seconds); prepend "0".
  const fields = parts.length === 5 ? ["0", ...parts] : parts;

  const [, minuteField, hourField, domField, , dowField] = fields;

  const isHourly =
    hourField === "*" ||
    (minuteField.startsWith("*/") && hourField === "*") ||
    minuteField === "*";

  const isWeekly =
    !isHourly &&
    hourField !== "*" &&
    dowField !== "*" &&
    domField === "*";

  const isMonthly =
    !isHourly &&
    !isWeekly &&
    hourField !== "*" &&
    domField !== "*" &&
    dowField === "*";

  const isDaily =
    !isHourly &&
    !isWeekly &&
    !isMonthly &&
    hourField !== "*" &&
    domField === "*";

  let hour = 0;
  let minute = 0;
  let daysOfWeek: number[] = [];
  let frequency: ScheduleSlot["frequency"];

  if (isHourly) {
    // For "0 */30 * * * *" show at :00 of each hour; minute marker at 0.
    hour = 0;
    minute = 0;
    daysOfWeek = [];
    frequency = "hourly";
  } else {
    hour = parseInt(hourField, 10) || 0;
    minute = parseInt(minuteField, 10) || 0;

    if (isWeekly) {
      // Parse DOW — can be "1", "1,3", "1-5" etc.
      daysOfWeek = parseDayOfWeek(dowField);
      frequency = "weekly";
    } else if (isMonthly) {
      daysOfWeek = [];
      frequency = "monthly";
    } else if (isDaily) {
      daysOfWeek = [];
      frequency = "daily";
    } else {
      daysOfWeek = [];
      frequency = "custom";
    }
  }

  return {
    scheduleId: schedule.id,
    label: schedule.name,
    hour,
    minute,
    daysOfWeek,
    frequency,
    color,
  };
}

/** Parse DOW field into array of 0-based integers (0=Sun). */
function parseDayOfWeek(field: string): number[] {
  if (field === "*") return [];
  const days: number[] = [];
  const parts = field.split(",");
  for (const part of parts) {
    if (part.includes("-")) {
      const [start, end] = part.split("-").map(Number);
      for (let d = start; d <= end; d++) days.push(d);
    } else {
      const d = parseInt(part, 10);
      if (!isNaN(d)) days.push(d);
    }
  }
  return [...new Set(days)].sort();
}

export function frequencyLabel(slot: ScheduleSlot): string {
  const timeStr = `${String(slot.hour).padStart(2, "0")}:${String(slot.minute).padStart(2, "0")}`;
  switch (slot.frequency) {
    case "hourly":  return "Every hour";
    case "daily":   return `Daily at ${timeStr}`;
    case "weekly": {
      const dayNames = ["Sun","Mon","Tue","Wed","Thu","Fri","Sat"];
      const days = slot.daysOfWeek.map((d) => dayNames[d]).join(", ");
      return `Weekly — ${days || "?"} at ${timeStr}`;
    }
    case "monthly": return `Monthly at ${timeStr}`;
    default:        return `Custom (${timeStr})`;
  }
}
