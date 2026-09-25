import { useState, useEffect, useCallback, useRef } from "react";
import { DetailShell } from "./DetailShell";
import { Card, Row } from "./controls";
import { HubIco } from "../../primitives/HubIco";
import { api } from "../../../api/PondApiClient";
import { useAppDispatch } from "../../../state/AppContext";
import type { Settings } from "../../../api/types";
import type { Device } from "../../../api/types";

const CHEVR_PATH = "M9 6l6 6-6 6";

// ─── Hardcoded IANA timezone list (offline-first) ─────────────
const TIMEZONES = [
  "UTC",
  "Africa/Nairobi",
  "Africa/Lagos",
  "Africa/Cairo",
  "America/New_York",
  "America/Chicago",
  "America/Denver",
  "America/Los_Angeles",
  "America/Toronto",
  "America/Vancouver",
  "America/Sao_Paulo",
  "Europe/London",
  "Europe/Paris",
  "Europe/Berlin",
  "Europe/Rome",
  "Europe/Moscow",
  "Asia/Tokyo",
  "Asia/Shanghai",
  "Asia/Kolkata",
  "Asia/Dubai",
  "Asia/Singapore",
  "Australia/Sydney",
];

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

// ─── Inline editable field ────────────────────────────────────

interface EditableRowProps {
  label: string;
  value: string;
  onSave: (v: string) => Promise<void>;
  placeholder?: string;
  testId?: string;
}

function EditableRow({ label, value, onSave, placeholder, testId }: EditableRowProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const [saving, setSaving] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  // Sync draft when parent value changes (e.g. after save)
  useEffect(() => {
    if (!editing) setDraft(value);
  }, [value, editing]);

  function startEdit() {
    setDraft(value);
    setEditing(true);
    // Focus on next tick after render
    setTimeout(() => inputRef.current?.focus(), 20);
  }

  async function commit() {
    if (draft.trim() === value) {
      setEditing(false);
      return;
    }
    setSaving(true);
    try {
      await onSave(draft.trim() || value);
    } finally {
      setSaving(false);
      setEditing(false);
    }
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter") { e.preventDefault(); void commit(); }
    if (e.key === "Escape") { setEditing(false); setDraft(value); }
  }

  if (editing) {
    return (
      <div className="srow" style={{ alignItems: "center" }}>
        <span className="srow__text">
          <span className="srow__label">{label}</span>
        </span>
        <span className="srow__control" style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <input
            ref={inputRef}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder={placeholder}
            disabled={saving}
            data-testid={testId}
            style={{
              height: 30,
              border: "1px solid #a5b4fc",
              borderRadius: 6,
              padding: "0 8px",
              fontSize: 13,
              width: 160,
              background: "#fff",
              color: "#1e293b",
              outline: "none",
            }}
            aria-label={`Edit ${label}`}
          />
          <button
            className="mrow__btn"
            type="button"
            onClick={void commit}
            onMouseDown={(e) => { e.preventDefault(); void commit(); }}
            disabled={saving}
          >
            {saving ? "Saving..." : "Save"}
          </button>
          <button
            className="mrow__btn"
            type="button"
            onClick={() => { setEditing(false); setDraft(value); }}
            disabled={saving}
          >
            Cancel
          </button>
        </span>
      </div>
    );
  }

  return (
    <Row
      label={label}
      sub={value || placeholder}
      control={<HubIco d={CHEVR_PATH} size={16} color="var(--color-text-tertiary,#566178)" />}
      onClick={startEdit}
    />
  );
}

// ─── Component ───────────────────────────────────────────────

interface AccountDetailProps {
  go: (route: string) => void;
}

export function AccountDetail({ go }: AccountDetailProps) {
  const dispatch = useAppDispatch();
  const [settings, setSettings] = useState<Partial<Settings>>({});
  const [devices, setDevices] = useState<Device[]>([]);
  const [loading, setLoading] = useState(true);
  const [signingOut, setSigningOut] = useState(false);
  const [restarting, setRestarting] = useState(false);
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
      const [s, devs] = await Promise.all([
        api.getSettings(),
        api.listDevices(),
      ]);
      setSettings(s);
      setDevices(devs);
    } catch (e) {
      console.warn("[AccountDetail] API offline — using defaults:", e);
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

  async function saveSetting(field: keyof Settings, value: string) {
    try {
      const updated = await api.updateSettings({ [field]: value });
      setSettings(updated);
      showFlash("Saved.");
    } catch (e) {
      showFlash(`Failed to save: ${String(e)}`, false);
    }
  }

  async function handleSignOut() {
    setSigningOut(true);
    try {
      api.setToken(null);
      dispatch({ type: "SET_SESSION_TOKEN", payload: null });
      // Reload to onboarding / login screen
      window.location.reload();
    } catch {
      setSigningOut(false);
      showFlash("Sign out failed.", false);
    }
  }

  async function handleRestartOnboarding() {
    if (restarting) return;
    setRestarting(true);
    try {
      await api.resetOnboarding();
      // Route back into the onboarding wizard. Settings are preserved server-side.
      dispatch({ type: "SET_NEEDS_ONBOARDING", payload: true });
    } catch (e) {
      setRestarting(false);
      showFlash(`Couldn't restart setup: ${String(e)}`, false);
    }
  }

  const userName = settings.user_name ?? "";
  const homeName = settings.home_name ?? settings.weather_location_name ?? "Goose Pond";
  const timezone = settings.timezone ?? "UTC";

  const roomSet = new Set(
    devices
      .filter((d) => typeof d.room === "string")
      .map((d) => d.room as string),
  );
  const roomCount = roomSet.size || devices.length;

  const initials = userName ? userName.charAt(0).toUpperCase() : "?";

  return (
    <DetailShell
      title="Account"
      subtitle="Your profile and home."
      accent="#475569"
      onBack={() => go("settings")}
    >
      <FlashBanner flash={flash} />

      {/* Profile hero */}
      <div className="acct-hero">
        <span className="acct-hero__avatar" aria-label={`Avatar for ${userName || "user"}`}>
          {loading ? "…" : initials}
        </span>
        <div>
          <div className="acct-hero__name" data-testid="acct-hero-name">
            {loading ? "Loading..." : userName || "Unnamed"}
          </div>
          <div className="acct-hero__home">
            {loading ? "" : `${homeName} · ${roomCount} ${roomCount === 1 ? "room" : "rooms"}`}
          </div>
        </div>
        <span className="acct-hero__badge">On-device</span>
      </div>

      <Card title="Profile">
        <EditableRow
          label="Name"
          value={userName}
          onSave={(v) => saveSetting("user_name", v)}
          placeholder="Your name"
          testId="acct-name-input"
        />
        <EditableRow
          label="Home name"
          value={homeName}
          onSave={(v) => saveSetting("home_name", v)}
          placeholder="Goose Pond"
          testId="acct-home-input"
        />
        {/* Time zone — static select (no inline edit UX needed) */}
        <div className="srow" style={{ alignItems: "center" }}>
          <span className="srow__text">
            <span className="srow__label">Time zone</span>
            <span className="srow__sub">{timezone}</span>
          </span>
          <span className="srow__control">
            <select
              value={timezone}
              onChange={(e) => void saveSetting("timezone", e.target.value)}
              aria-label="Select time zone"
              data-testid="acct-timezone-select"
              style={{
                height: 30,
                border: "1px solid #e2e8f0",
                borderRadius: 6,
                padding: "0 28px 0 8px",
                fontSize: 13,
                background: "#fff",
                color: "#1e293b",
                appearance: "none",
                backgroundImage: "url(\"data:image/svg+xml,%3Csvg width='10' height='6' viewBox='0 0 10 6' fill='none' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%238A8A8A' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E\")",
                backgroundRepeat: "no-repeat",
                backgroundPosition: "right 8px center",
                cursor: "pointer",
              }}
            >
              {TIMEZONES.map((tz) => (
                <option key={tz} value={tz}>{tz}</option>
              ))}
            </select>
          </span>
        </div>
      </Card>

      <Card title="About">
        <Row
          label="Goose In A Pond"
          sub="Version 2.0 · on-device build"
          control={<span className="set-row__badge">Up to date</span>}
        />
        <Row
          label="Help &amp; feedback"
          control={<HubIco d={CHEVR_PATH} size={16} color="var(--color-text-tertiary,#566178)" />}
          onClick={() => {
            /* TODO: open help/feedback modal */
          }}
        />
        <Row
          label="Restart onboarding"
          sub="Walk through first-time setup again. Your settings are kept."
          control={
            <button
              className="acct-restart-btn"
              type="button"
              disabled={restarting}
              onClick={() => void handleRestartOnboarding()}
              data-testid="restart-onboarding-btn"
            >
              {restarting ? "Restarting..." : "Start over"}
            </button>
          }
        />
      </Card>

      <button
        className="signout-btn"
        type="button"
        disabled={signingOut}
        onClick={() => void handleSignOut()}
        aria-label="Sign out"
        data-testid="signout-btn"
      >
        {signingOut ? "Signing out..." : "Sign out"}
      </button>
    </DetailShell>
  );
}
