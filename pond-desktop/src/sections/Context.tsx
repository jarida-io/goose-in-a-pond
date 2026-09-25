// ─── Context: everything the pond knows about you ───────────────────────────
// Remembered (what you told it) stays apart from Collected (what it read from accounts): you correct a
// memory but disconnect a source. The wall matches Conversations' cards on purpose.

import { useCallback, useEffect, useMemo, useState } from "react";
import { Plus, Search, RefreshCw, Pencil, Trash2, Check, X } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { ContextIndexHealth, MemoryFragment } from "../api/types";
import { ConnectionsPanel } from "../connections/ConnectionsPanel";
import { useAppState } from "../state/AppContext";
import { Collected } from "./context/Collected";
import { Lineage } from "./context/Lineage";
import "../styles/context.css";

type View = "remembered" | "collected" | "sources" | "lineage";

const VIEWS: Array<{ id: View; label: string; blurb: string }> = [
  { id: "remembered", label: "Remembered", blurb: "What you told it" },
  { id: "collected", label: "Collected", blurb: "What it read from your accounts" },
  { id: "sources", label: "Sources", blurb: "Where else it may read" },
  { id: "lineage", label: "Lineage", blurb: "How it all connects" },
];

/** Only placeholders get a shape: a real card is as tall as its memory, never clipped to a guess. */
const GHOST_SHAPES = ["tile", "card", "column", "card", "tile", "card"] as const;

function relativeWhen(iso?: string): string {
  if (!iso) return "";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const mins = Math.max(0, Math.round((Date.now() - then) / 60000));
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days}d ago`;
  return `${Math.round(days / 30)}mo ago`;
}

export function Context() {
  const { sessionId } = useAppState();
  const [view, setView] = useState<View>("remembered");
  const [memories, setMemories] = useState<MemoryFragment[]>([]);
  const [health, setHealth] = useState<ContextIndexHealth | null>(null);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [confirmingId, setConfirmingId] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      // 500, not the default 20: the lineage view is meaningless over a slice.
      const [rows, index] = await Promise.all([
        api.listMemories(500),
        api.getContextIndexHealth().catch(() => null),
      ]);
      // For an empty body `request` returns undefined typed as T; coerce, or `.filter` white-screens.
      setMemories(Array.isArray(rows) ? rows : []);
      setHealth(index);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not read what the pond remembers.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const live = useMemo(
    () => memories.filter((m) => !m.lifecycle || m.lifecycle === "active"),
    [memories],
  );

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return live;
    return live.filter((m) => m.content.toLowerCase().includes(q));
  }, [live, query]);

  async function addMemory(event: React.FormEvent) {
    event.preventDefault();
    const text = draft.trim();
    if (!text) return;
    setAdding(true);
    try {
      await api.addMemory(text, undefined, undefined, undefined, undefined);
      setDraft("");
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not save that.");
    } finally {
      setAdding(false);
    }
  }

  async function saveEdit(id: string) {
    const text = editDraft.trim();
    const original = memories.find((m) => m.id === id);
    if (!text || text === original?.content) {
      setEditingId(null);
      return;
    }
    try {
      // In place, so the memory keeps its id, age and usage.
      await api.updateMemory(id, text);
      setEditingId(null);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not save that change.");
    }
  }

  async function removeMemory(id: string) {
    try {
      await api.deleteMemory(id);
      setConfirmingId(null);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not delete that.");
    }
  }

  return (
    <div className="ctx">
      <header className="ctx__head">
        <div>
          <h1 className="ctx__title">Context</h1>
          <p className="ctx__sub">
            {live.length === 0
              ? "Nothing kept yet."
              : `${live.length} thing${live.length === 1 ? "" : "s"} the pond is holding onto` +
                (memories.length > live.length
                  ? `, folded down from ${memories.length}.`
                  : ".")}
          </p>
        </div>
        <button type="button" className="ctx__refresh" onClick={() => void load()}>
          <RefreshCw size={15} />
          <span>Refresh</span>
        </button>
      </header>

      <nav className="ctx__views" aria-label="Context views">
        {VIEWS.map((v) => (
          <button
            key={v.id}
            type="button"
            className="ctx__view"
            aria-pressed={view === v.id}
            // Otherwise a screen reader runs the label and question spans together as the name.
            aria-label={v.label}
            onClick={() => setView(v.id)}
          >
            <span className="ctx__viewLabel">{v.label}</span>
            <span className="ctx__viewBlurb">{v.blurb}</span>
          </button>
        ))}
      </nav>

      {error && <p className="ctx__error">{error}</p>}

      {view === "remembered" && (
        <>
          <div className="ctx__tools">
            <label className="ctx__search">
              <Search size={15} aria-hidden="true" />
              <input
                className="ctx__searchInput"
                type="search"
                value={query}
                placeholder="Search what the pond remembers"
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
          </div>

          <form className="ctx__add" onSubmit={addMemory}>
            <input
              className="ctx__addInput"
              type="text"
              value={draft}
              placeholder="Tell the pond something worth keeping"
              onChange={(e) => setDraft(e.target.value)}
            />
            <button type="submit" className="ctx__addBtn" disabled={adding || !draft.trim()}>
              <Plus size={15} />
              <span>{adding ? "Saving" : "Remember"}</span>
            </button>
          </form>

          {loading ? (
            <div className="ctx__wall" aria-busy="true" aria-label="Loading">
              {GHOST_SHAPES.map((w, i) => (
                <div key={i} className="ctx__card ctx__card--ghost" data-weight={w} />
              ))}
            </div>
          ) : shown.length === 0 ? (
            <p className="ctx__empty">
              {query
                ? "Nothing here matches that."
                : "Nothing kept yet. Tell it something above, or connect a calendar under Sources."}
            </p>
          ) : (
            <div className="ctx__wall">
              {shown.map((m) => (
                <article key={m.id} className="ctx__card">
                  <div className="ctx__cardTop">
                    {m.tier && <span className="ctx__tier" data-tier={m.tier}>{m.tier}</span>}
                    <span className="ctx__when">{relativeWhen(m.created_at)}</span>
                  </div>

                  {editingId === m.id ? (
                    <>
                      <textarea
                        className="ctx__edit"
                        value={editDraft}
                        autoFocus
                        onChange={(e) => setEditDraft(e.target.value)}
                      />
                      <div className="ctx__cardActions">
                        <button type="button" onClick={() => void saveEdit(m.id)} aria-label="Save">
                          <Check size={14} />
                          <span>Save</span>
                        </button>
                        <button type="button" onClick={() => setEditingId(null)} aria-label="Cancel">
                          <X size={14} />
                          <span>Cancel</span>
                        </button>
                      </div>
                    </>
                  ) : confirmingId === m.id ? (
                    <>
                      <p className="ctx__cardBody">{m.content}</p>
                      <div className="ctx__cardActions">
                        <span className="ctx__confirmAsk">Forget this?</span>
                        <button
                          type="button"
                          className="ctx__danger"
                          onClick={() => void removeMemory(m.id)}
                        >
                          Forget
                        </button>
                        <button type="button" onClick={() => setConfirmingId(null)}>
                          Keep
                        </button>
                      </div>
                    </>
                  ) : (
                    <>
                      <p className="ctx__cardBody">{m.content}</p>
                      <div className="ctx__cardActions ctx__cardActions--hover">
                        <button
                          type="button"
                          onClick={() => {
                            setEditDraft(m.content);
                            setEditingId(m.id);
                          }}
                          aria-label="Edit this memory"
                        >
                          <Pencil size={14} />
                          <span>Edit</span>
                        </button>
                        <button
                          type="button"
                          onClick={() => setConfirmingId(m.id)}
                          aria-label="Forget this memory"
                        >
                          <Trash2 size={14} />
                          <span>Forget</span>
                        </button>
                      </div>
                    </>
                  )}
                </article>
              ))}
            </div>
          )}
        </>
      )}

      {view === "collected" && <Collected sessionId={sessionId} />}

      {view === "sources" && <ConnectionsPanel sessionId={sessionId} />}

      {view === "lineage" && (
        <Lineage memories={memories} health={health} onRebuilt={() => void load()} />
      )}
    </div>
  );
}
