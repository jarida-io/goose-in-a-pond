// Web search: the household's own Brave key. Shared by the sections UI and the settings hub.
// The key leaves only in the save request and is never read back; the field clears once stored.

import { useCallback, useContext, useEffect, useRef, useState } from "react";
import {
  CircleAlert,
  CircleCheck,
  Clock,
  Globe,
  LoaderCircle,
  MapPin,
  RefreshCw,
  Trash2,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import type { WebSearchState, WebSearchStatus } from "../api/types";
import { DispatchCtx } from "../state/useAppContext";
import {
  BRAVE_DASHBOARD_HOST,
  BRAVE_DASHBOARD_URL,
  NEARBY_FAILED,
  NEARBY_LABEL,
  NEARBY_OLD_POND,
  NEARBY_SET_LOCATION,
  OLD_POND,
  PRIVACY_LINE,
  STATUS_LINES,
  describeProblem,
  nearbyDescription,
  readWebSearchState,
  townSentence,
  type LocalResults,
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
  /** `null` while unread and from a pond that predates nearby places. */
  const [local, setLocal] = useState<LocalResults | null>(null);
  const [placesSaving, setPlacesSaving] = useState(false);
  const [placesError, setPlacesError] = useState<string | null>(null);
  // Absent in a bare mount; the only place a location is set is the classic Settings screen.
  const dispatch = useContext(DispatchCtx);
  const statusRef = useRef<HTMLDivElement>(null);
  const removeRef = useRef<HTMLButtonElement>(null);
  const keepRef = useRef<HTMLButtonElement>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const next = readWebSearchState(await api.getWebSearch());
      setState(next);
      setLocal(next?.local_results ?? null);
      setReadProblem(next ? null : OLD_POND);
    } catch (e) {
      setState(null);
      setLocal(null);
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

  async function toggleNearby() {
    if (!local || placesSaving) return;
    const next = !local.enabled;
    // Turning it on with no town would send nothing, so it is refused rather than saved as a promise.
    if (next && !local.town) return;
    setPlacesSaving(true);
    setPlacesError(null);
    setLocal({ ...local, enabled: next });
    try {
      const saved = await api.updateSettings({ web_search_local_results: next });
      const stored = saved?.web_search_local_results;
      if (typeof stored === "boolean" && stored !== next) {
        setLocal((now) => (now ? { ...now, enabled: stored } : now));
      }
    } catch {
      setLocal((now) => (now ? { ...now, enabled: !next } : now));
      setPlacesError(NEARBY_FAILED);
    } finally {
      setPlacesSaving(false);
    }
  }

  async function tryAgain() {
    await refresh();
    moveFocus("status");
  }

  const line = state ? STATUS_LINES[state.status] : null;
  const StatusIcon = state ? STATUS_ICONS[state.status] : LoaderCircle;
  const tone = line?.tone ?? "muted";
  const nearbyOn = local?.enabled ?? false;
  const nearbyBlocked = !local || (!nearbyOn && !local.town);
  const townLine = townSentence(local);

  return (
    <section className="connections websearch" aria-labelledby="web-search-title">
      <div className="connections-intro">
        <h3 id="web-search-title">Web search</h3>
        <p>
          {PRIVACY_LINE}
          {townLine ? ` ${townLine}` : null}
        </p>
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

      {known && (
        <div className="connections-item websearch-places">
          <span className="connections-icon" aria-hidden="true">
            <MapPin size={18} />
          </span>
          <div className="websearch-places__text">
            <div id="web-search-nearby-label" className="websearch-places__label">
              {NEARBY_LABEL}
            </div>
            <p id="web-search-nearby-hint" className="websearch-places__hint">
              {local ? nearbyDescription(local.town, local.enabled) : NEARBY_OLD_POND}
            </p>
            {local && !local.town && dispatch && (
              <button
                type="button"
                className="connections-sync"
                onClick={() => dispatch({ type: "SET_SECTION", payload: "settings" })}
              >
                {NEARBY_SET_LOCATION}
              </button>
            )}
            {placesError && (
              <p className="connections-error" id="web-search-nearby-error" role="alert">
                {placesError}
              </p>
            )}
          </div>
          <button
            type="button"
            role="switch"
            className="websearch-switch"
            aria-checked={nearbyOn}
            aria-labelledby="web-search-nearby-label"
            aria-describedby={
              placesError
                ? "web-search-nearby-hint web-search-nearby-error"
                : "web-search-nearby-hint"
            }
            aria-disabled={nearbyBlocked || undefined}
            aria-busy={placesSaving || undefined}
            onClick={() => void toggleNearby()}
          >
            <span className="websearch-switch__track" aria-hidden="true">
              <span className="websearch-switch__thumb" />
            </span>
          </button>
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
