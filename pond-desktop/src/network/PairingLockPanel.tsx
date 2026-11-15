import { useCallback, useEffect, useState } from "react";
import { Check, KeyRound, Lock, LockOpen, ShieldAlert, X } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { PairingLock, PairingRequest } from "../api/types";
import { clientLabel, isReadOnlyRefusal, lockLines, MAX_PIN_DIGITS, pinProblem } from "./pairingLockView";
import "./network.css";

const POLL_MS = 4000;

/** Whether new phones are held, who decides, and (for a member's own device) the controls. */
export function PairingLockPanel({ variant }: { variant: "hub" | "catalogue" }) {
  const [lock, setLock] = useState<PairingLock | null>(null);
  const [ownerName, setOwnerName] = useState<string | null>(null);
  const [requests, setRequests] = useState<PairingRequest[]>([]);
  // Set by a 403 on the requests list: this device may look and not change.
  const [readOnly, setReadOnly] = useState(false);
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [pin, setPin] = useState("");
  const [pinError, setPinError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setLock(await api.getPairingLock());
      setFailed(false);
    } catch {
      setFailed(true);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  useEffect(() => {
    if (!lock?.owner_profile_id) { setOwnerName(null); return; }
    let live = true;
    api.listProfiles()
      .then((r) => { if (live) setOwnerName(r.profiles.find((p) => p.id === lock.owner_profile_id)?.display_name ?? null); })
      .catch(() => { if (live) setOwnerName(null); });
    return () => { live = false; };
  }, [lock?.owner_profile_id]);

  const mayChange = Boolean(lock?.can_change) && !readOnly;

  const loadRequests = useCallback(async () => {
    try {
      setRequests((await api.listPairingRequests()).requests);
    } catch (e) {
      if (isReadOnlyRefusal(e)) setReadOnly(true);
    }
  }, []);

  useEffect(() => {
    if (!mayChange) return;
    void loadRequests();
    const id = setInterval(() => { if (document.visibilityState !== "hidden") void loadRequests(); }, POLL_MS);
    return () => clearInterval(id);
  }, [mayChange, loadRequests]);

  async function act(run: () => Promise<unknown>, after?: () => void) {
    setBusy(true);
    setProblem(null);
    try {
      await run();
      after?.();
      await load();
      await loadRequests();
    } catch (e) {
      if (isReadOnlyRefusal(e)) setReadOnly(true);
      else setProblem("The pond did not take that. Try again in a moment.");
    } finally {
      setBusy(false);
    }
  }

  if (failed && !lock) {
    return (
      <div className={`net net--${variant}`}>
        <p className="net__quiet" role="status">The pond has not said whether pairing is locked.</p>
      </div>
    );
  }
  if (!lock) return null;

  const lines = lockLines(lock, ownerName);
  const Icon = lock.locked ? Lock : LockOpen;

  function savePin() {
    const issue = pinProblem(pin, lock?.min_pin_digits ?? 6);
    setPinError(issue);
    if (issue) return;
    void act(() => api.setRecoveryPin(pin), () => setPin(""));
  }

  return (
    <div className={`net net--${variant}`}>
      <div className="net__item net__item--flat">
        <Icon className="net__icon" size={18} aria-hidden="true" />
        <div className="net__text">
          <span className="net__title">Pairing lock<span className="net__tag">{lock.locked ? "Locked" : "Open"}</span></span>
          <span className="net__line">{lines.state}</span>
          <span className="net__line">{lines.owner}</span>
          <span className="net__line">{lines.pin}</span>
        </div>
      </div>
      {lines.warning && (
        <div className="net__notice net__notice--problem" role="status">
          <ShieldAlert size={15} aria-hidden="true" />
          <span>{lines.warning}</span>
        </div>
      )}
      {!mayChange && (
        <p className="net__quiet">Only the main member's phone can change this.</p>
      )}
      {problem && <p className="net__quiet" role="alert">{problem}</p>}

      {mayChange && (
        <>
          <div className="net__row">
            <button type="button" className="net__btn" disabled={busy} aria-busy={busy} onClick={() => void act(() => api.setPairingLock(!lock.locked))}>
              {lock.locked ? "Unlock pairing" : "Lock pairing"}
            </button>
            {lock.has_recovery_pin && (
              <button type="button" className="net__btn" disabled={busy} onClick={() => void act(() => api.clearRecoveryPin())}>
                Remove recovery PIN
              </button>
            )}
          </div>
          <div className="net__row">
            <label className="net__line" htmlFor={`recovery-pin-${variant}`}>
              {lock.has_recovery_pin ? "Change the recovery PIN" : "Set a recovery PIN"}
            </label>
            <input
              id={`recovery-pin-${variant}`}
              className="net__input"
              type="password"
              inputMode="numeric"
              autoComplete="off"
              maxLength={MAX_PIN_DIGITS}
              value={pin}
              aria-describedby={pinError ? `recovery-pin-err-${variant}` : undefined}
              onChange={(e) => { setPin(e.target.value); setPinError(null); }}
            />
            <button type="button" className="net__btn" disabled={busy || pin.length === 0} onClick={savePin}>
              <KeyRound size={13} aria-hidden="true" />
              Save PIN
            </button>
          </div>
          {pinError && <p className="net__quiet" id={`recovery-pin-err-${variant}`} role="alert">{pinError}</p>}

          <div className="net__item net__item--flat">
            <div className="net__text">
              <span className="net__title">Phones asking to pair</span>
              {requests.length === 0 && <span className="net__line">None right now.</span>}
            </div>
          </div>
          {requests.length > 0 && (
            <ul className="net__list">
              {requests.map((r) => (
                <li className="net__item" key={r.id}>
                  <div className="net__text">
                    <span className="net__title">{r.device_name}<span className="net__tag">{clientLabel(r.client_type)}</span></span>
                  </div>
                  <div className="net__rowActions">
                    <button type="button" className="net__btn" disabled={busy} onClick={() => void act(() => api.approvePairingRequest(r.id))}>
                      <Check size={13} aria-hidden="true" />
                      Accept
                    </button>
                    <button type="button" className="net__btn" disabled={busy} onClick={() => void act(() => api.declinePairingRequest(r.id))}>
                      <X size={13} aria-hidden="true" />
                      Decline
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </div>
  );
}
