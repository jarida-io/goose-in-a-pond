import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Button, Switch } from '@heroui/react';
import { useTranslation } from 'react-i18next';
import { api } from '../api/PondApiClient';
import { isSignInRequired } from '../api/hostCredential';
import { refusalCounter, type Refusal, type SignInRefusalKey } from '../api/signInRefusal';
import { ApiError, type RecoveryRequest } from '../api/types';

const POLL_MS = 5000;

/**
 * The coordinator's refusals over an invite or a device certificate, each with its own `remote.*`
 * message. Every one of them brings the invite field back: a refused certificate falls back to an
 * invite, a refused invite needs correcting, and a coordinator with no provisioning keys asks a
 * provisioned Pond for an invite all the same.
 */
const ADMISSION_REFUSALS = ['invite_required', 'invite_invalid', 'invite_expired', 'invite_used', 'device_certificate_invalid', 'device_revoked', 'device_used'];

/** Household activation stays on the Pond's protected local dashboard. */
export function RemoteAccess() {
  const { t } = useTranslation();
  const [control, setControl] = useState('');
  const [enrollment, setEnrollment] = useState('');
  const [invite, setInvite] = useState('');
  const [device, setDevice] = useState<{ provisioned: boolean; serial?: string; registered: boolean } | null>(null);
  const [identity, setIdentity] = useState<{ household: string; publicKey: string } | null>(null);
  const [state, setState] = useState('connecting');
  const [requests, setRequests] = useState<RecoveryRequest[]>([]);
  const [reviewFailed, setReviewFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  // Every call here needs the host credential; a restart rotates it out from under a browser tab.
  const [signInNote, setSignInNote] = useState<SignInRefusalKey | null>(null);
  const [refusals] = useState(refusalCounter);
  const noteRefusal = useCallback((error: unknown): Refusal | null => {
    if (!isSignInRequired(error)) return null;
    const refusal = refusals.refused();
    if (refusal.notice) setSignInNote(refusal.key);
    return refusal;
  }, [refusals]);
  const generation = useRef(0);
  useEffect(() => {
    const current = ++generation.current;
    void api.remoteStatus().then((status) => {
      if (current === generation.current) setState(status.state === 'Running' ? 'enabled' : status.state === 'Stopped' ? 'local' : 'failed');
    }).catch((error: unknown) => { noteRefusal(error); if (current === generation.current) setState('failed'); });
    // Unmounting supersedes whatever is in flight, the activation poll in `run` included. Written
    // as an assignment: a ref whose `current` is never assigned is taken for a React-managed node.
    return () => { generation.current += 1; };
  }, [noteRefusal]);
  // The device status changes under this page: enabling registers the household, and a coordinator
  // can refuse what it admitted before. It is read again after every enable or disable, refused or
  // not, and only the latest read is kept.
  const deviceGeneration = useRef(0);
  const refreshDevice = useCallback(() => {
    const current = ++deviceGeneration.current;
    // Unknown is shown as neither provisioned nor registered, so the invite field stays available.
    void api.remoteDevice().then((found) => { if (current === deviceGeneration.current) setDevice(found); })
      .catch(() => { if (current === deviceGeneration.current) setDevice(null); console.warn('[remote] device provisioning status unavailable'); });
  }, []);
  useEffect(() => {
    refreshDevice();
    return () => { deviceGeneration.current += 1; };
  }, [refreshDevice]);
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      let wait = POLL_MS;
      try {
        const pending = await api.remoteRecoveryRequests();
        refusals.clear();
        if (active) { setRequests(pending ?? []); setReviewFailed(false); setSignInNote(null); }
      } catch (error) {
        // A signed-out browser is refused by every later poll, and the Pond logs each, until a new
        // sign-in link reloads the page: say so once and stop. The shell keeps asking, ever less often.
        const refusal = noteRefusal(error);
        if (refusal && refusal.retryAfterMs === null) return;
        wait = refusal?.retryAfterMs ?? POLL_MS;
        if (active) setReviewFailed(true);
      }
      if (active) timer = setTimeout(() => void poll(), wait);
    };
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [refusals, noteRefusal]);
  const approve = async (id: string) => {
    setBusy(true);
    try {
      await api.approveRemoteRecovery(id);
      setRequests((pending) => pending.map((request) => request.id === id ? { ...request, approved: true } : request));
      setReviewFailed(false);
    } catch (error) { noteRefusal(error); setReviewFailed(true); console.warn('[remote] recovery approval failed'); }
    finally { setBusy(false); }
  };
  const run = async (operation: 'identity' | 'enable' | 'disable') => {
    const current = ++generation.current;
    setBusy(true);
    try {
      if (operation === 'identity') {
        const prepared = await api.prepareRemoteIdentity();
        if (current !== generation.current) return;
        setIdentity(prepared);
        setState('provision');
      } else if (operation === 'disable') {
        await api.disableRemoteAccess();
        if (current !== generation.current) return;
        setState('local');
      } else {
        // Empty means the hosted coordinator (server-filled), so only chosen origins are validated.
        for (const value of [control, enrollment].filter((entry) => entry !== '')) {
          const url = new URL(value);
          if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash || url.pathname !== '/') throw new Error('invalid origin');
        }
        await api.enableRemoteAccess({ enabled: true, controlUrl: control, enrollmentUrl: enrollment });
        setState('connecting');
        let registered = false;
        for (let attempt = 0; attempt < 30; attempt++) {
          if (current !== generation.current) return;
          const status = await api.remoteStatus();
          if (current !== generation.current) return;
          if (status.state === 'Running') { setState('enabled'); return; }
          if (status.authUrl && !registered) {
            registered = true;
            // Never retry registration automatically after an ambiguous response.
            await api.registerRemotePond(invite.trim() || undefined);
          }
          await new Promise<void>((resolve) => setTimeout(resolve, 2000));
        }
        throw new Error('activation timeout');
      }
    } catch (error) {
      noteRefusal(error);
      // The coordinator names which admission refusal it was; each has its own explanation.
      const refusal = error instanceof ApiError && ADMISSION_REFUSALS.includes(error.message) ? error.message : null;
      if (current === generation.current) setState(refusal ?? 'failed');
      console.warn('[remote] local activation operation failed');
    } finally {
      if (current === generation.current) {
        setBusy(false);
        if (operation !== 'identity') refreshDevice();
      }
    }
  };
  const on = state === 'enabled';
  const settling = state === 'connecting' || state === 'provision';
  const field: React.CSSProperties = { display: 'grid', gap: 4 };
  return <section aria-labelledby="remote-access-title" style={{ marginTop: 32, display: 'grid', gap: 16 }}>
    <div style={{ display: 'grid', gap: 6 }}>
      <h2 id="remote-access-title" style={{ margin: 0 }}>{t('remote.title')}</h2>
      <p style={{ margin: 0 }}>{t('remote.description')}</p>
      {signInNote && <p role="alert" style={{ margin: 0 }}>{t(signInNote)}</p>}
    </div>

    <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 16, padding: 'var(--space-3) var(--space-4)', border: '1px solid var(--color-border)', borderRadius: 'var(--radius-md)' }}>
      <div style={{ display: 'grid', gap: 2 }}>
        <span style={{ fontWeight: 'var(--weight-semibold)' }}>{t('remote.toggleLabel')}</span>
        <span role="status" style={{ color: 'var(--color-text-secondary)', fontSize: 'var(--text-sm)' }}>{t(`remote.${state}`)}</span>
      </div>
      <Switch
        aria-label={t('remote.toggleLabel')}
        isSelected={on}
        isDisabled={busy || settling}
        onChange={(next: boolean) => void run(next ? 'enable' : 'disable')}
      >
        <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
      </Switch>
    </div>

    {device?.provisioned && !ADMISSION_REFUSALS.includes(state)
      ? <p style={{ margin: 0, overflowWrap: 'anywhere' }}>{t('remote.provisioned', { serial: device.serial ?? '' })}</p>
      // An invite admits a household once. One already registered needs none, unless the service
      // has just refused it, which is what switching to another coordination service looks like.
      : device?.registered && !ADMISSION_REFUSALS.includes(state)
      ? <p style={{ margin: 0 }}>{t('remote.registered')}</p>
      : <label style={field}>
      <span>{t('remote.invite')}</span>
      <input type="text" autoCapitalize="characters" autoComplete="off" spellCheck={false} placeholder="giap-inv1-..." value={invite} onChange={(e) => setInvite(e.target.value)} disabled={busy || on} />
      <span style={{ color: 'var(--color-text-secondary)', fontSize: 'var(--text-sm)' }}>{t('remote.inviteHint')}</span>
    </label>}

    <details>
      <summary>{t('remote.advanced')}</summary>
      <div style={{ display: 'grid', gap: 12, marginTop: 12 }}>
        <p style={{ margin: 0 }}>{t('remote.advancedDescription')}</p>
        <label style={field}>
          <span>{t('remote.coordinator')}</span>
          <input type="url" inputMode="url" placeholder="https://control.example" value={control} onChange={(e) => setControl(e.target.value)} disabled={busy || on} />
        </label>
        <label style={field}>
          <span>{t('remote.enrollment')}</span>
          <input type="url" inputMode="url" placeholder="https://enroll.example" value={enrollment} onChange={(e) => setEnrollment(e.target.value)} disabled={busy || on} />
        </label>
        <div>
          <Button variant="secondary" isDisabled={busy} onPress={() => void run('identity')}>{t('remote.identity')}</Button>
        </div>
        {identity && <div style={{ overflowWrap: 'anywhere', display: 'grid', gap: 4 }}>
          <p style={{ margin: 0 }}>{t('remote.provision')}</p>
          <dl style={{ display: 'grid', gap: 4, margin: 0 }}>
            <dt style={{ fontWeight: 'var(--weight-semibold)' }}>{t('remote.household')}</dt><dd style={{ margin: 0 }}>{identity.household}</dd>
            <dt style={{ fontWeight: 'var(--weight-semibold)' }}>{t('remote.publicKey')}</dt><dd style={{ margin: 0 }}>{identity.publicKey}</dd>
          </dl>
        </div>}
      </div>
    </details>

    {/* Only when there is something to approve, or the requests could not be read: recovery is the
        rare path, for a phone whose identity changed while its enrollment was still active, and a
        removed or lapsed phone now comes back without it. Polling continues, so a request appears. */}
    {(requests.length > 0 || reviewFailed) && <div style={{ display: 'grid', gap: 6 }}>
      <h3 style={{ margin: 0 }}>{t('remote.recoveryTitle')}</h3>
      <p style={{ margin: 0 }}>{t('remote.recoveryDescription')}</p>
      {reviewFailed && <p role="alert" style={{ margin: 0 }}>{t('remote.reviewFailed')}</p>}
      {requests.map((request) => <div key={request.id} style={{ overflowWrap: 'anywhere', display: 'grid', gap: 4 }}>
        <p style={{ margin: 0 }}>{request.deviceName ? t('remote.reviewDevice', { name: request.deviceName }) : t('remote.reviewUnnamed')}</p>
        <p style={{ margin: 0, fontFamily: 'var(--font-mono, monospace)' }}>{t('remote.reviewKey', { key: request.keyPreview })}</p>
        <p style={{ margin: 0 }}>{t('remote.reviewCode', { id: request.id })}</p>
        {request.refused
          ? <p role="alert" style={{ margin: 0 }}>{t(`remote.${request.refused}`)}</p>
          : <div>
            <Button variant="secondary" isDisabled={busy || request.approved} onPress={() => void approve(request.id)}>{t(request.approved ? 'remote.reviewApproved' : 'remote.reviewApprove')}</Button>
          </div>}
      </div>)}
    </div>}
  </section>;
}
