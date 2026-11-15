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
  // Bumped after a switch, so the panel shown re-reads the route the Pond has just started.
  const [generation, setGeneration] = useState(0);

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

  // The Pond turns the built-in network off itself once the tunnel is up: one way out at a time.
  const ngrokStarted = () => setNgrokOn(true);

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

  // Choosing the other route while remote access is on moves it there rather than turning it off.
  // Without an authtoken there is nothing to move to yet, so the ngrok panel asks for one first.
  const choose = async (value: Provider) => {
    setProvider(value);
    setSwitchFailed(false);
    const wanted = value === 'ngrok' ? 'ngrok' : 'built_in';
    try {
      const now = await api.remoteRoute();
      const on = now.route === 'ngrok' || now.builtInRunning;
      if (!on || now.route === wanted || (wanted === 'ngrok' && !now.tokenSet)) return;
      setSwitching(true);
      const next = await api.chooseRemoteRoute(wanted);
      setNgrokOn(next.route === 'ngrok');
      setNgrokConfig((config) => (config ? { ...config, enabled: next.route === 'ngrok' } : config));
      setGeneration((n) => n + 1);
    } catch (error) {
      console.warn('[remote] could not switch the way phones reach the Pond', error);
      setSwitchFailed(true);
    } finally {
      setSwitching(false);
    }
  };

  const choice = (value: Provider, label: string, hint: string) => <label style={option}>
    <input type="radio" name="remote-provider" checked={provider === value} disabled={switching} onChange={() => void choose(value)} style={{ marginTop: 3 }} />
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
    {switching ? <p role="status" style={{ margin: 0 }}>{t('tunnel.switching')}</p> : null}
    {switchFailed && !(provider === 'tailnet' && ngrokOn) ? <p role="alert" style={{ margin: 0 }}>{t('tunnel.routeSwitchFailed')}</p> : null}
    {switching ? null : provider === 'ngrok'
      ? <section style={{ display: 'grid', gap: 12 }}>
        <h3 style={{ margin: 0 }}>{t('tunnel.title')}</h3>
        <NgrokTunnel key={generation} onStarted={ngrokStarted} onStopped={() => setNgrokOn(false)} />
      </section>
      : provider === 'tailnet'
        ? ngrokOn
          ? <div style={{ display: 'grid', gap: 10, justifyItems: 'start' }}>
            <p role="status" style={{ margin: 0 }}>{t('tunnel.ngrokRunning')}</p>
            <Button variant="primary" isDisabled={switching} onPress={() => void stopNgrokForBuiltIn()}>{t('tunnel.switchToBuiltIn')}</Button>
            {switchFailed && <p role="alert" style={{ margin: 0 }}>{t('tunnel.switchFailed')}</p>}
          </div>
          : <RemoteAccess key={generation} />
        : null}
  </div>;
}
