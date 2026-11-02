// The server's room voice over HTTP: its status, its switch, its interrupt and its event stream.
// The voice session attaches to it instead of spawning a child wherever serve already listens.

/**
 * Where the renderer keeps the token it paired with (`PondApiClient`'s `LS_SESSION`). The room
 * routes want a paired device's bearer, and this app is one.
 */
export const RENDERER_TOKEN_KEY = "giap-session-token";

/** The fields of `GET /api/v1/voice/room` the shell reads. */
export interface RoomStatus {
  enabled: boolean;
  available: boolean;
  unavailable_reason: string | null;
  state: string;
  session_id: string | null;
}

export interface RoomEvents {
  close(): void;
}

export interface RoomVoiceClient {
  /** Null when the server cannot be asked: not running, no token yet, or no room voice. */
  status(): Promise<RoomStatus | null>;
  setEnabled(enabled: boolean): Promise<RoomStatus | null>;
  interrupt(): Promise<void>;
  /** Each NDJSON line the room's loop emits; `onEnd` once, when the stream stops for any reason. */
  events(onLine: (line: string) => void, onEnd: (error: string | null) => void): RoomEvents;
}

/** Splits an SSE body into its `data:` payloads, across chunks cut anywhere. */
export class SseLines {
  private buffer = "";

  push(chunk: string): string[] {
    this.buffer += chunk.replace(/\r\n/g, "\n");
    const out: string[] = [];
    let end = this.buffer.indexOf("\n\n");
    while (end !== -1) {
      const frame = this.buffer.slice(0, end);
      this.buffer = this.buffer.slice(end + 2);
      const data = frame
        .split("\n")
        .filter((l) => l.startsWith("data:"))
        .map((l) => l.slice(5).replace(/^ /, ""));
      if (data.length > 0) out.push(data.join("\n"));
      end = this.buffer.indexOf("\n\n");
    }
    return out;
  }
}

export interface RoomClientDeps {
  /** The server, e.g. `http://127.0.0.1:4000`. */
  baseUrl(): string;
  /** The bearer token the app paired with; null before it has paired. */
  bearer(): Promise<string | null>;
  fetch?: typeof fetch;
  log?: { debug(message: string): void };
}

/** How long a status or switch request may take: the switch answers before the room starts. */
const REQUEST_TIMEOUT_MS = 5_000;

export function createRoomClient(deps: RoomClientDeps): RoomVoiceClient {
  const doFetch = deps.fetch ?? fetch;
  const debug = (m: string) => deps.log?.debug(m);

  const request = async (
    method: string,
    path: string,
    body?: unknown,
    signal?: AbortSignal,
  ): Promise<Response | null> => {
    const token = await deps.bearer();
    if (token === null) return null;
    const headers: Record<string, string> = { authorization: `Bearer ${token}` };
    if (body !== undefined) headers["content-type"] = "application/json";
    return doFetch(`${deps.baseUrl()}/api/v1${path}`, {
      method,
      headers,
      ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
      signal: signal ?? AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
  };

  const statusFrom = async (res: Response | null): Promise<RoomStatus | null> => {
    if (res === null || !res.ok) {
      if (res) debug(`room voice answered ${res.status}`);
      return null;
    }
    return (await res.json()) as RoomStatus;
  };

  return {
    async status() {
      try {
        return await statusFrom(await request("GET", "/voice/room"));
      } catch (e) {
        debug(`room voice status unreachable: ${String(e)}`);
        return null;
      }
    },

    async setEnabled(enabled) {
      try {
        return await statusFrom(await request("PUT", "/voice/room", { enabled }));
      } catch (e) {
        debug(`room voice switch failed: ${String(e)}`);
        return null;
      }
    },

    async interrupt() {
      try {
        await request("POST", "/voice/room/interrupt");
      } catch (e) {
        debug(`room voice interrupt failed: ${String(e)}`);
      }
    },

    events(onLine, onEnd) {
      const abort = new AbortController();
      let ended = false;
      const end = (error: string | null) => {
        if (ended) return;
        ended = true;
        onEnd(error);
      };
      void (async () => {
        try {
          const res = await request("GET", "/voice/room/events", undefined, abort.signal);
          if (res === null || !res.ok || res.body === null) {
            end(res === null ? "no token" : `room voice events answered ${res.status}`);
            return;
          }
          const lines = new SseLines();
          const reader = res.body.getReader();
          const decoder = new TextDecoder();
          for (;;) {
            const { done, value } = await reader.read();
            if (done) break;
            for (const line of lines.push(decoder.decode(value, { stream: true }))) onLine(line);
          }
          end(null);
        } catch (e) {
          end(abort.signal.aborted ? null : String(e));
        }
      })();
      return {
        close() {
          ended = true;
          abort.abort();
        },
      };
    },
  };
}
