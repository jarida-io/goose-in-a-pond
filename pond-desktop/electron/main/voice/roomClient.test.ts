import { describe, it, expect, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createRoomClient, RENDERER_TOKEN_KEY, SseLines } from "./roomClient";

describe("SseLines", () => {
  it("returns each event's data however the body is cut", () => {
    const lines = new SseLines();
    expect(lines.push('data: {"event":"wake"')).toEqual([]);
    expect(lines.push(',"who":null}\n\n: keep-alive\n\ndata: {"event":"state",')).toEqual([
      '{"event":"wake","who":null}',
    ]);
    expect(lines.push('"state":"listen"}\r\n\r\n')).toEqual(['{"event":"state","state":"listen"}']);
  });
});

describe("the renderer's token", () => {
  it("is read under the key PondApiClient stores it in", () => {
    const client = readFileSync(join(__dirname, "../../../src/api/PondApiClient.ts"), "utf8");
    expect(client).toContain(`LS_SESSION = "${RENDERER_TOKEN_KEY}"`);
  });
});

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const ROOM = {
  enabled: true,
  available: true,
  unavailable_reason: null,
  state: "wait",
  session_id: "room-1",
};

describe("createRoomClient", () => {
  it("asks nothing before the app has paired", async () => {
    const fetch = vi.fn();
    const client = createRoomClient({
      baseUrl: () => "http://127.0.0.1:4000",
      bearer: async () => null,
      fetch,
    });
    expect(await client.status()).toBeNull();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("reads the status with the app's bearer, and a refusal as no room", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(200, ROOM))
      .mockResolvedValueOnce(jsonResponse(503, { reason: "room_voice_unavailable" }));
    const client = createRoomClient({
      baseUrl: () => "http://127.0.0.1:4000",
      bearer: async () => "tok",
      fetch,
    });
    expect(await client.status()).toEqual(ROOM);
    const [url, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://127.0.0.1:4000/api/v1/voice/room");
    expect((init.headers as Record<string, string>)["authorization"]).toBe("Bearer tok");
    expect(await client.status()).toBeNull();
  });

  it("switches the room with a PUT and interrupts with a POST", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(200, { ...ROOM, enabled: false, state: "off" }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const client = createRoomClient({
      baseUrl: () => "http://127.0.0.1:4000",
      bearer: async () => "tok",
      fetch,
    });
    expect((await client.setEnabled(false))?.enabled).toBe(false);
    const [, put] = fetch.mock.calls[0] as [string, RequestInit];
    expect(put.method).toBe("PUT");
    expect(put.body).toBe('{"enabled":false}');
    await client.interrupt();
    const [url, post] = fetch.mock.calls[1] as [string, RequestInit];
    expect(url).toBe("http://127.0.0.1:4000/api/v1/voice/room/interrupt");
    expect(post.method).toBe("POST");
  });

  it("streams the room's lines and ends once", async () => {
    const encoder = new TextEncoder();
    const body = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(encoder.encode('data: {"event":"state","state":"wait"}\n\n'));
        c.enqueue(encoder.encode('data: {"event":"interrupt","why":"wake_word"}\n\n'));
        c.close();
      },
    });
    const fetch = vi.fn().mockResolvedValue(new Response(body, { status: 200 }));
    const client = createRoomClient({
      baseUrl: () => "http://127.0.0.1:4000",
      bearer: async () => "tok",
      fetch,
    });
    const lines: string[] = [];
    const ended = await new Promise<string | null>((resolve) => {
      client.events((l) => lines.push(l), resolve);
    });
    expect(ended).toBeNull();
    expect(lines).toEqual([
      '{"event":"state","state":"wait"}',
      '{"event":"interrupt","why":"wake_word"}',
    ]);
    expect((fetch.mock.calls[0] as [string])[0]).toBe(
      "http://127.0.0.1:4000/api/v1/voice/room/events",
    );
  });
});
