/** NDJSON logs to stderr, same shape as matter-server's; stdout is the MCP JSON-RPC channel. */

export type Level = "error" | "warn" | "info" | "debug";

export interface LogRecord {
  level: Level;
  /** Discriminator, matching the `kind` convention on GIAP's `giap::trace` events. */
  kind: string;
  message: string;
  fields?: Record<string, unknown>;
}

/** Scrubs OAuth credentials from log text; deliberately over-eager. */
export function redactSecrets(text: string): string {
  return (
    text
      .replace(/Bearer\s+[A-Za-z0-9._~+/=-]{8,}/gi, "Bearer [redacted]")
      .replace(
        /("(?:access|refresh|id)_token"\s*:\s*")[^"]+(")/gi,
        (_m, open: string, close: string) => `${open}[redacted]${close}`,
      )
      .replace(/("client_secret"\s*:\s*")[^"]+(")/gi,
        (_m, open: string, close: string) => `${open}[redacted]${close}`)
      // Spotify access tokens start `BQ` and turn up bare in error text.
      .replace(/\bBQ[A-Za-z0-9._-]{20,}/g, "[redacted:token]")
  );
}

function emit(level: Level, kind: string, message: string, fields?: Record<string, unknown>): void {
  const record: LogRecord = { level, kind, message: redactSecrets(message) };
  if (fields && Object.keys(fields).length > 0) {
    record.fields = JSON.parse(redactSecrets(JSON.stringify(fields))) as Record<string, unknown>;
  }
  // A record that can't serialise must neither throw nor emit a half-line.
  let line: string;
  try {
    line = JSON.stringify(record);
  } catch {
    line = JSON.stringify({ level, kind, message: "log record was not serialisable" });
  }
  process.stderr.write(`${line}\n`);
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

/** Redacted cause chain, no stack: `fetch` wraps errors, so the outer message isn't the reason. */
export function describeError(error: unknown): string {
  const parts: string[] = [];
  const seen = new Set<unknown>();
  let current: unknown = error;

  while (current !== undefined && current !== null && parts.length < 8) {
    if (seen.has(current)) break;
    seen.add(current);
    const message = current instanceof Error ? current.message : String(current);
    if (message.length > 0 && !parts.includes(message)) parts.push(message);
    current = current instanceof Error ? (current as { cause?: unknown }).cause : undefined;
  }

  return redactSecrets(parts.join(": ")) || "an error with no message";
}
