import { useEffect, useState, type CSSProperties } from 'react';
import { Button } from '@heroui/react';
import { useTranslation } from 'react-i18next';
import { api } from '../api/PondApiClient';
import { ApiError, type NgrokTunnelConfig } from '../api/types';
import { NgrokTunnel } from './NgrokTunnel';
import { RemoteAccess } from './RemoteAccess';

type Provider = 'tailnet' | 'ngrok';

const option: CSSProperties = {
  display: 'flex', gap: 10, alignItems: 'flex-start', padding: 'var(--space-3) var(--space-4)',
  border: '1px solid var(--color-border)', borderRadius: 'var(--radius-md)', cursor: 'pointer',
};

/** Which way phones reach the Pond from outside: the built-in network, or the household's ngrok. */
export function RemoteProvider() {
  const { t } = useTranslation();
  // Null until the Pond says which is in use, or the household picks one: a guess would show the
  // built-in network as chosen while the tunnel runs.
  const [provider, setProvider] = useState<Provider | null>(null);
  const [ngrokOn, setNgrokOn] = useState(false);
  const [ngrokConfig, setNgrokConfig] = useState<NgrokTunnelConfig | null>(null);
  const [unread, setUnread] = useState<'signedOut' | 'failed' | null>(null);
  const [switching, setSwitching] = useState(false);
  const [switchFailed, setSwitchFailed] = useState(false);

  useEffect(() => {
    let live = true;
    void api.ngrokTunnel().then((tunnel) => {
      if (!live) return;
      setUnread(null);
      setNgrokOn(tunnel.config.enabled);
      setNgrokConfig(tunnel.config);
      setProvider((chosen) => chosen ?? (tunnel.config.enabled ? 'ngrok' : 'tailnet'));
    }).catch((error: unknown) => {
      if (!live) return;
      const signedOut = error instanceof ApiError && (error.status === 401 || error.status === 403);
      console.warn('[remote] could not read which way phones reach the Pond', error);
      setUnread(signedOut ? 'signedOut' : 'failed');
    });
    return () => { live = false; };
  }, []);

  // One way out at a time: a running tunnel replaces the built-in network.
  const ngrokStarted = () => {
    setNgrokOn(true);
    void api.remoteStatus()
      .then((status) => (status.state === 'Running' ? api.disableRemoteAccess() : undefined))
      .catch((error: unknown) => console.warn('[remote] could not turn the built-in network off', error));
  };

  // Choosing the built-in network while the tunnel runs: stop the tunnel here, rather than send
  // the household back to the ngrok panel the choice just hid.
  const stopNgrokForBuiltIn = async () => {
    setSwitching(true);
    setSwitchFailed(false);
    try {
      const next = await api.configureNgrokTunnel({ ...(ngrokConfig ?? { endpoint: 'tcp' }), enabled: false });
      setNgrokConfig(next.config);
      setNgrokOn(next.config.enabled);
    } catch (error) {
      console.warn('[remote] could not stop the ngrok tunnel', error);
      setSwitchFailed(true);
    } finally {
      setSwitching(false);
    }
  };

  const choice = (value: Provider, label: string, hint: string) => <label style={option}>
    <input type="radio" name="remote-provider" checked={provider === value} onChange={() => setProvider(value)} style={{ marginTop: 3 }} />
    <span style={{ display: 'grid', gap: 2 }}>
      <span style={{ fontWeight: 'var(--weight-semibold)' }}>{label}</span>
      <span style={{ color: 'var(--color-text-secondary)', fontSize: 'var(--text-sm)' }}>{hint}</span>
    </span>
  </label>;

  return <div style={{ display: 'grid', gap: 12 }}>
    <fieldset style={{ border: 0, padding: 0, margin: 0, display: 'grid', gap: 8 }}>
      <legend style={{ padding: 0, marginBottom: 4, fontWeight: 'var(--weight-semibold)' }}>{t('tunnel.providerTitle')}</legend>
      {choice('tailnet', t('tunnel.providerTailnet'), t('tunnel.providerTailnetHint'))}
      {choice('ngrok', t('tunnel.providerNgrok'), t('tunnel.providerNgrokHint'))}
    </fieldset>
    {unread && provider === null
      ? <p role="status" style={{ margin: 0 }}>{unread === 'signedOut' ? t('tunnel.providerSignedOut') : t('tunnel.providerUnread')}</p>
      : null}
    {provider === 'ngrok'
      ? <section style={{ display: 'grid', gap: 12 }}>
        <h3 style={{ margin: 0 }}>{t('tunnel.title')}</h3>
        <NgrokTunnel onStarted={ngrokStarted} onStopped={() => setNgrokOn(false)} />
      </section>
      : provider === 'tailnet'
        ? ngrokOn
          ? <div style={{ display: 'grid', gap: 10, justifyItems: 'start' }}>
            <p role="status" style={{ margin: 0 }}>{t('tunnel.ngrokRunning')}</p>
            <Button variant="primary" isDisabled={switching} onPress={() => void stopNgrokForBuiltIn()}>{t('tunnel.switchToBuiltIn')}</Button>
            {switchFailed && <p role="alert" style={{ margin: 0 }}>{t('tunnel.switchFailed')}</p>}
          </div>
          : <RemoteAccess />
        : null}
  </div>;
}
