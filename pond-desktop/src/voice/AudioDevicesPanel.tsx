import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, Loader2, Mic, MicOff, RefreshCw, Volume2 } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { AudioChoice, AudioDeviceStatus, Settings } from "../api/types";
import { Toggle } from "../hub/views/settings/controls";
import { describeBoard, describeChoice, describeWhen, deviceOptions, isAudioStatus } from "./audioDevices";
import "./audio-devices.css";

/** Often enough that a headset plugged in shows up while the page is open; a GET reads a file. */
const POLL_MS = 4_000;

type PinKey = "audio_input_device" | "audio_output_device";

interface Props {
  /** `hub` for the touch panel, `catalogue` for the dense desktop list. */
  variant: "hub" | "catalogue";
  /** The hub has no catalogue rows, so the pickers and the chime switch live here. */
  withPickers?: boolean;
  /** Every fresh status, so pickers rendered elsewhere list what the last test found. */
  onStatus?: (status: AudioDeviceStatus) => void;
}

export function AudioDevicesPanel({ variant, withPickers = false, onStatus }: Props) {
  const [status, setStatus] = useState<AudioDeviceStatus | null>(null);
  const [settings, setSettings] = useState<Partial<Settings> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"test" | "chime" | null>(null);
  const [said, setSaid] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const alive = useRef(true);
  // A ref, so a parent re-rendering with a new callback does not restart the poll.
  const report = useRef(onStatus);
  report.current = onStatus;

  const load = useCallback(async () => {
    try {
      const next = await api.getAudioDevices();
      if (!alive.current) return;
      if (isAudioStatus(next)) {
        setStatus(next);
        report.current?.(next);
        setError(null);
      }
    } catch {
      if (alive.current) setError("The pond is not answering, so the devices in use are unknown.");
    } finally {
      if (alive.current) setNow(Date.now());
    }
  }, []);

  useEffect(() => {
    alive.current = true;
    void load();
    if (withPickers) {
      api.getSettings().then(
        (s) => alive.current && setSettings(s),
        () => undefined,
      );
    }
    const timer = window.setInterval(() => void load(), POLL_MS);
    return () => {
      alive.current = false;
      window.clearInterval(timer);
    };
  }, [load, withPickers]);

  async function test(chime: boolean) {
    setBusy(chime ? "chime" : "test");
    setSaid(null);
    try {
      const next = await api.testAudioDevices(chime);
      if (!alive.current) return;
      if (isAudioStatus(next)) {
        setStatus(next);
        report.current?.(next);
        const speaker = next.output.device?.label;
        setSaid(chime && speaker ? `Played a test sound on ${speaker}.` : "Tested every device just now.");
      }
      setError(null);
    } catch {
      if (alive.current) setError("The test did not finish. Try again in a moment.");
    } finally {
      if (alive.current) {
        setBusy(null);
        setNow(Date.now());
      }
    }
  }

  async function pin(key: PinKey, value: string) {
    setSettings((s) => ({ ...(s ?? {}), [key]: value }));
    setSaid(value ? "Saved. Testing it now." : "Saved. The pond picks a device that works.");
    try {
      // The server chooses again on its own when a pin changes; the poll shows the result.
      await api.updateSettings({ [key]: value });
      window.setTimeout(() => void load(), 1_500);
    } catch {
      setSaid(null);
      setError("That choice was not saved. Try again.");
    }
  }

  async function setSwitch(key: "audio_sound_test" | "audio_manage_system", on: boolean) {
    setSettings((s) => ({ ...(s ?? {}), [key]: on }));
    try {
      await api.updateSettings({ [key]: on });
    } catch {
      setError("That switch was not saved. Try again.");
    }
  }

  const board = status ? describeBoard(status) : null;

  const rows: { key: PinKey; title: string; choice: AudioChoice | undefined }[] = [
    { key: "audio_input_device", title: "Microphone", choice: status?.input },
    { key: "audio_output_device", title: "Speaker", choice: status?.output },
  ];

  return (
    <div className={`adev adev--${variant}`}>
      {error && (
        <p className="adev__error" role="status">
          {error}
        </p>
      )}
      <ul className="adev__list">
        {rows.map(({ key, title, choice }) => {
          const line = choice ? describeChoice(choice) : null;
          const Icon =
            line?.problem ? AlertTriangle : choice?.basis === "mic_off" ? MicOff : key === "audio_input_device" ? Mic : Volume2;
          const pinned = settings?.[key] ?? "";
          return (
            <li className="adev__row" key={key}>
              <Icon className="adev__icon" size={18} strokeWidth={2} aria-hidden="true" />
              <div className="adev__text">
                <span className="adev__title">{title}</span>
                <span className="adev__line" data-problem={line?.problem || undefined}>
                  {line ? line.text : "Finding out what is connected."}
                </span>
              </div>
              {withPickers && (
                <select
                  className="adev__select"
                  aria-label={`${title} to use`}
                  value={pinned}
                  disabled={settings === null}
                  onChange={(e) => void pin(key, e.target.value)}
                >
                  {deviceOptions(choice, pinned).map((o) => (
                    <option key={o.value} value={o.value}>
                      {o.label}
                    </option>
                  ))}
                </select>
              )}
            </li>
          );
        })}
      </ul>

      {board && (
        <p className="adev__line adev__board" data-problem={board.problem || undefined}>
          {board.text}
        </p>
      )}

      {withPickers && (
        <div className="adev__row adev__row--switch">
          <div className="adev__text">
            <span className="adev__title">Find them by sound</span>
            <span className="adev__line">
              A short, soft sound checks that a speaker and a microphone hear each other. Off, they
              are checked silently, as a Bluetooth headset always is: the room cannot hear it.
            </span>
          </div>
          {settings && (
            // Remounted on load: Toggle reads `on` once, and settings arrive after mount.
            <Toggle
              key={`sound-test-${settings.audio_sound_test !== false}`}
              on={settings.audio_sound_test !== false}
              onChange={(on) => void setSwitch("audio_sound_test", on)}
              label="Find them by sound"
            />
          )}
        </div>
      )}

      {withPickers && (
        <div className="adev__row adev__row--switch">
          <div className="adev__text">
            <span className="adev__title">Set up the board's sound</span>
            <span className="adev__line">
              On a Linux board like the nano, the pair the pond picks becomes every program's
              default, unmuted, and a level too low to be heard is turned up. A Bluetooth headset
              the pond uses becomes the default either way, as that is the only way to reach it. A
              Mac's own settings are never changed.
            </span>
          </div>
          {settings && (
            <Toggle
              key={`manage-system-${settings.audio_manage_system !== false}`}
              on={settings.audio_manage_system !== false}
              onChange={(on) => void setSwitch("audio_manage_system", on)}
              label="Set up the board's sound"
            />
          )}
        </div>
      )}

      <div className="adev__actions">
        <button className="adev__btn" type="button" disabled={busy !== null} onClick={() => void test(false)}>
          {busy === "test" ? <Loader2 className="adev__spin" size={14} aria-hidden="true" /> : <RefreshCw size={14} aria-hidden="true" />}
          {busy === "test" ? "Testing" : "Test again"}
        </button>
        <button className="adev__btn" type="button" disabled={busy !== null} onClick={() => void test(true)}>
          {busy === "chime" ? <Loader2 className="adev__spin" size={14} aria-hidden="true" /> : <Volume2 size={14} aria-hidden="true" />}
          Play a test sound
        </button>
        <span className="adev__when" aria-live="polite">
          {said ?? (status ? describeWhen(status, now) : "")}
        </span>
      </div>
    </div>
  );
}
