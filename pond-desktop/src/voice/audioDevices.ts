// What the pond is using to hear and speak, and why, in the household's words. Pure, so both
// settings surfaces say exactly the same thing and the sentences can be tested.

import type {
  AudioChoice,
  AudioDeviceStatus,
  AudioLevelChange,
  AudioProbeOutcome,
  ProbedAudioDevice,
} from "../api/types";

export interface ChoiceLine {
  /** One sentence: what is in use, and why. */
  text: string;
  /** Something the household should act on; the sentence says what. */
  problem: boolean;
}

const NOUN = { input: "microphone", output: "speaker" } as const;

function heard(outcome: AudioProbeOutcome): string {
  switch (outcome.result) {
    case "hearing":
      return "hearing sound";
    case "silent":
      return "hearing only silence";
    case "accepting":
      return "taking audio";
    case "stalled":
      return "not responding";
    case "failed":
      return "not opening";
    case "untested":
      return "not tested yet";
  }
}

/** Why a passed-over device lost: by its outcome, or "not connected" when it is not listed. */
function whyNot(device: ProbedAudioDevice | undefined): string {
  if (!device) return "is not connected";
  switch (device.outcome.result) {
    case "silent":
      return "hears only silence";
    case "stalled":
      return "did not respond";
    case "failed":
      return device.outcome.reason.includes("not connected")
        ? "is not connected"
        : "would not open";
    default:
      return "did not pass its test";
  }
}

const SILENCE_HINT = "Check its mute switch, and that the pond may use the microphone.";

/** "heard by the mic" for a speaker, "which heard the speaker" for a mic. */
function byEar(choice: AudioChoice, partner: string): string {
  return choice.direction === "output" ? `heard by ${partner}` : `which heard ${partner}`;
}

export function describeChoice(choice: AudioChoice): ChoiceLine {
  const noun = NOUN[choice.direction];
  const device = choice.device;
  const state = heard(choice.outcome);
  const silent = choice.outcome.result === "silent";
  const hint = silent ? ` ${SILENCE_HINT}` : "";
  const partner = choice.heard_with ?? null;

  if (device && partner) {
    const ear = byEar(choice, partner);
    if (choice.basis === "pinned") return { text: `${device.label}, your choice, ${ear}.`, problem: false };
    if (choice.basis === "fallback" && choice.pinned && choice.pinned !== device.id) {
      const pinned = choice.devices.find((d) => d.id === choice.pinned);
      const what = choice.direction === "output" ? "was not heard" : "did not hear it";
      return {
        text: `${device.label}, ${ear}: your choice, ${pinned?.label ?? choice.pinned}, ${what}.`,
        problem: false,
      };
    }
    return { text: `${device.label}, ${ear}.`, problem: false };
  }
  if (device && choice.unheard && choice.direction === "output") {
    return {
      text: `${device.label}, but no microphone heard it play its test sound. Check that it is switched on and turned up.`,
      problem: true,
    };
  }

  switch (choice.basis) {
    case "mic_off":
      return { text: "Off in Privacy. The pond is not listening.", problem: false };
    case "no_device":
      return { text: `No ${noun} found. Connect one and the pond starts using it.`, problem: true };
    case "not_tested":
      return {
        text: device
          ? `${device.label} will be tried first. It is tested when voice starts, or when you test again.`
          : `Not tested yet. It is tested when voice starts, or when you test again.`,
        problem: false,
      };
  }
  if (!device) return { text: `No ${noun} found.`, problem: true };

  switch (choice.basis) {
    case "pinned":
      return { text: `${device.label}, your choice, ${state}.${hint}`, problem: silent };
    case "system_default":
      return { text: `${device.label}, the system default, ${state}.${hint}`, problem: silent };
    case "plugged_in": {
      const how = device.kind === "bluetooth" ? "connected over Bluetooth" : "plugged in";
      return { text: `${device.label}, ${how}, ${state}.${hint}`, problem: silent };
    }
    case "fallback": {
      const pin = choice.pinned;
      if (pin && pin !== device.id) {
        const pinned = choice.devices.find((d) => d.id === pin);
        const name = pinned?.label ?? pin;
        return {
          text: `${device.label}, ${state}: your choice, ${name}, ${whyNot(pinned)}.${hint}`,
          problem: silent,
        };
      }
      const def = choice.devices.find((d) => d.is_default && d.id !== device.id);
      return {
        text: def
          ? `${device.label}, ${state}: the system default, ${def.label}, ${whyNot(def)}.${hint}`
          : `${device.label}, ${state}.${hint}`,
        problem: silent,
      };
    }
    case "best_effort":
      return {
        text: silent
          ? `${device.label}, but every ${noun} hears only silence. ${SILENCE_HINT}`
          : `${device.label}, though no ${noun} passed its test. Check that it is connected and switched on.`,
        problem: true,
      };
  }
  return { text: `${device.label}, ${state}.`, problem: false };
}

function raisedPhrase(change: AudioLevelChange): string {
  return change.what === "mute"
    ? `unmuted ${change.device}`
    : `turned ${change.device} up from ${change.from} to ${change.to}`;
}

/** What the pond changed on the board itself, or null when it changed nothing there. */
export function describeBoard(status: AudioDeviceStatus): ChoiceLine | null {
  const parts: string[] = [];
  const raised = status.raised ?? [];
  if (raised.length) {
    parts.push(`So its test sound was heard, the pond ${raised.map(raisedPhrase).join(" and ")}.`);
  }
  const board = status.sound_server;
  if (board?.problems.length) {
    parts.push(`The board's own sound settings could not follow: ${board.problems.join("; ")}.`);
    return { text: parts.join(" "), problem: true };
  }
  if (board?.speaker && board.mic) {
    parts.push("Both are also the board's defaults for other programs.");
  } else if (board?.speaker) {
    parts.push("The speaker is also the board's default for other programs.");
  } else if (board?.mic) {
    parts.push("The microphone is also the board's default for other programs.");
  }
  return parts.length ? { text: parts.join(" "), problem: false } : null;
}

/** "Tested 12 seconds ago by the voice session." Says which process, since only voice hears. */
export function describeWhen(status: AudioDeviceStatus, now = Date.now()): string {
  const secs = Math.max(0, Math.round((now - status.updated_at_ms) / 1000));
  const ago =
    secs < 5
      ? "just now"
      : secs < 90
        ? `${secs} seconds ago`
        : secs < 5400
          ? `${Math.round(secs / 60)} minutes ago`
          : `${Math.round(secs / 3600)} hours ago`;
  const by = status.source === "voice" ? "by the voice session" : "by the pond";
  return `Checked ${ago} ${by}.`;
}

export interface DeviceOption {
  value: string;
  label: string;
}

/** A picker row is narrow, so the last test is a word or two, not the status sentence's phrase. */
function shortly(outcome: AudioProbeOutcome): string | null {
  switch (outcome.result) {
    case "hearing":
      return "hears sound";
    case "silent":
      return "silent";
    case "accepting":
      return "works";
    case "stalled":
      return "no response";
    case "failed":
      return "won't open";
    case "untested":
      return null;
  }
}

/** Picker rows: "Automatic" first, then each device with what its last test found. A pin that
 *  is not connected stays listed, or opening the picker would silently drop it. */
export function deviceOptions(choice: AudioChoice | undefined, pinned: string): DeviceOption[] {
  const rows: DeviceOption[] = [{ value: "", label: "Automatic" }];
  const devices = choice?.devices ?? [];
  for (const d of devices) {
    const parts: string[] = [];
    if (d.kind === "bluetooth") parts.push("Bluetooth");
    if (d.is_default) parts.push("default");
    const tested = shortly(d.outcome);
    if (tested) parts.push(tested);
    rows.push({
      value: d.id,
      label: parts.length ? `${d.label} (${parts.join(", ")})` : d.label,
    });
  }
  if (pinned && !devices.some((d) => d.id === pinned)) {
    rows.push({ value: pinned, label: `${pinned} (not connected)` });
  }
  return rows;
}

/** A body that is not a status (an HTML error page, an empty 204) must not crash the panel. */
export function isAudioStatus(v: unknown): v is AudioDeviceStatus {
  const s = v as AudioDeviceStatus | null;
  return Boolean(s && typeof s === "object" && s.input && s.output && Array.isArray(s.input.devices));
}
