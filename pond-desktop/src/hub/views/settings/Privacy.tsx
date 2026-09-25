import { useState, useEffect, useCallback, useRef } from "react";
import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card, Row, Toggle } from "./controls";
import { api } from "../../../api/PondApiClient";
import type { Settings } from "../../../api/types";

const SHIELD_PATH = "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z";

// ─── Types ────────────────────────────────────────────────────

interface PrivacyDetailProps {
  go: (route: string) => void;
}

// ─── Flash banner ─────────────────────────────────────────────

interface FlashBannerProps {
  flash: { text: string; ok: boolean } | null;
}
function FlashBanner({ flash }: FlashBannerProps) {
  if (!flash) return null;
  return (
    <div
      style={{
        padding: "8px 12px",
        borderRadius: 6,
        fontSize: 13,
        background: flash.ok ? "#f0fdf4" : "#fef2f2",
        color: flash.ok ? "#16a34a" : "#dc2626",
        border: `1px solid ${flash.ok ? "#bbf7d0" : "#fecaca"}`,
      }}
      role="status"
      aria-live="polite"
    >
      {flash.text}
    </div>
  );
}

// ─── Confirm modal ────────────────────────────────────────────

interface ConfirmModalProps {
  message: string;
  onConfirm: () => void;
  onCancel: () => void;
  busy: boolean;
}
function ConfirmModal({ message, onConfirm, onCancel, busy }: ConfirmModalProps) {
  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 1000,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        background: "rgba(0,0,0,0.35)",
      }}
      onClick={!busy ? onCancel : undefined}
    >
      <div
        style={{
          background: "#fff",
          borderRadius: 12,
          padding: "24px 28px",
          maxWidth: 360,
          width: "90%",
          boxShadow: "0 8px 32px rgba(0,0,0,0.18)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <p style={{ margin: "0 0 20px", fontSize: 14, color: "#1e293b", lineHeight: 1.5 }}>
          {message}
        </p>
        <div style={{ display: "flex", gap: 10, justifyContent: "flex-end" }}>
          <button
            className="mrow__btn"
            type="button"
            onClick={onCancel}
            disabled={busy}
          >
            Cancel
          </button>
          <button
            className="mrow__btn"
            type="button"
            onClick={onConfirm}
            disabled={busy}
            style={{
              background: "#dc2626",
              color: "#fff",
              borderColor: "#dc2626",
              opacity: busy ? 0.6 : 1,
            }}
            aria-label="Confirm clear conversations"
          >
            {busy ? "Clearing..." : "Clear All"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── Component ───────────────────────────────────────────────

export function PrivacyDetail({ go }: PrivacyDetailProps) {
  const [settings, setSettings] = useState<Partial<Settings>>({});
  const [sessionCount, setSessionCount] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [confirmClear, setConfirmClear] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [flash, setFlash] = useState<{ text: string; ok: boolean } | null>(null);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  function showFlash(text: string, ok = true) {
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlash({ text, ok });
    flashTimer.current = setTimeout(() => setFlash(null), 3000);
  }

  const loadData = useCallback(async () => {
    setLoading(true);
    try {
      const [s, sessions] = await Promise.all([
        api.getSettings(),
        api.listSessions(),
      ]);
      setSettings(s);
      setSessionCount(sessions.length);
    } catch (e) {
      console.warn("[PrivacyDetail] API offline — using defaults:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadData();
    return () => {
      if (flashTimer.current) clearTimeout(flashTimer.current);
    };
  }, [loadData]);

  async function handleToggle(field: keyof Settings, value: boolean) {
    // Optimistic update
    setSettings((prev) => ({ ...prev, [field]: value }));
    try {
      const updated = await api.updateSettings({ [field]: value });
      setSettings(updated);
      showFlash("Setting saved.");
    } catch (e) {
      // Revert on error
      setSettings((prev) => ({ ...prev, [field]: !value }));
      showFlash(`Failed to save: ${String(e)}`, false);
    }
  }

  async function handleClearConversations() {
    setClearing(true);
    try {
      // No bulk-delete endpoint, so sessions are deleted one by one.
      const sessions = await api.listSessions();
      await Promise.all(sessions.map((s) => api.deleteSession(s.id)));
      setSessionCount(0);
      setConfirmClear(false);
      showFlash("Conversation history cleared.");
    } catch (e) {
      showFlash(`Clear failed: ${String(e)}`, false);
    } finally {
      setClearing(false);
    }
  }

  const sessionSub = loading
    ? "Loading..."
    : sessionCount !== null
      ? `Stored locally · ${sessionCount} session${sessionCount !== 1 ? "s" : ""}`
      : "Stored locally";

  return (
    <>
      {confirmClear && (
        <ConfirmModal
          message="This will permanently delete all conversation history. This action cannot be undone."
          onConfirm={handleClearConversations}
          onCancel={() => setConfirmClear(false)}
          busy={clearing}
        />
      )}

      <DetailShell
        title="Privacy"
        subtitle="Goose lives entirely in your home."
        accent="#16A34A"
        onBack={() => go("settings")}
      >
        <FlashBanner flash={flash} />

        {/* Hero card */}
        <div className="privacy-hero">
          <span className="privacy-hero__icon">
            <HubIco d={SHIELD_PATH} size={30} color="#fff" />
          </span>
          <div>
            <div className="privacy-hero__title">Everything runs on-device</div>
            <div className="privacy-hero__sub">
              No audio, video or home data ever leaves this hub. No cloud account required.
            </div>
          </div>
        </div>

        <Card title="Data &amp; sensors">
          <Row
            label="Microphone"
            sub="Used only after the wake word"
            control={
              <Toggle
                on={settings.mic_enabled ?? true}
                onChange={(v) => handleToggle("mic_enabled", v)}
              />
            }
          />
          <Row
            label="Cameras"
            sub="Feeds stay local; nothing is uploaded"
            control={
              <Toggle
                on={settings.cameras_enabled ?? true}
                onChange={(v) => handleToggle("cameras_enabled", v)}
              />
            }
          />
          <Row
            label="Cloud fallback"
            sub="Use a cloud model if local fails"
            control={
              <Toggle
                on={settings.cloud_fallback_enabled ?? false}
                onChange={(v) => handleToggle("cloud_fallback_enabled", v)}
              />
            }
          />
          <Row
            label="Anonymous diagnostics"
            sub="Share crash logs to improve Goose"
            control={
              <Toggle
                on={settings.telemetry_enabled ?? false}
                onChange={(v) => handleToggle("telemetry_enabled", v)}
              />
            }
          />
        </Card>

        <Card title="Storage">
          <Row
            label="Conversations"
            sub={sessionSub}
            control={
              <button
                className="mrow__btn"
                type="button"
                onClick={() => setConfirmClear(true)}
                disabled={loading || sessionCount === 0}
                aria-label="Clear all conversation history"
              >
                Clear
              </button>
            }
          />
          <Row
            label="Camera clips"
            sub="Local · auto-deletes after 7 days"
            control={
              /* TODO: open camera-clips retention modal when camera storage API is available */
              <button
                className="mrow__btn"
                type="button"
                disabled
                title="Camera clip management coming soon"
              >
                Manage
              </button>
            }
          />
        </Card>
      </DetailShell>
    </>
  );
}
