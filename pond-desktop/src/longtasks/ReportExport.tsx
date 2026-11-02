// Keeping a finished report: as Markdown, as a web page, on paper or as a PDF, or its dates in a calendar.

import { useCallback, useEffect, useId, useRef, useState } from "react";
import { CalendarPlus, Download, FileText, Globe, Printer, RotateCw } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import "./report.css";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import type { LongTask, LongTaskExportFormat } from "../api/types";
import { fileNameFor, saveBlob } from "../lib/saveFile";
import { printHtml } from "../lib/printHtml";
import type { LongTaskReportHandle } from "./useLongTaskReport";
import {
  ADD_TO_CALENDAR,
  ADD_TO_CALENDAR_HINT,
  CANT_CHECK_SAVE,
  CHECKING_SAVE,
  GETTING_FILE,
  NO_DATED_EVENTS,
  ONLY_MARKDOWN,
  PRINT_REPORT,
  PRINT_REPORT_HINT,
  SAVE_MARKDOWN,
  SAVE_MARKDOWN_HINT,
  SAVE_OR_PRINT,
  SAVE_WEB_PAGE,
  SAVE_WEB_PAGE_HINT,
  TRY_AGAIN,
  couldntGetFile,
  couldntPrint,
  fileReady,
} from "./reportWords";

type Kind = "markdown" | "html" | "print" | "calendar";

const EXTENSION: Record<LongTaskExportFormat, string> = { markdown: "md", html: "html", ics: "ics" };

interface Item {
  key: string;
  Icon: LucideIcon;
  label: string;
  sub: string | null;
  /** Nothing to press: it says what is going on, or why there is no more. */
  inert: boolean;
  run: () => void;
}

interface Props {
  task: Pick<LongTask, "id" | "goal">;
  handle: LongTaskReportHandle;
  variant: "classic" | "hub";
  /** `inline` opens the menu in the flow, under the buttons: a card sits in a scrolling thread, where a popover would be cut off. */
  placement: "inline" | "popover";
}

function reasonOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function ReportExport({ task, handle, variant, placement }: Props) {
  const { state, load } = handle;
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const mounted = useRef(true);
  const menuId = useId();

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const report = state.status === "ready" ? state.report : null;
  const title = report?.title ?? task.goal;

  const save = useCallback(
    async (format: LongTaskExportFormat) => {
      if (report?.summaryOnly) {
        // An older pond cannot make the file, so the text already in hand is saved as it is.
        const name = fileNameFor(title, EXTENSION.markdown);
        saveBlob(new Blob([report.markdown], { type: "text/markdown;charset=utf-8" }), name);
        return fileReady(name);
      }
      const { blob, filename } = await api.getLongTaskExport(task.id, format);
      const name = filename ?? fileNameFor(title, EXTENSION[format]);
      saveBlob(blob, name);
      return fileReady(name);
    },
    [report, task.id, title],
  );

  const run = useCallback(
    async (kind: Kind) => {
      setBusy(true);
      setNote(null);
      setProblem(null);
      try {
        if (kind === "print") {
          const { blob } = await api.getLongTaskExport(task.id, "html");
          await printHtml(await blob.text());
        } else {
          const done = await save(kind === "calendar" ? "ics" : kind);
          if (mounted.current) setNote(done);
        }
      } catch (e) {
        const calendarWithNone = kind === "calendar" && e instanceof ApiError && e.status === 404;
        if (mounted.current) {
          setProblem(calendarWithNone ? NO_DATED_EVENTS : kind === "print" ? couldntPrint(reasonOf(e)) : couldntGetFile(reasonOf(e)));
        }
      } finally {
        if (mounted.current) {
          setBusy(false);
          // Printing moves focus into the hidden frame, which is gone once the dialog is.
          if (kind === "print") triggerRef.current?.focus();
        }
      }
    },
    [save, task.id],
  );

  const items: Item[] = [];
  if (report) {
    items.push({ key: "markdown", Icon: FileText, label: SAVE_MARKDOWN, sub: SAVE_MARKDOWN_HINT, inert: false, run: () => void run("markdown") });
    if (report.summaryOnly) {
      items.push({ key: "only", Icon: Globe, label: ONLY_MARKDOWN, sub: null, inert: true, run: () => {} });
    } else {
      items.push({ key: "html", Icon: Globe, label: SAVE_WEB_PAGE, sub: SAVE_WEB_PAGE_HINT, inert: false, run: () => void run("html") });
      items.push({ key: "print", Icon: Printer, label: PRINT_REPORT, sub: PRINT_REPORT_HINT, inert: false, run: () => void run("print") });
      if (report.calendarEvents > 0) {
        items.push({ key: "calendar", Icon: CalendarPlus, label: ADD_TO_CALENDAR, sub: ADD_TO_CALENDAR_HINT, inert: false, run: () => void run("calendar") });
      }
    }
  } else if (state.status === "failed") {
    items.push({ key: "retry", Icon: RotateCw, label: TRY_AGAIN, sub: `${CANT_CHECK_SAVE} ${state.message}`, inert: false, run: () => void load() });
  } else {
    items.push({ key: "checking", Icon: RotateCw, label: CHECKING_SAVE, sub: null, inert: true, run: () => {} });
  }

  const close = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) triggerRef.current?.focus();
  }, []);

  const focusItem = useCallback((at: "first" | "last" | number) => {
    const list = itemRefs.current.filter((el): el is HTMLButtonElement => el !== null);
    if (list.length === 0) return;
    const index = at === "first" ? 0 : at === "last" ? list.length - 1 : (at + list.length) % list.length;
    list[index].focus();
  }, []);

  // Refocused when what the menu holds changes, or a retry would leave focus on an item that is gone.
  const landOn = useRef<"first" | "last">("first");
  useEffect(() => {
    if (open) focusItem(landOn.current);
  }, [open, state.status, focusItem]);

  useEffect(() => {
    if (!open) return;
    function away(e: Event) {
      if (!wrapRef.current?.contains(e.target as Node)) close(false);
    }
    // Capture, and stop the event: a reading view around this menu closes on Escape too.
    function onEscape(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      close(true);
    }
    document.addEventListener("mousedown", away);
    document.addEventListener("focusin", away);
    document.addEventListener("keydown", onEscape, true);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("focusin", away);
      document.removeEventListener("keydown", onEscape, true);
    };
  }, [open, close]);

  function openMenu(land: "first" | "last") {
    landOn.current = land;
    setOpen(true);
    if (state.status === "idle") void load();
  }

  function onTriggerClick() {
    if (open) close(true);
    else openMenu("first");
  }

  function onTriggerKeyDown(e: React.KeyboardEvent<HTMLButtonElement>) {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const land = e.key === "ArrowDown" ? "first" : "last";
    if (open) focusItem(land);
    else openMenu(land);
  }

  function onPanelKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    const at = itemRefs.current.findIndex((el) => el === document.activeElement);
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        focusItem(at + 1);
        break;
      case "ArrowUp":
        e.preventDefault();
        focusItem(at < 0 ? "last" : at - 1);
        break;
      case "Home":
        e.preventDefault();
        focusItem("first");
        break;
      case "End":
        e.preventDefault();
        focusItem("last");
        break;
      case "Tab":
        // Focus goes back to the button first, so the Tab itself moves on from there.
        close(true);
        break;
    }
  }

  function choose(item: Item) {
    if (item.inert || busy) return;
    // A retry keeps the menu open to show what it finds; a save closes it.
    if (item.key !== "retry") close(true);
    item.run();
  }

  const status = busy ? GETTING_FILE : note;

  return (
    <>
      <div ref={wrapRef} className={`rexp rexp--${variant} rexp--${placement}`}>
        <button
          ref={triggerRef}
          type="button"
          className="ink-btn ink-btn--quiet rexp__btn"
          aria-haspopup="menu"
          aria-expanded={open}
          aria-controls={open ? menuId : undefined}
          aria-busy={busy || undefined}
          onClick={onTriggerClick}
          onKeyDown={onTriggerKeyDown}
        >
          <Download size={14} aria-hidden="true" />
          {SAVE_OR_PRINT}
        </button>

        {open && (
          <div
            id={menuId}
            role="menu"
            aria-label={SAVE_OR_PRINT}
            className={`rexp__panel rexp__panel--${placement}`}
            onKeyDown={onPanelKeyDown}
          >
            {items.map((item, i) => {
              const labelId = `${menuId}-${item.key}`;
              const subId = `${labelId}-why`;
              return (
                <button
                  key={item.key}
                  ref={(el) => {
                    itemRefs.current[i] = el;
                  }}
                  type="button"
                  role="menuitem"
                  className="rexp__item"
                  tabIndex={-1}
                  aria-labelledby={labelId}
                  aria-describedby={item.sub ? subId : undefined}
                  aria-disabled={item.inert ? true : undefined}
                  data-inert={item.inert ? "true" : undefined}
                  onClick={() => choose(item)}
                >
                  <item.Icon size={16} aria-hidden="true" className="rexp__icon" />
                  <span className="rexp__text">
                    <span id={labelId} className="rexp__label">
                      {item.label}
                    </span>
                    {item.sub && (
                      <span id={subId} className="rexp__sub">
                        {item.sub}
                      </span>
                    )}
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </div>

      {status ? (
        <p className="rexp__note" role="status">
          {status}
        </p>
      ) : null}
      {problem ? (
        <p className="rexp__problem" role="alert">
          {problem}
        </p>
      ) : null}
    </>
  );
}
