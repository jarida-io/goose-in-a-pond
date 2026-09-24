import { useState, useEffect, useRef, useCallback } from "react";
import { Button, Chip } from "@heroui/react";
import { RefreshCw, Smartphone, Wifi } from "lucide-react";
import QRCode from "qrcode";
import { api } from "../api/PondApiClient";
import { PageHeader } from "../components/shared";

interface PairingInfo {
  code: string;
  expiresAt: string;
  pairUrl: string;
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

  const loadPairingInfo = useCallback(async (forceNew = false) => {
    setLoading(true);
    setError(null);
    try {
      const [sysInfo, pc] = await Promise.all([
        api.getSystemInfo(),
        forceNew
          ? api.issuePairingCode()
          : api.getPairingCode().then((r) =>
              r.code ? r : api.issuePairingCode()
            ),
      ]);
      if (!pc.code) throw new Error("Server returned no pairing code");
      // Every address this Pond has, because none of them works everywhere.
      // The mDNS name survives a DHCP lease change and is what an iPhone
      // resolves happily; Android's resolver does no mDNS at all, so
      // `<host>.local` fails there and the phone needs the raw address; and
      // neither reaches the Pond once the phone leaves the house, which is what
      // the tailnet address is for. The client tries them in that order.
      const params = new URLSearchParams({
        host: `${sysInfo.hostname}.local`,
        port: String(sysInfo.port),
        code: pc.code,
      });
      if (sysInfo.lan_address) params.set("ip", sysInfo.lan_address);
      if (sysInfo.tailnet_address) params.set("ts", sysInfo.tailnet_address);
      const pairUrl = `pond://pair?${params.toString()}`;
      setInfo({ code: pc.code, expiresAt: pc.expires_at ?? "", pairUrl });
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // Render QR code onto canvas whenever pairUrl changes.
  useEffect(() => {
    if (!info?.pairUrl || !canvasRef.current) return;
    QRCode.toCanvas(canvasRef.current, info.pairUrl, {
      width: 220,
      margin: 2,
      color: { dark: "#000000", light: "#ffffff" },
    }).catch((e) => console.error("QR render failed", e));
  }, [info?.pairUrl]);

  // Countdown timer.
  useEffect(() => {
    if (!info?.expiresAt) return;
    const id = setInterval(() => setTimeLeft(timeUntil(info.expiresAt)), 500);
    setTimeLeft(timeUntil(info.expiresAt));
    return () => clearInterval(id);
  }, [info?.expiresAt]);

  // Auto-refresh when expired.
  useEffect(() => {
    if (timeLeft === "expired") loadPairingInfo(true);
  }, [timeLeft, loadPairingInfo]);

  useEffect(() => {
    loadPairingInfo(false);
  }, [loadPairingInfo]);

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
          </div>

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
