import i18n from '../i18n';
import { RemoteAccess } from './RemoteAccess';
import { useState, useEffect, useRef, useCallback } from "react";
import { Button, Chip } from "@heroui/react";
import { RefreshCw, Smartphone, Wifi } from "lucide-react";
import QRCode from "qrcode";
import { api } from "../api/PondApiClient";
import type { PairingCodeBinding } from "../api/types";
import { isSignInRequired } from "../api/hostCredential";
import { signInRefusalKey } from "../api/signInRefusal";
import { PageHeader } from "../components/shared";

interface PairingInfo {
  code: string;
  expiresAt: string;
  pairUrl: string;
  /** The member a device pairing with this code becomes, as the Pond reports it; null is nobody. */
  profileId: string | null;
}

interface Member {
  id: string;
  display_name: string;
}

/** The picker's value for a code bound to nobody; member ids are never empty. */
const NOBODY = "";

/** What a choice in the picker asks the Pond for. Nobody is asked for explicitly, so a household
 *  of one can still pair a guest's phone. */
function bindingFor(choice: string): PairingCodeBinding {
  return choice === NOBODY ? { unattributed: true } : { profile_id: choice };
}

function timeUntil(isoString: string): string {
  const diff = new Date(isoString).getTime() - Date.now();
  if (diff <= 0) return "expired";
  const mins = Math.floor(diff / 60000);
  const secs = Math.floor((diff % 60000) / 1000);
  return mins > 0 ? `${mins}m ${secs}s` : `${secs}s`;
}

export function Pairing() {
  const [info, setInfo] = useState<PairingInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [timeLeft, setTimeLeft] = useState("");
  const canvasRef = useRef<HTMLCanvasElement>(null);
  /** Null until read; the read failing is said on screen, not shown as an empty household. */
  const [members, setMembers] = useState<Member[] | null>(null);
  const [membersFailed, setMembersFailed] = useState(false);
  /** The operator's choice, once made; every later code (New code, expiry) is issued for it. */
  const binding = useRef<PairingCodeBinding | undefined>(undefined);

  const loadPairingInfo = useCallback(async (forceNew = false) => {
    setLoading(true);
    setError(null);
    try {
      const pc = await (forceNew
        ? api.issuePairingCode(binding.current)
        : api.getPairingCode().then((r) => (r.code ? r : api.issuePairingCode(binding.current))));
      if (!pc.code) throw new Error("Server returned no pairing code");
      // Pairing material comes with the code, from the host-only route, not /system/info.
      const sysInfo = pc.pairing;
      // Every address, tried in this order: mDNS (survives DHCP; Android's resolver has no mDNS),
      // the raw LAN address, then the tailnet address for when the phone is away from home.
      if (!sysInfo.https_port || !sysInfo.tls_spki_sha256) throw new Error(i18n.t('pairing.unavailable'));
      const params = new URLSearchParams({
        v: "2",
        scheme: "https",
        pin: sysInfo.tls_spki_sha256,
        host: `${sysInfo.hostname}.local`,
        port: String(sysInfo.https_port),
        code: pc.code,
      });
      if (sysInfo.lan_address) params.set("ip", sysInfo.lan_address);
      if (sysInfo.tailnet_address) params.set("ts", sysInfo.tailnet_address);
      const pairUrl = `pond://pair?${params.toString()}`;
      setInfo({ code: pc.code, expiresAt: pc.expires_at ?? "", pairUrl, profileId: pc.profile_id ?? null });
    } catch (e) {
      setError(isSignInRequired(e) ? i18n.t(signInRefusalKey()) : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!info?.pairUrl || !canvasRef.current) return;
    QRCode.toCanvas(canvasRef.current, info.pairUrl, {
      width: 220,
      margin: 2,
      color: { dark: "#000000", light: "#ffffff" },
    }).catch((e) => console.error("QR render failed", e));
  }, [info?.pairUrl]);

  useEffect(() => {
    if (!info?.expiresAt) return;
    const id = setInterval(() => setTimeLeft(timeUntil(info.expiresAt)), 500);
    setTimeLeft(timeUntil(info.expiresAt));
    return () => clearInterval(id);
  }, [info?.expiresAt]);

  useEffect(() => {
    if (timeLeft === "expired") loadPairingInfo(true);
  }, [timeLeft, loadPairingInfo]);

  useEffect(() => {
    loadPairingInfo(false);
  }, [loadPairingInfo]);

  useEffect(() => {
    let live = true;
    api
      .listProfiles()
      .then((r) => live && setMembers(r.profiles))
      .catch((e) => {
        console.warn("[pairing] could not read the household members", e);
        if (live) setMembersFailed(true);
      });
    return () => {
      live = false;
    };
  }, []);

  const chooseOwner = (choice: string) => {
    binding.current = bindingFor(choice);
    void loadPairingInfo(true);
  };
  const ownerName = info?.profileId
    ? (members?.find((m) => m.id === info.profileId)?.display_name ?? info.profileId)
    : null;

  return (
    <div className="screen screen--pairing">
      <PageHeader
        title="Pair a device"
        action={
          <Button
            size="sm"
            variant="secondary"
            className="page-header-btn"
            isDisabled={loading}
            onPress={() => loadPairingInfo(true)}
          >
            <RefreshCw size={13} style={{ display: "inline" }} /> New code
          </Button>
        }
      />

      {error && <p className="muted-12 text-error" style={{ marginBottom: 16 }}>{error}</p>}

      <div style={{ display: "flex", gap: 32, flexWrap: "wrap", alignItems: "flex-start" }}>
        {/* QR code */}
        <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 12 }}>
          <div
            style={{
              border: "1px solid var(--color-border)",
              borderRadius: 12,
              padding: 12,
              background: "#fff",
              lineHeight: 0,
              opacity: loading ? 0.4 : 1,
              transition: "opacity 0.2s",
            }}
          >
            <canvas ref={canvasRef} width={220} height={220} />
          </div>
          {info && (
            <Chip
              size="sm"
              color={timeLeft === "expired" ? "danger" : "success"}
              variant="soft"
              className="header-status-chip"
            >
              {timeLeft === "expired" ? "Expired — refreshing…" : `Expires in ${timeLeft}`}
            </Chip>
          )}
        </div>

        {/* Text instructions */}
        <div style={{ flex: 1, minWidth: 220 }}>
          <div style={{ marginBottom: 20 }}>
            <p className="muted-12" style={{ marginBottom: 8 }}>Pairing code</p>
            <div
              style={{
                fontFamily: "var(--font-mono, monospace)",
                fontSize: 36,
                fontWeight: 700,
                letterSpacing: "0.18em",
                opacity: loading ? 0.4 : 1,
              }}
            >
              {info?.code ?? "——————"}
            </div>
            <label className="muted-12" htmlFor="pairing-owner" style={{ display: "block", marginTop: 16, marginBottom: 8 }}>
              {i18n.t("pairing.owner")}
            </label>
            <select
              id="pairing-owner"
              className="native-select"
              disabled={loading || !info || members === null}
              value={info?.profileId ?? NOBODY}
              onChange={(e) => chooseOwner(e.target.value)}
            >
              <option value={NOBODY}>{i18n.t("pairing.ownerNobody")}</option>
              {/* A code bound to a member this list does not have still shows whose it is. */}
              {info?.profileId && members && !members.some((m) => m.id === info.profileId) && (
                <option value={info.profileId}>{info.profileId}</option>
              )}
              {(members ?? []).map((m) => (
                <option key={m.id} value={m.id}>
                  {m.display_name}
                </option>
              ))}
            </select>
            {info && (
              <p className="muted-12" role="status" style={{ marginTop: 8 }}>
                {ownerName
                  ? i18n.t("pairing.boundTo", { name: ownerName })
                  : i18n.t("pairing.boundToNobody")}
              </p>
            )}
            {membersFailed && (
              <p className="muted-12 text-error" style={{ marginTop: 8 }}>
                {i18n.t("pairing.membersFailed")}
              </p>
            )}
          </div>

          {info && <div style={{ overflowWrap: "anywhere", marginBottom: 20 }}>
            <p>{i18n.t('pairing.address')}</p>
            <code>{`https://${new URL(info.pairUrl).searchParams.get("ip") || new URL(info.pairUrl).searchParams.get("host")}:${new URL(info.pairUrl).searchParams.get("port")}`}</code>
            <p>{i18n.t('pairing.fingerprint')}</p>
            <code>{new URL(info.pairUrl).searchParams.get("pin")}</code>
            <p>{i18n.t('pairing.manual')}</p>
          </div>}
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <Step n={1} icon={<Wifi size={16} />}>
              Make sure your phone is on the <strong>same Wi-Fi network</strong> as this hub.
            </Step>
            <Step n={2} icon={<Smartphone size={16} />}>
              Open <strong>Goose On The Go</strong> and tap <em>Pair new hub</em>.
            </Step>
            <Step n={3} icon={<RefreshCw size={16} />}>
              Scan the QR code or enter the 6-digit code. The hub is advertised as{" "}
              <code style={{ fontSize: 11 }}>_pond._tcp.local.</code> via mDNS.
            </Step>
          </div>
          <RemoteAccess />
        </div>
      </div>
    </div>
  );
}

function Step({
  n,
  icon,
  children,
}: {
  n: number;
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div style={{ display: "flex", gap: 10, alignItems: "flex-start" }}>
      <div
        style={{
          minWidth: 22,
          height: 22,
          borderRadius: "50%",
          background: "var(--color-primary, #6366f1)",
          color: "#fff",
          fontSize: 11,
          fontWeight: 700,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        {n}
      </div>
      <span style={{ display: "flex", gap: 6, alignItems: "center", fontSize: 13, color: "var(--color-text-secondary)" }}>
        {icon}
        <span>{children}</span>
      </span>
    </div>
  );
}
