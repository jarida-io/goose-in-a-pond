// Folds a run of identical consecutive events into one row with a count, so two dozen
// Spotify calls in a row read as one line instead of a wall.

import type { ActivityEvent } from "../../api/types";
import { describeEvent, type EventDescription } from "./describe";

/** One row of the feed: the newest event of a run, its description, and how many it stands for. */
export interface ActivityRow {
  /** The newest event in the run; the feed is newest first, so this is the run's first event. */
  event: ActivityEvent;
  description: EventDescription;
  /** Events in the run, at least 1. */
  count: number;
}

/** Two events read the same when a householder would see the same row for both. */
function sameRow(a: ActivityRow, event: ActivityEvent, description: EventDescription): boolean {
  return (
    a.event.category === event.category &&
    a.event.privacy_sensitivity === event.privacy_sensitivity &&
    a.description.title === description.title &&
    a.description.detail === description.detail
  );
}

/** Group consecutive events that read the same; order is preserved. */
export function groupRepeats(events: ActivityEvent[]): ActivityRow[] {
  const rows: ActivityRow[] = [];
  for (const event of events) {
    const description = describeEvent(event);
    const last = rows[rows.length - 1];
    if (last && sameRow(last, event, description)) {
      last.count += 1;
    } else {
      rows.push({ event, description, count: 1 });
    }
  }
  return rows;
}

/** "Goose reached Spotify for music, 24 times"; a single event keeps its title. */
export function rowTitle(row: ActivityRow): string {
  return row.count > 1 ? `${row.description.title}, ${row.count} times` : row.description.title;
}
