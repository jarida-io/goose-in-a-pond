import {
  AlertTriangle,
  Bluetooth,
  BluetoothConnected,
  BluetoothOff,
  CheckCircle2,
  EthernetPort,
  Info,
  KeyRound,
  Loader2,
  RefreshCw,
  Unplug,
} from "lucide-react";
import { useConnections } from "./useConnections";
import { PairingLockPanel } from "./PairingLockPanel";
import { signalBars, signalLabel, describeBluetooth, describeEthernet, describeSetup } from "./connectionsView";
import "./network.css";

export type Variant = "hub" | "catalogue";

export function SignalBars({ signal }: { signal: number | null }) {
  const lit = signalBars(signal);
  return (
    <span className="net__signal">
      <span className="net__bars" role="img" aria-label={signalLabel(signal)}>
        {[1, 2, 3, 4].map((n) => (
          <span key={n} className="net__bar" data-lit={n <= lit || undefined} />
        ))}
      </span>
      {signal !== null && (
        <span className="net__pct" aria-hidden="true">
          {Math.round(signal)}%
        </span>
      )}
    </span>
  );
}

export function Message({ tone, text, detail }: {
  tone: "ok" | "info" | "problem";
  text: string;
  detail?: string;
}) {
  const Icon = tone === "problem" ? AlertTriangle : tone === "ok" ? CheckCircle2 : Info;
  return (
    <div
      className={`net__notice${tone === "problem" ? " net__notice--problem" : ""}`}
      role={tone === "problem" ? "alert" : "status"}
    >
      <Icon size={15} aria-hidden="true" />
      <span>
        {text}
        {detail && <span className="net__detail">{detail}</span>}
      </span>
    </div>
  );
}

export function LinkHealthNotice({ always = false }: { always?: boolean }) {
  const { connections, health, recovered, refresh } = useConnections();
  if (recovered && health === "ok") {
    return <Message tone="ok" text="Back in touch with the pond." />;
  }
  if (health === "ok") return null;
  if (health === "loading") {
    return connections ? null : (
      <p className="net__quiet" role="status">
        <Loader2 className="net__spin" size={14} aria-hidden="true" />
        Finding out what is connected.
      </p>
    );
  }
  if (connections && !always) return null;
  return (
    <div className="net__notice net__notice--problem" role="status">
      <AlertTriangle size={15} aria-hidden="true" />
      <span>
        {health === "unexpected"
          ? "The pond answered in a form this app does not understand. The pond or this app may need an update."
          : "The pond is not answering. If it is switching networks this can take a moment, and the app keeps trying."}
      </span>
      <button className="net__btn net__btn--quiet" type="button" onClick={() => void refresh()}>
        <RefreshCw size={13} aria-hidden="true" />
        Try now
      </button>
    </div>
  );
}

function Waiting() {
  const { connections } = useConnections();
  return connections ? null : <LinkHealthNotice />;
}

export function EthernetPanel({ variant }: { variant: Variant }) {
  const { connections } = useConnections();
  return (
    <div className={`net net--${variant}`}>
      <Waiting />
      {connections && connections.ethernet.length === 0 && (
        <p className="net__empty">
          <Unplug size={16} aria-hidden="true" />
          <span>
            <b>No wired connection</b>
            Plug in a cable and it shows up here.
          </span>
        </p>
      )}
      {connections && connections.ethernet.length > 0 && (
        <ul className="net__list">
          {connections.ethernet.map((link) => {
            const line = describeEthernet(link);
            return (
              <li className="net__item" key={link.interface}>
                <EthernetPort className="net__icon" size={18} aria-hidden="true" />
                <div className="net__text">
                  <span className="net__title">
                    <span className="net__mono">{link.interface}</span>
                    <span className="net__tag">{line.state}</span>
                  </span>
                  <span className="net__line">
                    {line.address && <span className="net__mono">{line.address}</span>}
                    {line.speed && <span className="net__mono">{line.speed}</span>}
                  </span>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

export function BluetoothStatus({ variant }: { variant: Variant }) {
  const { connections } = useConnections();
  const bt = connections?.bluetooth;
  const line = bt ? describeBluetooth(bt) : null;
  const Icon = !bt ? Bluetooth : bt.state === "unavailable" ? AlertTriangle : bt.state === "off" ? BluetoothOff : bt.session_open ? BluetoothConnected : Bluetooth;
  return (
    <div className={`net net--${variant}`}>
      <Waiting />
      {bt && line && (
        <div className="net__item net__item--flat">
          <Icon className="net__icon" size={18} aria-hidden="true" />
          <div className="net__text">
            <span className="net__title">
              {line.headline}
              {bt.state === "advertising" && bt.advertising_as && (
                <span className="net__mono net__name">{bt.advertising_as}</span>
              )}
            </span>
            <span className="net__line" data-problem={line.problem || undefined}>
              {line.detail}
            </span>
            {bt.session_open && <span className="net__line net__line--strong">A phone is connected now.</span>}
          </div>
        </div>
      )}
    </div>
  );
}

export function PairingStatus({ variant }: { variant: Variant }) {
  const { setup } = useConnections();
  const line = setup ? describeSetup(setup) : null;
  return (
    <div className={`net net--${variant}`}>
      <div className="net__item net__item--flat">
        <KeyRound className="net__icon" size={18} aria-hidden="true" />
        <div className="net__text">
          <span className="net__title">
            Setup card
            {line && <span className="net__tag">{line.headline}</span>}
          </span>
          <span className="net__line">{line ? line.detail : "The pond has not said whether its setup card was claimed."}</span>
          {setup?.claimed && setup.claimed_by_this_device && (
            <span className="net__line">This device claimed it.</span>
          )}
          <span className="net__line">The code stays on the card and is never shown here.</span>
        </div>
      </div>
      <PairingLockPanel variant={variant} />
    </div>
  );
}

