import { useEffect, useState, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api/PondApiClient';
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
  const [provider, setProvider] = useState<Provider>('tailnet');
  const [ngrokOn, setNgrokOn] = useState(false);

  useEffect(() => {
    let live = true;
    void api.ngrokTunnel().then((tunnel) => {
      if (!live) return;
      setNgrokOn(tunnel.config.enabled);
      if (tunnel.config.enabled) setProvider('ngrok');
    }).catch(() => { /* The ngrok panel says so when it is shown. */ });
    return () => { live = false; };
  }, []);

  // One way out at a time: a running tunnel replaces the built-in network.
  const ngrokStarted = () => {
    setNgrokOn(true);
    void api.remoteStatus()
      .then((status) => (status.state === 'Running' ? api.disableRemoteAccess() : undefined))
      .catch((error: unknown) => console.warn('[remote] could not turn the built-in network off', error));
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
    {provider === 'ngrok'
      ? <section style={{ display: 'grid', gap: 12 }}>
        <h3 style={{ margin: 0 }}>{t('tunnel.title')}</h3>
        <NgrokTunnel onStarted={ngrokStarted} />
      </section>
      : ngrokOn
        ? <p role="status" style={{ margin: 0 }}>{t('tunnel.ngrokRunning')}</p>
        : <RemoteAccess />}
  </div>;
}
