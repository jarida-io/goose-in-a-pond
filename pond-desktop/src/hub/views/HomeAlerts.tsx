// Two things worth interrupting Home for: a routine due before midnight, and a device that's gone
// quiet. Both are read straight off the real APIs -- no fabricated "skip" or "wake" action, since
// neither has a backend behind it today (see PondApiClient: pauseSchedule disables a routine
// entirely, not just tonight's run; markDeviceOnline is a manual registry correction, not a wake).

import { useEffect, useState } from "react";
import { InkButton, InkCard } from "@jarida/ink/react";
import { api } from "../../api/PondApiClient";
import { timeSince } from "../../sections/timeSince";
import { useHomeData } from "../state/hubDataStore";
import { nextDueTonight } from "../state/routines";
import type { Device, Schedule } from "../../api/types";
import type { GuiSection } from "../../desktopState";
import "./home-alerts.css";

export interface HomeAlertsProps {
  /** Where the offline banner's "View devices" goes -- the device list, not
      the separate pairing/setup flow: it's already paired, just quiet. */
  onNavigate: (section: GuiSection) => void;
}

/** "10:08 PM", matching HomeStatusBar's own clock format. */
function formatTime(iso: string): string {
  return new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

export function HomeAlerts({ onNavigate }: HomeAlertsProps) {
  const { devicesAreReal } = useHomeData();
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const [devices, setDevices] = useState<Device[]>([]);
  // Keyed on schedule id + its next_run, so a dismissal only lasts for that one occurrence --
  // once the schedule advances to a new next_run, the key no longer matches and it can show again.
  const [dismissed, setDismissed] = useState<string | null>(null);
  const [running, setRunning] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .listSchedules()
      .then((list) => {
        if (!cancelled) setSchedules(list);
      })
      .catch(() => {
        // Silent: an alert that failed to load is not a routine due tonight.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!devicesAreReal) return;
    let cancelled = false;
    api
      .listDevices()
      .then((list) => {
        if (!cancelled) setDevices(list);
      })
      .catch(() => {
        // Silent: same as above, no banner over no data.
      });
    return () => {
      cancelled = true;
    };
  }, [devicesAreReal]);

  const due = nextDueTonight(schedules);
  const dueKey = due ? `${due.id}:${due.next_run}` : null;
  const showDue = due !== null && dueKey !== dismissed;

  const offline = devices.find((d) => !d.is_online) ?? null;

  async function runNow(schedule: Schedule) {
    setRunning(schedule.id);
    try {
      await api.runScheduleNow(schedule.id);
    } catch {
      // Schedules already surfaces failures of its own; this banner just stops claiming it's due.
    }
    setRunning(null);
  }

  if (!showDue && !offline) return null;

  // InkBudget's 2 raised signals on Home are the composer and whichever alert shows first
  // (DESIGN.md §1 / plan §1) -- due always renders before offline, so it's "first" whenever
  // it's showing at all; offline is only first when it's the sole alert.
  const dueIsFirst = showDue;
  const offlineIsFirst = !showDue;

  return (
    <div className="halerts">
      {showDue && due && (
        <InkCard raised={dueIsFirst} className={`halert halert--warning${dueIsFirst ? "" : " halert--flat"}`}>
          <div className="halert__text">
            <span className="halert__kind">Routine</span>
            <span className="halert__msg">
              {/* Non-null: nextDueTonight only returns schedules with a truthy next_run. */}
              {due.label ?? due.name} runs at <span className="halert__time">{formatTime(due.next_run as string)}</span>
            </span>
          </div>
          <div className="halert__actions">
            <InkButton
              variant="primary"
              disabled={running === due.id}
              onPress={() => void runNow(due)}
            >
              Run now
            </InkButton>
            <InkButton variant="quiet" onPress={() => onNavigate("schedules")}>
              Open schedule
            </InkButton>
            <InkButton variant="quiet" onPress={() => setDismissed(dueKey)}>
              Dismiss
            </InkButton>
          </div>
        </InkCard>
      )}

      {offline && (
        <InkCard raised={offlineIsFirst} className={`halert halert--info${offlineIsFirst ? "" : " halert--flat"}`}>
          <div className="halert__text">
            <span className="halert__kind">Device</span>
            <span className="halert__msg">
              {offline.name} hasn't checked in
              <br />
              Last seen {timeSince(offline.last_seen)}
            </span>
          </div>
          <div className="halert__actions">
            <InkButton variant="quiet" onPress={() => onNavigate("devices")}>
              View devices
            </InkButton>
          </div>
        </InkCard>
      )}
    </div>
  );
}
