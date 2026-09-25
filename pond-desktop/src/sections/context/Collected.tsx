// ─── Collected: what the pond has read from your accounts ───────────────────
// Separate from Remembered: a wrong memory is corrected, a wrong source disconnected.
// Search is by word, not meaning: a cosine ranking would bury an exact title match.

import { useCallback, useEffect, useMemo, useState } from "react";
import { CalendarDays, Mail, Search, Camera, Radio } from "lucide-react";
import { api } from "../../api/PondApiClient";
import type { ContextItem } from "../../api/types";

interface Props {
  sessionId: string | null;
}

function iconFor(sourceKind: string) {
  switch (sourceKind) {
    case "calendar":
      return <CalendarDays size={15} />;
    case "mail":
      return <Mail size={15} />;
    case "camera":
      return <Camera size={15} />;
    default:
      return <Radio size={15} />;
  }
}

/** Calendar events are often in the future, so this reads both ways. */
function when(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const mins = Math.round((t - Date.now()) / 60000);
  const ahead = mins > 0;
  const m = Math.abs(mins);
  if (m < 1) return "now";
  const say = (n: number, unit: string) =>
    ahead ? `in ${n} ${unit}${n === 1 ? "" : "s"}` : `${n} ${unit}${n === 1 ? "" : "s"} ago`;
  if (m < 60) return say(m, "minute");
  const h = Math.round(m / 60);
  if (h < 24) return say(h, "hour");
  const d = Math.round(h / 24);
  if (d < 30) return say(d, "day");
  return say(Math.round(d / 30), "month");
}

export function Collected({ sessionId }: Props) {
  const [items, setItems] = useState<ContextItem[]>([]);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const scopeId = sessionId ?? "context-setup";

  const load = useCallback(
    async (q?: string) => {
      setLoading(true);
      try {
        const result = await api.listContextItems(scopeId, q);
        // `request` returns undefined cast to T for an empty body.
        setItems(result?.items ?? []);
        setError(null);
      } catch (e) {
        setError(e instanceof Error ? e.message : "Could not read what the pond collected.");
      } finally {
        setLoading(false);
      }
    },
    [scopeId],
  );

  useEffect(() => {
    void load();
  }, [load]);

  // Search server-side: the list is capped, so a local filter would miss rows.
  useEffect(() => {
    const t = setTimeout(() => void load(query), 250);
    return () => clearTimeout(t);
  }, [query, load]);

  const unsearchable = useMemo(() => items.filter((i) => !i.searchable).length, [items]);

  return (
    <div className="coll">
      <div className="ctx__tools">
        <label className="ctx__search">
          <Search size={15} aria-hidden="true" />
          <input
            className="ctx__searchInput"
            type="search"
            value={query}
            placeholder="Search what the pond has read"
            onChange={(e) => setQuery(e.target.value)}
          />
        </label>
      </div>

      {error && <p className="ctx__error">{error}</p>}

      {unsearchable > 0 && (
        <p className="coll__note">
          {unsearchable} of these cannot be found by meaning yet — the pond is still reading
          them. Searching by word works on all of them.
        </p>
      )}

      {loading ? (
        <p className="ctx__empty">Reading…</p>
      ) : items.length === 0 ? (
        <p className="ctx__empty">
          {query
            ? "Nothing here matches that."
            : "Nothing collected yet. Connect a calendar or mailbox under Sources."}
        </p>
      ) : (
        <ul className="coll__list">
          {items.map((item) => (
            <li key={item.id} className="coll__item">
              <span className="coll__icon" aria-hidden="true">
                {iconFor(item.source_kind)}
              </span>
              <div className="coll__body">
                <div className="coll__head">
                  <strong className="coll__title">{item.title}</strong>
                  <span className="coll__when">{when(item.occurred_at)}</span>
                </div>
                {item.body && <p className="coll__text">{item.body}</p>}
                {item.participants.length > 0 && (
                  <p className="coll__with">With {item.participants.join(", ")}</p>
                )}
                {!item.searchable && (
                  <p className="coll__pending">Not searchable by meaning yet</p>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
