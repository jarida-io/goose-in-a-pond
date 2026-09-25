/**
 * NDJSON logs on stderr, which the Rust side relays into `tracing` at each record's level (other
 * lines at debug). stdout is kept free for a future channel.
 */

export type Level = "error" | "warn" | "info" | "debug";

export interface LogRecord {
  level: Level;
  /** Discriminator, matching the `kind` convention on GIAP's `giap::trace` events. */
  kind: string;
  message: string;
  fields?: Record<string, unknown>;
}

/**
 * Setup codes grant fabric access, so scrub them from anything near commissioning (matter.js
 * errors echo their input). Deliberately over-eager; the Rust side redacts independently.
 */
export function redactSetupCode(text: string): string {
  return (
    text
      // QR payloads (`MT:` + base-38) first, so the digit rules can't redact only part of one.
      .replace(/MT:[A-Z0-9.$%*+\-./:]+/gi, "[redacted:setup-code]")
      // Manual codes are 11 or 21 digits, passcodes 8; exact-length runs only.
      .replace(/(?<!\d)(\d{21}|\d{11}|\d{8})(?!\d)/g, "[redacted:setup-code]")
  );
}

/** Setup-code form; each takes its own matter.js route (`commissioningOptions`). Safe to log. */
export type SetupCodeKind = "qr_payload" | "pairing_code" | "passcode" | "unknown";

export function setupCodeKind(code: string): SetupCodeKind {
  const trimmed = code.trim();
  if (/^MT:/i.test(trimmed)) return "qr_payload";
  const digits = trimmed.replace(/[\s-]/g, "");
  if (/^\d{11}$/.test(digits) || /^\d{21}$/.test(digits)) return "pairing_code";
  if (/^\d{8}$/.test(digits)) return "passcode";
  return "unknown";
}

/** Hook used by the server to also fan log records out to connected clients. */
type Sink = (record: LogRecord) => void;

let extraSink: Sink | undefined;

export function onLog(sink: Sink | undefined): void {
  extraSink = sink;
}

function emit(level: Level, kind: string, message: string, fields?: Record<string, unknown>): void {
  const record: LogRecord = { level, kind, message: redactSetupCode(message) };
  if (fields && Object.keys(fields).length > 0) {
    record.fields = fields;
  }
  // A record that can't serialise must neither throw nor emit a half-line.
  let line: string;
  try {
    line = JSON.stringify(record);
  } catch {
    line = JSON.stringify({ level, kind, message: "log record was not serialisable" });
  }
  process.stderr.write(`${line}\n`);
  extraSink?.(record);
}

export const log = {
  error: (kind: string, message: string, fields?: Record<string, unknown>) =>
    emit("error", kind, message, fields),
  warn: (kind: string, message: string, fields?: Record<string, unknown>) =>
    emit("warn", kind, message, fields),
  info: (kind: string, message: string, fields?: Record<string, unknown>) =>
    emit("info", kind, message, fields),
  debug: (kind: string, message: string, fields?: Record<string, unknown>) =>
    emit("debug", kind, message, fields),
};

/** Redacted cause chain, no stack (that goes in `fields`): matter.js wraps errors as it rethrows. */
export function describeError(error: unknown): string {
  const parts: string[] = [];
  const seen = new Set<unknown>();
  let current: unknown = error;

  // Capped at 8: a longer chain is a bug of its own.
  while (current !== undefined && current !== null && parts.length < 8) {
    if (seen.has(current)) break;
    seen.add(current);

    const message = current instanceof Error ? current.message : String(current);
    // matter.js may rethrow with the cause's message unchanged; skip repeats.
    if (message.length > 0 && !parts.includes(message)) parts.push(message);

    current = current instanceof Error ? (current as { cause?: unknown }).cause : undefined;
  }

  return redactSetupCode(parts.join(": ")) || "an error with no message";
}
