// ─── Connecting a calendar or mailbox ───────────────────────────────────────
// Shared by the sections UI and the settings hub. The password leaves only in the connect
// request: never in app state or logs, and cleared as soon as it returns.

import { useCallback, useEffect, useMemo, useState } from "react";
import { CalendarDays, Mail, Trash2, RefreshCw } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { ContextSource } from "../api/types";
import {
  PROVIDERS,
  describeHaul,
  describeLastSync,
  describeSourceOutcome,
  describeStatus,
  type ProviderOption,
} from "./providers";
import "../styles/connections.css";

interface Props {
  /** Which conversation this is being done in. The OWNER is resolved from it. */
  sessionId: string | null;
}

/** Sent when no conversation is open (the server resolves the owner). A real session id is
 *  preferred: in a multi-member household it says whose account this is. */
const NO_CONVERSATION = "context-setup";

export function ConnectionsPanel({ sessionId }: Props) {
  const [sources, setSources] = useState<ContextSource[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<ProviderOption>(PROVIDERS[0]);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [server, setServer] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  /** Per-account lines from the last check, keyed by source id. */
  const [lastRun, setLastRun] = useState<Record<string, string>>({});

  const scopeId = sessionId ?? NO_CONVERSATION;

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const result = await api.listContextSources(scopeId);
      // `request` returns undefined (cast to T) for an empty body.
      setSources((result?.sources ?? []).filter((s) => s.needs_credentials));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not read your accounts.");
    } finally {
      setLoading(false);
    }
  }, [scopeId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const canSubmit = useMemo(
    () =>
      !busy &&
      username.trim().length > 0 &&
      password.length > 0 &&
      (!selected.needsServer || server.trim().length > 0),
    [busy, username, password, selected, server],
  );

  async function connect(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await api.connectContextSource({
        kind: selected.kind,
        provider: selected.id,
        sessionId: scopeId,
        credentials: {
          username: username.trim(),
          password,
          ...(selected.needsServer ? { baseUrl: server.trim() } : {}),
        },
      });
      // Cleared immediately, and before anything else can await.
      setPassword("");
      setUsername("");
      setServer("");
      setNotice(
        `${selected.label} connected. The pond checks every half hour, and the first check runs shortly.`,
      );
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not connect that account.");
    } finally {
      setBusy(false);
    }
  }

  /** Ask the pond to read every connected account now. */
  async function syncNow() {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const r = await api.syncContextSources();
      // Per-account results first: a total can't say which account things came from.
      setLastRun(
        Object.fromEntries(
          (r.per_source ?? []).map((o) => [
            o.source_id,
            describeSourceOutcome(o.outcome, o.ingested),
          ]),
        ),
      );
      // Every outcome gets its own sentence; a bare "Done" says nothing.
      if (r.sources === 0) {
        setNotice("Nothing to check yet — no accounts are connected.");
      } else if (r.needs_reauth > 0) {
        setError(
          "The password was refused. Check it is an app password, not your normal one, and connect again.",
        );
      } else if (r.paused > 0) {
        setNotice("This pond is offline, so it did not reach out.");
      } else if (r.failed > 0) {
        setError("The pond could not reach that account. It will try again on its own.");
      } else if (r.ingested > 0) {
        setNotice(
          r.ingested === 1 ? "Read one new thing." : `Read ${r.ingested} new things.`,
        );
      } else {
        setNotice("Checked. Nothing new since last time.");
      }
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : "The sync could not run.");
    } finally {
      setBusy(false);
    }
  }

  async function disconnect(source: ContextSource) {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = await api.disconnectContextSource(source.id, scopeId);
      setNotice(
        result.removed === 1
          ? "Disconnected, and the one thing it had read was deleted."
          : `Disconnected, and the ${result.removed} things it had read were deleted.`,
      );
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not disconnect that account.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="connections">
      <section className="connections-intro">
        <div className="connections-introHead">
          <h3>Your accounts</h3>
          <button
            type="button"
            className="connections-sync"
            onClick={() => void syncNow()}
            disabled={busy}
          >
            <RefreshCw size={15} className={busy ? "connections-spin" : undefined} />
            <span>{busy ? "Checking" : "Check now"}</span>
          </button>
        </div>
        <p>
          The pond reads your calendar, and the subject lines of your mail, so it knows what
          your week looks like. It never sends, replies, deletes, or reads the body of a
          message — and nothing you say to your assistant is ever sent to these accounts.
        </p>
      </section>

      {loading ? (
        <p className="connections-muted">Reading your accounts…</p>
      ) : sources.length === 0 ? (
        <p className="connections-muted">No accounts connected yet.</p>
      ) : (
        <ul className="connections-list">
          {sources.map((source) => {
            const status = describeStatus(source.status, source.last_sync);
            const haul = describeHaul(source.items ?? 0, source.awaiting_index ?? 0);
            const label =
              PROVIDERS.find((p) => p.id === source.provider && p.kind === source.kind)
                ?.label ?? `${source.provider} ${source.kind}`;
            return (
              <li key={source.id} className="connections-item">
                <span className="connections-icon" aria-hidden="true">
                  {source.kind === "calendar" ? (
                    <CalendarDays size={18} />
                  ) : (
                    <Mail size={18} />
                  )}
                </span>
                <div className="connections-item-body">
                  <div className="connections-item-head">
                    <strong>{label}</strong>
                    <span className={`connections-pill connections-pill-${status.tone}`}>
                      {status.label}
                    </span>
                  </div>
                  <p className="connections-detail">{status.detail}</p>
                  <p className="connections-muted">
                    {describeLastSync(source.last_sync)}
                    {haul ? ` · ${haul}` : ""}
                  </p>
                  {lastRun[source.id] && (
                    <p className="connections-ran">
                      Last check: {lastRun[source.id]}
                    </p>
                  )}
                </div>
                <button
                  type="button"
                  className="connections-disconnect"
                  onClick={() => void disconnect(source)}
                  disabled={busy}
                  aria-label={`Disconnect ${label}`}
                >
                  <Trash2 size={16} />
                  <span>Disconnect</span>
                </button>
              </li>
            );
          })}
        </ul>
      )}

      <form className="connections-form" onSubmit={connect}>
        <h3>Connect an account</h3>

        <label className="connections-field">
          <span>Account</span>
          <select
            value={`${selected.kind}:${selected.id}:${selected.label}`}
            onChange={(e) => {
              const next = PROVIDERS.find(
                (p) => `${p.kind}:${p.id}:${p.label}` === e.target.value,
              );
              if (next) {
                setSelected(next);
                setServer("");
              }
            }}
          >
            {PROVIDERS.map((p) => (
              <option key={`${p.kind}:${p.id}:${p.label}`} value={`${p.kind}:${p.id}:${p.label}`}>
                {p.label}
              </option>
            ))}
          </select>
        </label>

        <p className="connections-hint">{selected.hint}</p>

        <label className="connections-field">
          <span>Username or email</span>
          <input
            type="text"
            autoComplete="username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            placeholder="you@example.org"
          />
        </label>

        <label className="connections-field">
          <span>App password</span>
          <input
            type="password"
            autoComplete="off"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder="Not your normal password"
          />
        </label>

        {selected.needsServer && (
          <label className="connections-field">
            <span>Server address</span>
            <input
              type="text"
              value={server}
              onChange={(e) => setServer(e.target.value)}
              placeholder={selected.serverPlaceholder}
            />
          </label>
        )}

        <button type="submit" className="connections-submit" disabled={!canSubmit}>
          {busy ? <RefreshCw size={16} className="connections-spin" /> : null}
          <span>{busy ? "Connecting…" : "Connect"}</span>
        </button>

        {error && <p className="connections-error">{error}</p>}
        {notice && <p className="connections-notice">{notice}</p>}
      </form>
    </div>
  );
}
