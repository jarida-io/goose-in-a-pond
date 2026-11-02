// A finished report to read at length: its own sheet over the chat, with its own scroll and a way to jump to a section.

import { useCallback, useId, useMemo, useRef } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";
import "./report.css";
import type { LongTask } from "../api/types";
import { MarkdownText } from "../components/MarkdownText";
import { useDialogFocusTrap } from "../components/shared";
import { blockId, outlineOf, parseMarkdown } from "../lib/markdownBlocks";
import { ReportExport } from "./ReportExport";
import { CLOSE_REPORT, REPORT_SECTIONS_LABEL, REPORT_TEXT_LABEL, reportCounts } from "./reportWords";
import { withoutTitle } from "./longTaskWords";
import type { LongTaskReportHandle, ReportView } from "./useLongTaskReport";

/** Deeper headings are a section's own parts, so the outline stops at the third level. */
const OUTLINE_DEPTH = 3;

interface Props {
  task: Pick<LongTask, "id" | "goal">;
  report: ReportView;
  handle: LongTaskReportHandle;
  variant: "classic" | "hub";
  onClose: () => void;
}

export function ReportSheet({ task, report, handle, variant, onClose }: Props) {
  const titleId = useId();
  const prefix = useId();
  const scrollRef = useRef<HTMLDivElement>(null);
  // A new `onClose` every render would restart the focus trap, which hands focus back to the opener and takes it again.
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const close = useCallback(() => closeRef.current(), []);
  // The text takes focus first, so the arrow and page keys read it at once; Escape and Tab stay inside.
  const dialogRef = useDialogFocusTrap<HTMLDivElement>(true, close, scrollRef);

  const markdown = useMemo(() => withoutTitle(report.markdown, report.title), [report.markdown, report.title]);
  const outline = useMemo(() => outlineOf(parseMarkdown(markdown)).filter((entry) => entry.level <= OUTLINE_DEPTH), [markdown]);
  const shallowest = Math.min(...outline.map((entry) => entry.level));
  const counts = reportCounts(report.sections, report.sources.length);

  // A pond whose markdown does not list what it cites still gets its sources shown.
  const sourcesApart = useMemo(
    () =>
      report.sources.some((source) => markdown.includes(source.url))
        ? []
        : report.sources.filter((source) => /^https?:\/\//i.test(source.url)),
    [report.sources, markdown],
  );

  function goTo(index: number) {
    const heading = document.getElementById(blockId(prefix, index));
    if (!heading) return;
    heading.scrollIntoView({ block: "start" });
    heading.focus({ preventScroll: true });
  }

  return createPortal(
    <div
      className="rsheet-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div ref={dialogRef} className={`rsheet rsheet--${variant}`} role="dialog" aria-modal="true" aria-labelledby={titleId} tabIndex={-1}>
        <header className="rsheet__head">
          <div className="rsheet__titles">
            <h2 id={titleId} className="rsheet__title">
              {report.title}
            </h2>
            {counts ? <p className="rsheet__counts">{counts}</p> : null}
          </div>
          <div className="rsheet__actions">
            <ReportExport task={task} handle={handle} variant={variant} placement="popover" />
            <button type="button" className="ink-btn ink-btn--quiet rsheet__btn" aria-label={CLOSE_REPORT} onClick={close}>
              <X size={16} aria-hidden="true" />
              Close
            </button>
          </div>
        </header>

        <div className="rsheet__body">
          {outline.length >= 2 ? (
            <nav className="rsheet__nav" aria-label={REPORT_SECTIONS_LABEL}>
              <p className="rsheet__navlabel" aria-hidden="true">
                {REPORT_SECTIONS_LABEL}
              </p>
              <ol className="rsheet__navlist">
                {outline.map((entry) => (
                  <li key={entry.index}>
                    <button type="button" className="rsheet__navbtn" data-depth={entry.level - shallowest} onClick={() => goTo(entry.index)}>
                      {entry.text}
                    </button>
                  </li>
                ))}
              </ol>
            </nav>
          ) : null}

          <div ref={scrollRef} className="rsheet__scroll" role="region" aria-label={REPORT_TEXT_LABEL} tabIndex={0}>
            <div className="rsheet__read">
              <MarkdownText source={markdown} headings="report" idPrefix={prefix} />
              {sourcesApart.length > 0 ? (
                <section className="rsheet__sources" aria-label="Sources">
                  <h3 className="md__h md__h2">Sources</h3>
                  <ul className="md__list">
                    {sourcesApart.map((source) => (
                      <li key={source.url}>
                        <a href={source.url} target="_blank" rel="noopener noreferrer">
                          {source.title}
                        </a>
                      </li>
                    ))}
                  </ul>
                </section>
              ) : null}
            </div>
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}
