import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Button } from '@heroui/react';
import { useTranslation } from 'react-i18next';
import { api } from '../api/PondApiClient';
import { ApiError, type NgrokTunnelConfig, type NgrokTunnelState } from '../api/types';

const POLL_MS = 3000;

const field: React.CSSProperties = { display: 'grid', gap: 4 };
const hint: React.CSSProperties = { color: 'var(--color-text-secondary)', fontSize: 'var(--text-sm)' };

function refusalMessage(error: unknown): string {
  if (error instanceof ApiError) {
    const body = error.body as { error?: unknown } | undefined;
    if (body && typeof body.error === 'string') return body.error;
  }
  return error instanceof Error ? error.message : String(error);
}

/** The household's own ngrok tunnel: its authtoken, the kind of address, and on or off. */
export function NgrokTunnel({ onStarted, onStopped }: { onStarted?: () => void; onStopped?: () => void }) {
  const { t } = useTranslation();
  const [tunnel, setTunnel] = useState<NgrokTunnelState | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [token, setToken] = useState('');
  const [draft, setDraft] = useState<NgrokTunnelConfig>({ enabled: false, endpoint: 'tcp' });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Only the first read, and a change made here, seed the form; polls leave typing alone.
  const seeded = useRef(false);
  const adopt = useCallback((next: NgrokTunnelState, seed: boolean) => {
    setTunnel(next);
    if (seed || !seeded.current) {
      seeded.current = true;
      setDraft(next.config);
    }
  }, []);

  useEffect(() => {
    let live = true;
    const read = () => api.ngrokTunnel()
      .then((next) => { if (live) { setUnavailable(false); adopt(next, false); } })
      .catch(() => { if (live) setUnavailable(true); });
    void read();
    const timer = window.setInterval(() => void read(), POLL_MS);
    return () => { live = false; window.clearInterval(timer); };
  }, [adopt]);

  const act = async (work: () => Promise<NgrokTunnelState>) => {
    setBusy(true);
    setError(null);
    try {
      const next = await work();
      adopt(next, true);
      return next;
    } catch (e) {
      setError(refusalMessage(e));
      return null;
    } finally {
      setBusy(false);
    }
  };

  const saveToken = async () => {
    const next = await act(() => api.setNgrokAuthtoken(token.trim()));
    if (next) setToken('');
  };

  const setEnabled = async (enabled: boolean) => {
    const next = await act(() => api.configureNgrokTunnel({ ...draft, enabled }));
    if (next && enabled && next.status.state !== 'failed') onStarted?.();
    if (next && !enabled) onStopped?.();
  };

  const status = tunnel?.status;
  const on = tunnel?.config.enabled ?? false;
  const statusText = !status ? '' :
    status.state === 'running' ? t('tunnel.running', { address: status.address }) :
    status.state === 'failed' ? t('tunnel.failed', { reason: status.reason }) :
    t(`tunnel.${status.state}`);

  if (unavailable && !tunnel) return <p role="alert" style={{ margin: 0 }}>{t('tunnel.unavailable')}</p>;

  return <div style={{ display: 'grid', gap: 12 }}>
    <p style={{ margin: 0 }}>{t('tunnel.description')}</p>

    <div style={field}>
      <label htmlFor="ngrok-authtoken">{t('tunnel.token')}</label>
      <input
        id="ngrok-authtoken"
        aria-describedby="ngrok-authtoken-hint"
        type="password"
        autoComplete="off"
        spellCheck={false}
        value={token}
        onChange={(e) => setToken(e.target.value)}
        disabled={busy}
      />
      <span id="ngrok-authtoken-hint" style={hint}>{tunnel?.tokenSet ? t('tunnel.tokenSet') : t('tunnel.tokenHint')}</span>
    </div>
    <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
      <Button variant="secondary" isDisabled={busy || token.trim() === ''} onPress={() => void saveToken()}>{t('tunnel.saveToken')}</Button>
      {tunnel?.tokenSet && <Button variant="danger-soft" isDisabled={busy} onPress={() => void act(() => api.forgetNgrokAuthtoken())}>{t('tunnel.forgetToken')}</Button>}
    </div>

    <fieldset style={{ border: 0, padding: 0, margin: 0, display: 'grid', gap: 6 }} disabled={busy || on}>
      <legend style={{ padding: 0, marginBottom: 4 }}>{t('tunnel.endpoint')}</legend>
      <label style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
        <input type="radio" name="ngrok-endpoint" checked={draft.endpoint === 'tcp'} onChange={() => setDraft({ ...draft, endpoint: 'tcp' })} />
        {t('tunnel.endpointTcp')}
      </label>
      <label style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
        <input type="radio" name="ngrok-endpoint" checked={draft.endpoint === 'tls'} onChange={() => setDraft({ ...draft, endpoint: 'tls' })} />
        {t('tunnel.endpointTls')}
      </label>
      {draft.endpoint === 'tls'
        ? <label style={field}>
          <span>{t('tunnel.domain')}</span>
          <input type="text" inputMode="url" autoComplete="off" spellCheck={false} placeholder="pond.example.org"
            value={draft.domain ?? ''} onChange={(e) => setDraft({ ...draft, domain: e.target.value.trim() || undefined })} />
        </label>
        : <div style={field}>
          <label htmlFor="ngrok-remote-addr">{t('tunnel.remoteAddr')}</label>
          <input id="ngrok-remote-addr" aria-describedby="ngrok-remote-addr-hint" type="text" autoComplete="off" spellCheck={false} placeholder="1.tcp.ngrok.io:20123"
            value={draft.remoteAddr ?? ''} onChange={(e) => setDraft({ ...draft, remoteAddr: e.target.value.trim() || undefined })} />
          <span id="ngrok-remote-addr-hint" style={hint}>{t('tunnel.remoteAddrHint')}</span>
        </div>}
    </fieldset>

    <div style={{ display: 'flex', gap: 12, alignItems: 'center', flexWrap: 'wrap' }}>
      {on
        ? <Button variant="secondary" isDisabled={busy} onPress={() => void setEnabled(false)}>{t('tunnel.stop')}</Button>
        : <Button variant="primary" isDisabled={busy || !tunnel?.tokenSet} onPress={() => void setEnabled(true)}>{t('tunnel.start')}</Button>}
      <span role="status" style={hint}>{statusText}</span>
    </div>
    {!on && <p style={{ ...hint, margin: 0 }}>{t('tunnel.replacesTailnet')}</p>}
    {error && <p role="alert" style={{ margin: 0 }}>{error}</p>}
  </div>;
}
