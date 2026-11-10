// A report's citations as small numbered chips in the text, and every source in one collapsed row at the end.

import { useEffect, useId, useRef, useState } from "react";
import { ChevronDown, ExternalLink } from "lucide-react";
import type { LongTaskSource } from "../api/types";
import { siteOf } from "./sourceNumbers";
import { SOURCES_LABEL, openSite, sourceChipLabel, sourcesRow } from "./reportWords";

export function SourceChip({ n, source }: { n: number; source: LongTaskSource }) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement>(null);
  const chipRef = useRef<HTMLButtonElement>(null);
  const cardId = useId();
  const site = siteOf(source.url);

  useEffect(() => {
    if (!open) return;
    function away(e: Event) {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    }
    // Capture, and stop it: the reading sheet around the chip closes on Escape too.
    function onEscape(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
      chipRef.current?.focus();
    }
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", onEscape, true);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", onEscape, true);
    };
  }, [open]);

  return (
    <span ref={wrapRef} className="rcite">
      <button
        ref={chipRef}
        type="button"
        className="rcite__chip reach"
        aria-label={sourceChipLabel(n, source.title)}
        aria-expanded={open}
        aria-controls={open ? cardId : undefined}
        onClick={() => setOpen((v) => !v)}
      >
        {n}
      </button>
      {open ? (
        <span id={cardId} className="rcite__card" role="group" aria-label={sourceChipLabel(n, source.title)}>
          <span className="rcite__title">{source.title}</span>
          <span className="rcite__site">{site}</span>
          <a className="rcite__open" href={source.url} target="_blank" rel="noopener noreferrer">
            <ExternalLink size={14} aria-hidden="true" />
            {openSite(site)}
          </a>
        </span>
      ) : null}
    </span>
  );
}

export function SourcesRow({ sources }: { sources: LongTaskSource[] }) {
  const [open, setOpen] = useState(false);
  const listId = useId();
  if (sources.length === 0) return null;
  return (
    <section className="rsrc" aria-label={SOURCES_LABEL}>
      <button
        type="button"
        className="rsrc__toggle"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        onClick={() => setOpen((v) => !v)}
      >
        {sourcesRow(sources.length)}
        <ChevronDown size={16} aria-hidden="true" className={`rsrc__caret${open ? " is-open" : ""}`} />
      </button>
      {open ? (
        <ol id={listId} className="rsrc__list">
          {sources.map((source, i) => (
            <li key={source.url} className="rsrc__item">
              <span className="rsrc__n" aria-hidden="true">
                {i + 1}
              </span>
              <span className="rsrc__text">
                <a href={source.url} target="_blank" rel="noopener noreferrer">
                  {source.title}
                </a>
                <span className="rsrc__site">{siteOf(source.url)}</span>
              </span>
            </li>
          ))}
        </ol>
      ) : null}
    </section>
  );
}
