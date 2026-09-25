import { useState, useEffect, useRef, useCallback } from "react";
import { Button, Separator, Switch } from "@heroui/react";
import { Share2, Plus, X, Trash2, Copy, Wifi, WifiOff, Coins, Cpu, Zap } from "lucide-react";
import QRCode from "qrcode";
import { api } from "../api/PondApiClient";
import type {
  MeshPeer,
  MeshPeerCapabilities,
  MeshSelf,
  MeshSettlementStatus,
} from "../api/types";
import { PageHeader } from "../components/shared";

function truncatePeerId(peerId: string): string {
  return peerId.length > 16 ? `${peerId.slice(0, 8)}…${peerId.slice(-8)}` : peerId;
}

function formatMillisats(msats: number): string {
  return `${(msats / 1000).toLocaleString()} sats`;
}

/** Accepts a bare peer-id hex string or a `pond-mesh://invite?...` URL from another Pond. */
function parseInvite(input: string): { peerId: string; address?: string } {
  const trimmed = input.trim();
  if (!trimmed.startsWith("pond-mesh://")) {
    return { peerId: trimmed };
  }
  try {
    const url = new URL(trimmed);
    const peerId = url.searchParams.get("peer") ?? "";
    const address = url.searchParams.get("addr") ?? undefined;
    return { peerId, address };
  } catch {
    return { peerId: trimmed };
  }
}

export function Mesh() {
  const [peers, setPeers] = useState<MeshPeer[]>([]);
  const [self, setSelf] = useState<MeshSelf | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [showForm, setShowForm] = useState(false);
  const [inviteInput, setInviteInput] = useState("");
  const [trustScope, setTrustScope] = useState<"self_owned" | "circle">("circle");
  const [submitting, setSubmitting] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const [topUpTarget, setTopUpTarget] = useState<MeshPeer | null>(null);
  const [topUpAmount, setTopUpAmount] = useState("");
  const [topUpSubmitting, setTopUpSubmitting] = useState(false);
  const [topUpError, setTopUpError] = useState<string | null>(null);

  // Queried live per connected peer; not in the peer list (see MeshPeerCapabilities).
  const [capabilities, setCapabilities] = useState<Record<string, MeshPeerCapabilities>>({});

  // The persisted setting; `self.mesh_enabled` is what is live. `PUT /settings` builds the
  // transport synchronously, so once `toggleMesh` re-fetches `self` the two should agree.
  const [meshEnabledSetting, setMeshEnabledSetting] = useState<boolean | null>(null);
  const [meshToggling, setMeshToggling] = useState(false);

  // Settlement-job status: informational and best-effort; a failure must not block the screen.
  const [settlementStatus, setSettlementStatus] = useState<MeshSettlementStatus | null>(null);

  const canvasRef = useRef<HTMLCanvasElement>(null);

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    Promise.all([api.listMeshPeers(), api.getMeshSelf(), api.getSettings()])
      .then(([p, s, settings]) => {
        setMeshEnabledSetting(settings.mesh_enabled ?? false);
        setPeers(p);
        setSelf(s);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
    api.getMeshSettlementStatus().then(setSettlementStatus).catch(() => {});
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // `connected` is a live swarm snapshot, not a DB flag, so poll; quietly (no loading/error
  // toggles) so the screen doesn't flicker to a spinner every tick.
  useEffect(() => {
    const interval = setInterval(() => {
      Promise.all([api.listMeshPeers(), api.getMeshSelf()])
        .then(([p, s]) => {
          setPeers(p);
          setSelf(s);
        })
        .catch(() => {});
    }, 10_000);
    return () => clearInterval(interval);
  }, []);

  useEffect(() => {
    if (!self?.invite_url || !canvasRef.current) return;
    QRCode.toCanvas(canvasRef.current, self.invite_url, {
      width: 180,
      margin: 2,
      color: { dark: "#000000", light: "#ffffff" },
    }).catch((e) => console.error("QR render failed", e));
  }, [self?.invite_url]);

  // Connected peers only (offline ones time out); a 404 (untrusted) or 503 (mesh off) shows no chips.
  useEffect(() => {
    for (const p of peers) {
      if (!p.connected || capabilities[p.peer_id]) continue;
      api
        .getMeshPeerCapabilities(p.peer_id)
        .then((c) => setCapabilities((prev) => ({ ...prev, [p.peer_id]: c })))
        .catch(() => {});
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [peers]);

  async function toggleMesh(next: boolean) {
    setMeshToggling(true);
    try {
      const settings = await api.updateSettings({ mesh_enabled: next });
      setMeshEnabledSetting(settings.mesh_enabled ?? next);
      // The PUT blocks until the transport is built, so `self` is current now.
      const fresh = await api.getMeshSelf();
      setSelf(fresh);
    } catch (e) {
      setError(String(e));
    } finally {
      setMeshToggling(false);
    }
  }

  function openForm() {
    setInviteInput("");
    setTrustScope("circle");
    setFormError(null);
    setShowForm(true);
  }

  function closeForm() {
    setShowForm(false);
    setFormError(null);
  }

  async function handleAddPeer() {
    const { peerId, address } = parseInvite(inviteInput);
    if (!peerId) {
      setFormError("Peer ID or invite link is required.");
      return;
    }
    setSubmitting(true);
    setFormError(null);
    try {
      await api.addMeshPeer({ peer_id: peerId, trust_scope: trustScope, address });
      closeForm();
      load();
    } catch (e) {
      setFormError(String(e));
    } finally {
      setSubmitting(false);
    }
  }

  async function handleRemove(peer: MeshPeer) {
    setBusyId(peer.peer_id);
    try {
      await api.removeMeshPeer(peer.peer_id);
      load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyId(null);
    }
  }

  function openTopUp(peer: MeshPeer) {
    setTopUpTarget(peer);
    setTopUpAmount("");
    setTopUpError(null);
  }

  function closeTopUp() {
    setTopUpTarget(null);
    setTopUpError(null);
  }

  async function handleTopUp() {
    if (!topUpTarget) return;
    const sats = Number(topUpAmount);
    if (!Number.isFinite(sats) || sats <= 0) {
      setTopUpError("Enter a whole number of sats greater than zero.");
      return;
    }
    setTopUpSubmitting(true);
    setTopUpError(null);
    try {
      const result = await api.topUpMeshPeer(topUpTarget.peer_id, Math.round(sats) * 1000);
      setPeers((prev) =>
        prev.map((p) =>
          p.peer_id === result.peer_id
            ? { ...p, credit_balance_millisats: result.credit_balance_millisats }
            : p,
        ),
      );
      closeTopUp();
    } catch (e) {
      setTopUpError(String(e));
    } finally {
      setTopUpSubmitting(false);
    }
  }

  function handleCopyInvite() {
    if (!self?.invite_url) return;
    navigator.clipboard.writeText(self.invite_url).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }

  return (
    <div className="screen screen--mesh">
      <PageHeader
        title="Mesh"
        action={
          <Button size="sm" variant="primary" className="page-header-btn" onPress={openForm}>
            <Plus size={14} /> Add trusted peer
          </Button>
        }
      />

      {loading && <p className="muted-12">Loading mesh…</p>}
      {error && <p className="muted-12 text-error">{error}</p>}

      {!loading && meshEnabledSetting !== null && (
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 16 }}>
          <Switch
            aria-label="Enable mesh"
            isSelected={meshEnabledSetting}
            isDisabled={meshToggling}
            onChange={toggleMesh}
          >
            {/* Switch.Content wires up the aria-label — without it the toggle has no accessible name. */}
            <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
          </Switch>
          <span className="muted-12">Enable mesh</span>
          {/* Turning mesh ON hot-builds the real transport synchronously
              (#132 follow-up) — `toggleMesh` already re-fetched `self` by
              the time this renders, so `meshToggling` is the only normal
              window where the two can disagree. Still mismatched once that
              settles only happens two ways, and they need different advice:
              turning OFF never tears down an already-running transport (by
              design — see the setting's own docs), so it staying live really
              is "until you restart"; turning ON and staying dark means the
              hot-build itself failed (no `mesh` feature in this build, or a
              real startup error) — telling the user to restart would be
              wrong, since restarting cannot fix either of those. */}
          {!meshToggling && self && meshEnabledSetting !== self.mesh_enabled && (
            <span className="muted-12 text-error">
              {meshEnabledSetting
                ? "Mesh couldn't start — check the server logs (this build may not include mesh support)."
                : "Still connected to peers until you restart pond-server."}
            </span>
          )}
        </div>
      )}

      {!loading && self && !self.mesh_enabled && (
        <div className="empty-state">
          <Share2 size={32} />
          <span>
            {meshEnabledSetting
              ? "Mesh is enabled but couldn't start — check the server logs."
              : "Mesh is disabled. Flip the switch above to invite trusted peers."}
          </span>
        </div>
      )}

      {!loading && self?.mesh_enabled && (
        <div style={{ display: "flex", gap: 32, flexWrap: "wrap", marginBottom: 24 }}>
          <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 10 }}>
            <div
              style={{
                border: "1px solid var(--color-border)",
                borderRadius: 12,
                padding: 10,
                background: "#fff",
                lineHeight: 0,
              }}
            >
              <canvas ref={canvasRef} width={180} height={180} />
            </div>
            <Button size="sm" variant="secondary" onPress={handleCopyInvite}>
              <Copy size={13} style={{ display: "inline" }} /> {copied ? "Copied!" : "Copy invite link"}
            </Button>
          </div>
          <div style={{ flex: 1, minWidth: 220 }}>
            <p className="muted-12" style={{ marginBottom: 8 }}>Your Pond ID</p>
            <code style={{ fontSize: 12, wordBreak: "break-all" }}>{self.peer_id}</code>
            <p className="muted-12" style={{ marginTop: 16 }}>
              Share the QR code or invite link with a trusted device or friend's Pond.
              They can scan it or paste it when adding you as a trusted peer.
            </p>
          </div>
        </div>
      )}

      {!loading && !error && peers.length === 0 && (
        <div className="empty-state">
          <Share2 size={32} />
          <span>No trusted peers yet.</span>
          <button className="empty-state__cta" onClick={openForm}>
            <Plus size={14} /> Add trusted peer
          </button>
        </div>
      )}

      {peers.length > 0 && (
        <div className="devices-grid">
          {peers.map((p) => (
            <div
              key={p.peer_id}
              className={`device-card${!p.connected ? " device-card--offline" : ""}`}
            >
              <div className="device-card__top">
                <span className="device-card__icon">
                  <Share2 size={22} />
                </span>
                <div className="device-card__info">
                  <div className="device-card__name">{truncatePeerId(p.peer_id)}</div>
                  <code className="device-card__ip">{formatMillisats(p.credit_balance_millisats)}</code>
                </div>
              </div>

              <div className="device-card__chips">
                <span className={`device-card__chip device-card__chip--${p.connected ? "online" : "offline"}`}>
                  {p.connected ? <Wifi size={11} /> : <WifiOff size={11} />}
                  {p.connected ? "connected" : "offline"}
                </span>
                <span className="device-card__chip">
                  {p.trust_scope === "self_owned" ? "own device" : "circle"}
                </span>
                {capabilities[p.peer_id]?.inference_available && (
                  <span className="device-card__chip" title="Offers compute to borrow">
                    <Cpu size={11} /> inference
                  </span>
                )}
                {capabilities[p.peer_id]?.lightning_available && (
                  <span className="device-card__chip" title="Can settle over Lightning">
                    <Zap size={11} /> lightning
                  </span>
                )}
              </div>

              <div className="device-card__actions">
                <button
                  className="device-card__action-btn"
                  onClick={() => openTopUp(p)}
                  disabled={busyId === p.peer_id}
                  type="button"
                >
                  <Coins size={12} /> Top up
                </button>
                <button
                  className="device-card__action-btn"
                  onClick={() => handleRemove(p)}
                  disabled={busyId === p.peer_id}
                  type="button"
                >
                  <Trash2 size={12} /> {busyId === p.peer_id ? "Removing…" : "Remove"}
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {settlementStatus && (
        <div style={{ marginTop: 24 }}>
          <Separator />
          <div style={{ display: "flex", alignItems: "center", gap: 8, margin: "16px 0 8px" }}>
            <h3 style={{ fontSize: 13, fontWeight: 600, margin: 0 }}>Settlement</h3>
            <span
              className={`device-card__chip device-card__chip--${settlementStatus.configured ? "online" : "offline"}`}
              title="The exchange rate is set via the settings API, not this UI"
            >
              {settlementStatus.configured
                ? `${settlementStatus.millisats_per_token} msat/token`
                : "not configured"}
            </span>
          </div>

          {!settlementStatus.configured && (
            <p className="muted-12">
              No exchange rate is set yet, so usage accumulates but nothing gets paid
              automatically.
            </p>
          )}
          {settlementStatus.configured && settlementStatus.peers.length === 0 && (
            <p className="muted-12">No pending usage with any trusted peer right now.</p>
          )}
          {settlementStatus.peers.length > 0 && (
            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              {settlementStatus.peers.map((p) => (
                <div
                  key={p.peer_id}
                  style={{ display: "flex", justifyContent: "space-between", fontSize: 12 }}
                >
                  <span>{truncatePeerId(p.peer_id)}</span>
                  <span className="muted-12">
                    {p.pending_tokens.toLocaleString()} tokens
                    {settlementStatus.configured &&
                      ` · ${formatMillisats(p.pending_millisats)} owed`}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {showForm && (
        <div className="sched-modal__overlay" onClick={closeForm}>
          <div className="sched-modal__dialog" onClick={(e) => e.stopPropagation()}>
            <div className="sched-modal__header">
              <h2 className="sched-modal__title">Add trusted peer</h2>
              <button className="sched-modal__close" onClick={closeForm} aria-label="Close">
                <X size={16} />
              </button>
            </div>
            <Separator />

            <div className="sched-modal__body">
              <div className="sched-modal__field">
                <label className="sched-modal__label">Peer ID or invite link</label>
                <input
                  className="sched-modal__input"
                  placeholder="pond-mesh://invite?peer=... or a raw peer ID"
                  value={inviteInput}
                  onChange={(e) => setInviteInput(e.target.value)}
                  autoFocus
                />
              </div>

              <div className="sched-modal__field">
                <label className="sched-modal__label">Trust scope</label>
                <select
                  className="sched-modal__select"
                  value={trustScope}
                  onChange={(e) => setTrustScope(e.target.value as "self_owned" | "circle")}
                >
                  <option value="circle">Circle (friend / family Pond)</option>
                  <option value="self_owned">My own device</option>
                </select>
              </div>

              {formError && <p className="text-error text-error--sm">{formError}</p>}
            </div>

            <Separator />

            <div className="sched-modal__footer">
              <Button size="sm" variant="ghost" onPress={closeForm}>Cancel</Button>
              <Button
                size="sm"
                variant="primary"
                isDisabled={submitting || !inviteInput.trim()}
                onPress={handleAddPeer}
              >
                {submitting ? "Adding…" : "Add peer"}
              </Button>
            </div>
          </div>
        </div>
      )}

      {topUpTarget && (
        <div className="sched-modal__overlay" onClick={closeTopUp}>
          <div className="sched-modal__dialog" onClick={(e) => e.stopPropagation()}>
            <div className="sched-modal__header">
              <h2 className="sched-modal__title">Top up {truncatePeerId(topUpTarget.peer_id)}</h2>
              <button className="sched-modal__close" onClick={closeTopUp} aria-label="Close">
                <X size={16} />
              </button>
            </div>
            <Separator />

            <div className="sched-modal__body">
              <p className="muted-12" style={{ marginBottom: 12 }}>
                Manual top-up — a stand-in until Lightning settlement is wired in.
                Current balance: {formatMillisats(topUpTarget.credit_balance_millisats)}.
              </p>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Amount (sats)</label>
                <input
                  className="sched-modal__input"
                  type="number"
                  min={1}
                  placeholder="1000"
                  value={topUpAmount}
                  onChange={(e) => setTopUpAmount(e.target.value)}
                  autoFocus
                />
              </div>

              {topUpError && <p className="text-error text-error--sm">{topUpError}</p>}
            </div>

            <Separator />

            <div className="sched-modal__footer">
              <Button size="sm" variant="ghost" onPress={closeTopUp}>Cancel</Button>
              <Button
                size="sm"
                variant="primary"
                isDisabled={topUpSubmitting || !topUpAmount.trim()}
                onPress={handleTopUp}
              >
                {topUpSubmitting ? "Adding…" : "Top up"}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
