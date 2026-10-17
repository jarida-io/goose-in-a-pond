// Web search: the household's own Brave key. Shared by the sections UI and the settings hub.
// The key leaves only in the save request and is never read back; the field clears once stored.

import { useCallback, useEffect, useRef, useState } from "react";
import {
  CircleAlert,
  CircleCheck,
  Clock,
  Globe,
  LoaderCircle,
  RefreshCw,
  Trash2,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import type { WebSearchState, WebSearchStatus } from "../api/types";
import {
  BRAVE_DASHBOARD_HOST,
  BRAVE_DASHBOARD_URL,
  OLD_POND,
  PRIVACY_LINE,
  STATUS_LINES,
  describeProblem,
  readWebSearchState,
} from "./webSearch";
import "../styles/connections.css";

/** A shape per state as well as a tone, so the line never depends on hue alone. */
const STATUS_ICONS: Record<WebSearchStatus, LucideIcon> = {
  ok: CircleCheck,
  not_configured: Globe,
  rejected: CircleAlert,
  quota: TriangleAlert,
  unverified: Clock,
};

export function WebSearchPanel() {
  /** `null` until the pond has said, and again when it could not. */
  const [state, setState] = useState<WebSearchState | null>(null);
  const [loading, setLoading] = useState(true);
  const [readProblem, setReadProblem] = useState<string | null>(null);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<"save" | "remove" | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [focusOn, setFocusOn] = useState<{ to: "status" | "remove"; n: number } | null>(null);
  const statusRef = useRef<HTMLDivElement>(null);
  const removeRef = useRef<HTMLButtonElement>(null);
  const keepRef = useRef<HTMLButtonElement>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const next = readWebSearchState(await api.getWebSearch());
      setState(next);
      setReadProblem(next ? null : OLD_POND);
    } catch (e) {
      setState(null);
      setReadProblem(describeProblem(e, "read"));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (confirming) keepRef.current?.focus();
  }, [confirming]);

  useEffect(() => {
    if (!focusOn) return;
    (focusOn.to === "status" ? statusRef : removeRef).current?.focus();
  }, [focusOn]);

  const known = state !== null;
  const typed = key.trim();
  const canSave = known && busy === null && typed.length > 0;

  /** The control that held focus is disabled or gone once the outcome is in, so focus is moved. */
  function moveFocus(to: "status" | "remove") {
    setFocusOn((prev) => ({ to, n: (prev?.n ?? 0) + 1 }));
  }

  async function save(event: React.FormEvent) {
    event.preventDefault();
    if (!canSave) return;
    setBusy("save");
    setError(null);
    try {
      const stored = await api.setWebSearchKey(typed);
      const next = readWebSearchState(stored);
      if (!next?.configured) {
        setError(OLD_POND);
        return;
      }
      setKey("");
      setState(next);
      moveFocus("status");
    } catch (e) {
      setError(describeProblem(e, "save"));
    } finally {
      setBusy(null);
    }
  }

  async function remove() {
    setBusy("remove");
    setConfirming(false);
    setError(null);
    try {
      await api.deleteWebSearchKey();
      setState({ provider: "brave", configured: false, status: "not_configured" });
      moveFocus("status");
    } catch (e) {
      setError(describeProblem(e, "remove"));
      moveFocus("remove");
    } finally {
      setBusy(null);
    }
  }

  function keep() {
    setConfirming(false);
    moveFocus("remove");
  }

  async function tryAgain() {
    await refresh();
    moveFocus("status");
  }

  const line = state ? STATUS_LINES[state.status] : null;
  const StatusIcon = state ? STATUS_ICONS[state.status] : LoaderCircle;
  const tone = line?.tone ?? "muted";

  return (
    <section className="connections websearch" aria-labelledby="web-search-title">
      <div className="connections-intro">
        <h3 id="web-search-title">Web search</h3>
        <p>{PRIVACY_LINE}</p>
      </div>

      {!loading && !known ? (
        <div className="websearch-problem">
          <p className="connections-detail" role="alert">
            {readProblem}
          </p>
          <button type="button" className="connections-sync" onClick={() => void tryAgain()}>
            <RefreshCw size={14} aria-hidden="true" />
            Try again
          </button>
        </div>
      ) : (
        <div className={`connections-item websearch-status websearch-status--${tone}`}>
          <span className="connections-icon" aria-hidden="true">
            <StatusIcon size={18} className={loading ? "connections-spin" : undefined} />
          </span>
          <div ref={statusRef} className="websearch-state" tabIndex={-1}>
            {loading ? "Reading the setting…" : line?.label}
          </div>
          {state?.configured &&
            (confirming ? (
              <div
                className="websearch-actions"
                role="group"
                aria-label="Remove the key?"
                onKeyDown={(e) => {
                  if (e.key === "Escape") keep();
                }}
              >
                <span className="websearch-ask">Remove the key?</span>
                <button
                  type="button"
                  className="connections-disconnect websearch-danger"
                  onClick={() => void remove()}
                >
                  Remove
                </button>
                <button ref={keepRef} type="button" className="connections-sync" onClick={keep}>
                  Keep
                </button>
              </div>
            ) : (
              <button
                ref={removeRef}
                type="button"
                className="connections-disconnect"
                onClick={() => setConfirming(true)}
                disabled={busy !== null}
                aria-label="Remove the Brave key"
              >
                <Trash2 size={16} aria-hidden="true" />
                <span>Remove</span>
              </button>
            ))}
        </div>
      )}

      <form className="connections-form" onSubmit={save}>
        <p className="connections-hint">
          Get a free key at{" "}
          <a href={BRAVE_DASHBOARD_URL} target="_blank" rel="noopener noreferrer">
            {BRAVE_DASHBOARD_HOST}
          </a>
          . The free monthly credit covers about 1,000 searches. Brave asks for a card to
          confirm who you are.
        </p>

        <label className="connections-field">
          <span>Brave Search key</span>
          <input
            className="websearch-key"
            type="password"
            name="brave-search-key"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            value={key}
            disabled={!known || busy !== null}
            placeholder={state?.configured ? "Paste a new key to replace it" : "Paste your key"}
            aria-invalid={error === null ? undefined : true}
            aria-describedby={error === null ? undefined : "web-search-error"}
            onChange={(e) => {
              setKey(e.target.value);
              setError(null);
            }}
          />
        </label>

        <button type="submit" className="connections-submit" disabled={!canSave}>
          {busy === "save" ? (
            <RefreshCw size={16} className="connections-spin" aria-hidden="true" />
          ) : null}
          <span>{busy === "save" ? "Saving…" : "Save"}</span>
        </button>

        {busy === "save" && (
          <p className="connections-muted" role="status">
            Checking the key with Brave…
          </p>
        )}
        {error && (
          <p className="connections-error" id="web-search-error" role="alert">
            {error}
          </p>
        )}
      </form>
    </section>
  );
}
