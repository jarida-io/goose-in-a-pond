import { useMemo, useState } from "react";
import { Bird, PenSquare, Search, Trash2 } from "lucide-react";
import type { SessionSummary } from "../api/types";
import {
  cardWeight, relativeWhen, messageCountLabel, matchesQuery, matchesSurface, surfaceLabel, isGooseThread,
  SURFACE_FILTERS, type SurfaceFilter,
} from "./chatHistoryView";
import "../styles/chat-history.css";

// A wall of conversation cards, as tall as each conversation was long. Newest-first reading order rules
// out CSS multi-column masonry (it fills columns top to bottom); row spans keep the order.

/** The opened card's centre, in pixels relative to the pane. */
export interface OpenOrigin {
  x: number;
  y: number;
}

interface Props {
  sessions: SessionSummary[];
  loading: boolean;
  /** The conversation to open, and the point the chat should grow out of. */
  onOpen: (id: string, origin: OpenOrigin) => void;
  onNewChat: () => void;
  onDelete: (id: string) => void;
}

export function ChatHistory({ sessions, loading, onOpen, onNewChat, onDelete }: Props) {
  const [query, setQuery] = useState("");
  const [confirmingDelete, setConfirmingDelete] = useState<string | null>(null);

  const [surface, setSurface] = useState<SurfaceFilter>("all");

  const visible = useMemo(
    () => sessions.filter((s) => matchesSurface(s, surface) && matchesQuery(s, query)),
    [sessions, query, surface],
  );

  function open(session: SessionSummary, event: React.MouseEvent<HTMLElement>) {
    const card = event.currentTarget.getBoundingClientRect();
    // Pane-relative: `transform-origin` is measured from the pane-filling chat's own box, not the viewport.
    const pane = event.currentTarget.closest(".chist")?.getBoundingClientRect();
    onOpen(session.id, {
      x: card.left + card.width / 2 - (pane?.left ?? 0),
      y: card.top + card.height / 2 - (pane?.top ?? 0),
    });
  }

  return (
    <div className="chist">
      <header className="chist__head">
        <div>
          <h1 className="chist__title">Conversations</h1>
          <p className="chist__sub">
            {sessions.length === 0
              ? "Nothing here yet."
              : `${sessions.length} on this device, and nowhere else.`}
          </p>
        </div>
        <div className="chist__actions">
          <label className="chist__search">
            <Search size={15} aria-hidden="true" />
            <input
              type="search"
              className="chist__searchInput"
              aria-label="Search conversations"
              placeholder="Search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          <button type="button" className="chist__new" onClick={onNewChat}>
            <PenSquare size={15} aria-hidden="true" />
            <span>New chat</span>
          </button>
        </div>
      </header>

      <div className="chist__filters" role="group" aria-label="Where the conversation started">
        {SURFACE_FILTERS.map((f) => (
          <button
            key={f.id}
            type="button"
            className="chist__filter"
            aria-pressed={surface === f.id}
            onClick={() => setSurface(f.id)}
          >
            {f.label}
          </button>
        ))}
      </div>

      {loading && (
        <div className="chist__grid" aria-busy="true" aria-label="Loading conversations">
          {(["card", "tile", "column", "tile", "card", "tile"] as const).map((w, i) => (
            <div key={i} className="ink-edge ink-card chist__card chist__card--ghost" data-weight={w} />
          ))}
        </div>
      )}

      {!loading && sessions.length === 0 && (
        <div className="chist__empty">
          <p className="chist__emptyLine">No conversations yet.</p>
          <button type="button" className="chist__new chist__new--lg" onClick={onNewChat}>
            <PenSquare size={16} aria-hidden="true" />
            <span>Start one</span>
          </button>
        </div>
      )}

      {!loading && sessions.length > 0 && visible.length === 0 && (
        <p className="chist__empty chist__emptyLine">
          {query.trim() ? `Nothing matches “${query.trim()}”.` : "No conversations started there yet."}
        </p>
      )}

      {!loading && visible.length > 0 && (
        <div className="chist__grid">
          {visible.map((session) => {
            const title = session.title?.trim() || "Untitled";
            const count = messageCountLabel(session.message_count);
            const from = surfaceLabel(session);
            const goose = isGooseThread(session);
            return (
              <article
                key={session.id}
                className="ink-edge ink-card ink-pressable chist__card"
                data-weight={cardWeight(session)}
                onClick={(e) => open(session, e)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    open(session, e as unknown as React.MouseEvent<HTMLElement>);
                  }
                }}
                role="button"
                tabIndex={0}
                aria-label={`Open conversation: ${title}`}
              >
                <span className="chist__when">
                  {goose && <Bird size={13} className="chist__gooseIcon" aria-label="The Goose thread" role="img" />}
                  {relativeWhen(session.updated_at)}
                </span>
                {goose
                  ? <span className="chist__from">What Goose did by itself</span>
                  : from && <span className="chist__from">Started on: {from}</span>}
                <h2 className="chist__cardTitle">{title}</h2>
                {session.preview && <p className="chist__preview">{session.preview}</p>}
                {count && <span className="chist__count">{count}</span>}

                {confirmingDelete === session.id ? (
                  <span className="chist__confirm">
                    <button
                      type="button"
                      className="chist__confirmYes"
                      aria-label={`Delete conversation: ${title}`}
                      onClick={(e) => { e.stopPropagation(); onDelete(session.id); setConfirmingDelete(null); }}
                    >
                      Delete
                    </button>
                    <button
                      type="button"
                      className="chist__confirmNo"
                      aria-label="Keep conversation"
                      onClick={(e) => { e.stopPropagation(); setConfirmingDelete(null); }}
                    >
                      Keep
                    </button>
                  </span>
                ) : (
                  <button
                    type="button"
                    className="chist__del"
                    aria-label={`Delete conversation: ${title}`}
                    onClick={(e) => { e.stopPropagation(); setConfirmingDelete(session.id); }}
                  >
                    <Trash2 size={14} aria-hidden="true" />
                  </button>
                )}
              </article>
            );
          })}
        </div>
      )}
    </div>
  );
}
