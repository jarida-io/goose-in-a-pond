import { useCallback, useEffect, useState } from "react";
import { Check, Lock, LockOpen, ShieldAlert, X } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { PairingLock, PairingRequest } from "../api/types";
import { clientLabel, isReadOnlyRefusal, lockLines } from "./pairingLockView";
import "./network.css";

const POLL_MS = 4000;

/**
 * The pairing lock as far as pairing a phone needs it: whether a new phone will wait, why the lock
 * is not holding when it is not, and, on the main member's own device, the phones waiting right
 * now with Accept and Decline. Each part shows only when it applies. Changing the lock and the
 * recovery PIN lives in Settings > Connections (`PairingLockPanel`).
 */
export function PairingLockBrief() {
  const [lock, setLock] = useState<PairingLock | null>(null);
  const [requests, setRequests] = useState<PairingRequest[]>([]);
  // A 403 on the requests list: this device may look and not decide.
  const [readOnly, setReadOnly] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setLock(await api.getPairingLock());
    } catch {
      // The page still pairs without it; Settings > Connections says what is wrong.
      setLock(null);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const mayDecide = Boolean(lock?.can_change) && !readOnly;

  const loadRequests = useCallback(async () => {
    try {
      setRequests((await api.listPairingRequests()).requests);
    } catch (e) {
      if (isReadOnlyRefusal(e)) setReadOnly(true);
    }
  }, []);

  useEffect(() => {
    if (!mayDecide) return;
    void loadRequests();
    const id = setInterval(() => { if (document.visibilityState !== "hidden") void loadRequests(); }, POLL_MS);
    return () => clearInterval(id);
  }, [mayDecide, loadRequests]);

  async function decide(run: () => Promise<unknown>) {
    setBusy(true);
    setProblem(null);
    try {
      await run();
      await load();
      await loadRequests();
    } catch (e) {
      if (isReadOnlyRefusal(e)) setReadOnly(true);
      else setProblem("The pond did not take that. Try again in a moment.");
    } finally {
      setBusy(false);
    }
  }

  if (!lock) return null;
  const lines = lockLines(lock, null);
  const Icon = lock.locked ? Lock : LockOpen;

  return (
    <div className="net net--catalogue">
      <div className="net__item net__item--flat">
        <Icon className="net__icon" size={18} aria-hidden="true" />
        <div className="net__text">
          <span className="net__line">{lines.state}</span>
        </div>
      </div>
      {lines.warning && (
        <div className="net__notice net__notice--problem" role="status">
          <ShieldAlert size={15} aria-hidden="true" />
          <span>{lines.warning}</span>
        </div>
      )}
      {problem && <p className="net__quiet" role="alert">{problem}</p>}
      {mayDecide && requests.length > 0 && (
        <>
          <span className="net__title">Phones asking to pair</span>
          <ul className="net__list">
            {requests.map((r) => (
              <li className="net__item" key={r.id}>
                <div className="net__text">
                  <span className="net__title">{r.device_name}<span className="net__tag">{clientLabel(r.client_type)}</span></span>
                </div>
                <div className="net__rowActions">
                  <button type="button" className="net__btn" disabled={busy} onClick={() => void decide(() => api.approvePairingRequest(r.id))}>
                    <Check size={13} aria-hidden="true" />
                    Accept
                  </button>
                  <button type="button" className="net__btn" disabled={busy} onClick={() => void decide(() => api.declinePairingRequest(r.id))}>
                    <X size={13} aria-hidden="true" />
                    Decline
                  </button>
                </div>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}
