// A finished task's report, fetched the first time something asks for it and kept for the card's life.

import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api/PondApiClient";
import type { LongTask, LongTaskSource } from "../api/types";
import { resultOf } from "./longTaskWords";

export interface ReportView {
  title: string;
  markdown: string;
  sections: number;
  sources: LongTaskSource[];
  /** Dated events the report holds; a calendar file is offered only above 0. */
  calendarEvents: number;
  /** An older pond has no report route, so this is only the Result and the pond cannot save or print it. */
  summaryOnly: boolean;
}

export type ReportState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; report: ReportView }
  | { status: "failed"; message: string };

export interface LongTaskReportHandle {
  state: ReportState;
  /** Resolves with the report, or `null` after setting `state` to the reason it could not be had. */
  load: () => Promise<ReportView | null>;
}

type Source = Pick<LongTask, "id" | "artifacts" | "result_artifact_id">;

function reasonOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

async function fetchReport(task: Source): Promise<ReportView> {
  // A partial api mock may lack the method; a sync throw reads as a failed request.
  const report = await Promise.resolve().then(() => api.getLongTaskReport(task.id));
  if (report) {
    return {
      title: report.title,
      markdown: report.markdown,
      sections: report.sections,
      sources: report.sources,
      calendarEvents: report.calendar_events,
      summaryOnly: false,
    };
  }
  const result = resultOf(task);
  if (!result) throw new Error("This task has no result to open.");
  const body = await api.getLongTaskArtifact(task.id, result.id);
  return { title: body.title, markdown: body.body, sections: 0, sources: [], calendarEvents: 0, summaryOnly: true };
}

export function useLongTaskReport(task: Source): LongTaskReportHandle {
  const [state, setState] = useState<ReportState>({ status: "idle" });
  const taskRef = useRef(task);
  taskRef.current = task;
  const cached = useRef<ReportView | null>(null);
  const inflight = useRef<Promise<ReportView | null> | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = useCallback((): Promise<ReportView | null> => {
    if (cached.current) return Promise.resolve(cached.current);
    if (inflight.current) return inflight.current;
    setState({ status: "loading" });
    const run = (async (): Promise<ReportView | null> => {
      try {
        const report = await fetchReport(taskRef.current);
        cached.current = report;
        if (mounted.current) setState({ status: "ready", report });
        return report;
      } catch (e) {
        if (mounted.current) setState({ status: "failed", message: reasonOf(e) });
        return null;
      } finally {
        inflight.current = null;
      }
    })();
    inflight.current = run;
    return run;
  }, []);

  return { state, load };
}
