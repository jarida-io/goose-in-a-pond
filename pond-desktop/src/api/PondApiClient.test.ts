import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";
import { ApiError } from "./types";

// ── fetch mock helpers ────────────────────────────────────────────────────────

function okJson(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function errJson(status: number, message: string, code?: string): Response {
  return new Response(JSON.stringify({ error: message, ...(code ? { code } : {}) }), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

// ── helpers ───────────────────────────────────────────────────────────────────

function client() {
  return new PondApiClient("http://localhost:4000");
}

// ── health ────────────────────────────────────────────────────────────────────

describe("health()", () => {
  it("returns HealthResponse on 200", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ status: "ok", version: "1.0" }));
    const res = await client().health();
    expect(res.status).toBe("ok");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/health",
      expect.objectContaining({ method: "GET" }),
    );
  });

  it("throws ApiError on non-2xx", async () => {
    fetchMock.mockResolvedValueOnce(errJson(503, "unavailable"));
    await expect(client().health()).rejects.toBeInstanceOf(ApiError);
  });
});

// ── settings ──────────────────────────────────────────────────────────────────

describe("getSettings()", () => {
  it("GETs /api/v1/settings", async () => {
    const payload = {
      assistant_name: "Goose",
      user_name: "Jerry",
      agent_memory_inject: true,
      prompt_style: "balanced",
    };
    fetchMock.mockResolvedValueOnce(okJson(payload));
    const s = await client().getSettings();
    expect(s.assistant_name).toBe("Goose");
  });
});

describe("updateSettings()", () => {
  it("PUTs the partial patch and returns updated settings", async () => {
    const payload = {
      assistant_name: "Puck",
      user_name: "Jerry",
      agent_memory_inject: false,
      prompt_style: "concise",
    };
    fetchMock.mockResolvedValueOnce(okJson(payload));
    const s = await client().updateSettings({ assistant_name: "Puck" });
    expect(s.assistant_name).toBe("Puck");
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(init.method).toBe("PUT");
    expect(JSON.parse(init.body as string)).toMatchObject({
      assistant_name: "Puck",
    });
  });
});

// ── models ────────────────────────────────────────────────────────────────────

describe("listModels()", () => {
  /** A row as `GET /api/v1/models` sends it. */
  const e2b = {
    id: "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL",
    category: "gguf",
    name: "gemma-4-E2B-it-qat-UD-Q4_K_XL",
    title: "Gemma 4 E2B",
    description: "Gemma 4 E2B Instruct, quantisation-aware 4-bit (~2.6 GB)",
    active: true,
    downloaded: true,
    size_mb: 2498,
    quantization: "UD-Q4_K_XL",
    engine: { id: "llama_cpp", label: "llama.cpp", file_format: ".gguf", in_process: true },
    provenance: "catalogue",
    kind: "conversation",
    acquire: "download",
    recommended: { rank: "lighter", reason: "Faster replies and a smaller download" },
    companions: [{ kind: "pictures", label: "Gemma 4 E2B", size_bytes: 986_833_728, state: "available" }],
    // The server still sends these; the client no longer reads them.
    reads_images: true,
    image_support_bytes: 986_833_728,
  };

  it("flattens the grouped reply, carrying the engine, source, kind, picks and companions through", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({
        gguf: [
          e2b,
          { name: "Llama-3.2-3B", description: "(detected on disk)", downloaded: true, category: "gguf" },
        ],
        litert: [],
        llamafile: [],
        whisper: [],
        tts: [],
        ollama: [],
        embedding: [],
      }),
    );
    const models = await client().listModels();
    const pick = models.find((m) => m.name === e2b.name)!;
    expect(pick.id).toBe("gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL");
    expect(pick.title).toBe("Gemma 4 E2B");
    expect(pick.engine).toEqual(e2b.engine);
    expect(pick.provenance).toBe("catalogue");
    expect(pick.kind).toBe("conversation");
    expect(pick.acquire).toBe("download");
    expect(pick.recommended).toEqual(e2b.recommended);
    expect(pick.companions).toEqual(e2b.companions);
    expect(pick.quantization).toBe("UD-Q4_K_XL");
    expect(pick).not.toHaveProperty("reads_images");
    expect(pick).not.toHaveProperty("image_support_bytes");
  });

  it("reads a row from an older server as text only, with no engine, title or pick", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({ gguf: [{ name: "llama-3.2-3b", description: "Llama 3.2 3B", downloaded: true }] }),
    );
    const [row] = await client().listModels();
    expect(row.id).toBe("gguf/llama-3.2-3b");
    expect(row.title).toBeUndefined();
    expect(row.engine).toBeUndefined();
    expect(row.recommended).toBeUndefined();
    expect(row.companions).toEqual([]);
  });

  it("keeps the server's row id, which a download's `model_id` names", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({ tts: [{ id: "tts_kokoro/af_heart", category: "tts_kokoro", name: "af_heart" }] }),
    );
    const [voice] = await client().listModels();
    expect(voice.id).toBe("tts_kokoro/af_heart");
    expect(voice.provider).toBe("tts");
  });
});

describe("getVisionStatus()", () => {
  it("GETs /api/v1/models/vision-status", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({
        model: "gemma-4-E2B-it-Q4_K_M",
        state: { kind: "downloading", done: 412 * 1_048_576, total: 941 * 1_048_576 },
        size_bytes: 986_833_728,
        message: "Getting picture support ready: 412 MB of 941 MB. Text chat works meanwhile.",
      }),
    );
    const status = await client().getVisionStatus();
    expect(status.state.kind).toBe("downloading");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/vision-status",
      expect.anything(),
    );
  });
});

// ── devices ───────────────────────────────────────────────────────────────────

describe("listDevices()", () => {
  it("returns an array of devices", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson([{ id: "d1", name: "Lamp", is_online: true }]),
    );
    const devices = await client().listDevices();
    expect(devices).toHaveLength(1);
    expect(devices[0].name).toBe("Lamp");
  });
});

// ── player user token ────────────────────────────────────────────────────────

describe("playerUserToken()", () => {
  it("asks for the service's token, and says when it wants a renewal", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ token: "stored" }))
      .mockResolvedValueOnce(okJson({ token: "renewed" }));
    expect(await client().playerUserToken("spotify")).toEqual({ token: "stored" });
    expect(await client().playerUserToken("spotify", true)).toEqual({ token: "renewed" });
    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://localhost:4000/api/v1/player/user-token?service=spotify",
    );
    expect(fetchMock.mock.calls[1][0]).toBe(
      "http://localhost:4000/api/v1/player/user-token?service=spotify&refresh=true",
    );
  });

  it("carries the server's own words when the person has not signed in", async () => {
    fetchMock.mockResolvedValueOnce(
      errJson(400, "Spotify is not connected: sign in to Spotify in the Music extension's settings."),
    );
    await expect(client().playerUserToken("spotify")).rejects.toMatchObject({
      message: expect.stringContaining("sign in to Spotify"),
    });
  });

  it("refuses a reply that is not a token rather than handing the SDK undefined", async () => {
    for (const body of [{}, { token: "" }, { token: 7 }, null]) {
      fetchMock.mockResolvedValueOnce(okJson(body));
      await expect(client().playerUserToken("spotify")).rejects.toThrow(/no spotify token/i);
    }
  });
});

// ── memory ────────────────────────────────────────────────────────────────────

describe("listMemories()", () => {
  it("appends limit query param", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));
    await client().listMemories(10);
    expect(fetchMock.mock.calls[0][0]).toContain("limit=10");
  });
});

describe("addMemory()", () => {
  it("POSTs content and optional tags", async () => {
    const frag = {
      id: "m1",
      content: "prefer Celsius",
      tags: ["prefs"],
      created_at: "2026-01-01",
    };
    fetchMock.mockResolvedValueOnce(okJson(frag));
    const res = await client().addMemory("prefer Celsius", ["prefs"]);
    expect(res.content).toBe("prefer Celsius");
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({
      content: "prefer Celsius",
      tags: ["prefs"],
    });
  });
});

describe("deleteMemory()", () => {
  it("DELETEs /api/v1/memories/:id", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    // 204 has no body — mock request as ok
    fetchMock.mockResolvedValueOnce(okJson({}));
    fetchMock.mockReset();
    fetchMock.mockResolvedValueOnce(okJson({}));
    await client().deleteMemory("m1");
    expect(fetchMock.mock.calls[0][0]).toContain("/memories/m1");
    expect((fetchMock.mock.calls[0][1] as RequestInit).method).toBe("DELETE");
  });
});

// ── sessions ────────────────────────────────────────────────────────────────────

describe("listSessions()", () => {
  it("unwraps the { sessions } envelope and passes message_count through", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({
        sessions: [
          {
            id: "s1",
            title: "Weather plan",
            message_count: 4,
            created_at: "",
            updated_at: "",
          },
        ],
      }),
    );
    const sessions = await client().listSessions();
    expect(sessions).toHaveLength(1);
    expect(sessions[0].message_count).toBe(4);
    expect(sessions[0].title).toBe("Weather plan");
    expect(fetchMock.mock.calls[0][0]).toContain("/api/v1/sessions");
  });

  it("tolerates a bare array response", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson([{ id: "s2", created_at: "", updated_at: "" }]),
    );
    const sessions = await client().listSessions();
    expect(sessions[0].id).toBe("s2");
  });
});

describe("renameSession()", () => {
  it("PATCHes /sessions/:id with the new title", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({ session_id: "s1", title: "Renamed" }),
    );
    await client().renameSession("s1", "Renamed");
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toContain("/api/v1/sessions/s1");
    expect(init.method).toBe("PATCH");
    expect(JSON.parse(init.body as string)).toEqual({ title: "Renamed" });
  });
});

describe("deleteSession()", () => {
  it("DELETEs /sessions/:id", async () => {
    fetchMock.mockResolvedValueOnce(okJson({}));
    await client().deleteSession("s1");
    expect(fetchMock.mock.calls[0][0]).toContain("/api/v1/sessions/s1");
    expect((fetchMock.mock.calls[0][1] as RequestInit).method).toBe("DELETE");
  });
});

// A real pond-server turn with `persist_thinking` on (ids shortened), through the real mapping:
// Chat.test.tsx mocks `getSessionMessages` and so skips it.
describe("getSessionMessages()", () => {
  const userRow = {
    id: "m-user",
    session_id: "sess-1",
    role: "user",
    content: "what is in this picture?",
    created_at: "2026-09-24T07:14:51+00:00",
    liked: null,
    images: [
      {
        id: "att-1",
        mime_type: "image/png",
        byte_size: 70,
        url: "/api/v1/sessions/sess-1/attachments/att-1",
      },
    ],
  };
  const replyRow = {
    id: "m-reply",
    session_id: "sess-1",
    role: "assistant",
    content: "A single white pixel.",
    created_at: "2026-09-24T07:15:42+00:00",
    liked: null,
    thinking: ["It is a 1x1 PNG.", "So: one pixel, white."],
  };

  it("keeps the persisted reasoning on the assistant row", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ messages: [userRow, replyRow] }));
    const msgs = await client().getSessionMessages("sess-1");
    // Both passages, in order; a presence check would pass a mapping that kept one.
    expect(msgs[1].thinking).toEqual([
      "It is a 1x1 PNG.",
      "So: one pixel, white.",
    ]);
  });

  it("drops nothing else the server sent", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ messages: [userRow, replyRow] }));
    const msgs = await client().getSessionMessages("sess-1");
    // Against the wire rows, so any field the mapping drops fails here.
    expect(msgs).toEqual([userRow, replyRow]);
  });

  it("keeps 'nothing was recorded' apart from 'recorded, and empty'", async () => {
    const unrecorded = { ...replyRow, id: "m-unrecorded", thinking: undefined };
    const empty = { ...replyRow, id: "m-empty", thinking: [] };
    fetchMock.mockResolvedValueOnce(okJson({ messages: [unrecorded, empty] }));
    const msgs = await client().getSessionMessages("sess-1");
    expect(msgs[0].thinking).toBeUndefined();
    expect(msgs[1].thinking).toEqual([]);
  });
});

describe("getSessionAttachment()", () => {
  const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
  const png = () =>
    new Response(PNG, {
      status: 200,
      headers: { "Content-Type": "image/png" },
    });

  afterEach(() => localStorage.clear());

  it("GETs the bytes with the bearer token an <img src> cannot send", async () => {
    fetchMock.mockResolvedValueOnce(png());
    const blob = await new PondApiClient(
      "http://localhost:4000",
      "tok",
    ).getSessionAttachment("sess 1", "att/1");

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe(
      "http://localhost:4000/api/v1/sessions/sess%201/attachments/att%2F1",
    );
    expect(init.method).toBe("GET");
    expect((init.headers as Record<string, string>)["Authorization"]).toBe(
      "Bearer tok",
    );
    expect(new Uint8Array(await blob.arrayBuffer())).toEqual(PNG);
  });

  it("re-pairs once on a 401 and retries, like every other call", async () => {
    const future = new Date(Date.now() + 3_600_000).toISOString();
    localStorage.setItem("giap-session-token", "stale");
    localStorage.setItem("giap-refresh-token", "r1");
    localStorage.setItem("giap-token-expires-at", future);
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/refresh")) {
        return okJson({
          accepted: true,
          session_token: "fresh",
          refresh_token: "r2",
          expires_at: future,
        });
      }
      const auth = (init?.headers as Record<string, string> | undefined)?.[
        "Authorization"
      ];
      return auth === "Bearer fresh"
        ? png()
        : errJson(401, "Invalid or expired token");
    });

    const blob = await client().getSessionAttachment("s", "a");
    expect(new Uint8Array(await blob.arrayBuffer())).toEqual(PNG);
  });

  it("throws ApiError when the bytes are gone", async () => {
    fetchMock.mockResolvedValueOnce(
      errJson(404, "Attachment bytes are no longer available"),
    );
    const err = await client()
      .getSessionAttachment("s", "a")
      .catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(404);
  });
});

describe("compactSession()", () => {
  it("POSTs /sessions/:id/compact", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({
        session_id: "s1",
        status: "compacted",
        reason: null,
        outcome: "refreshed",
        context: {},
      }),
    );
    const res = await client().compactSession("s1");
    expect(fetchMock.mock.calls[0][0]).toContain("/api/v1/sessions/s1/compact");
    expect((fetchMock.mock.calls[0][1] as RequestInit).method).toBe("POST");
    expect(res.status).toBe("compacted");
  });

  it("resolves a refusal rather than throwing", async () => {
    // The endpoint answers 200 with status/reason for anything short of a server fault.
    fetchMock.mockResolvedValueOnce(
      okJson({
        session_id: "s1",
        status: "skipped",
        reason: "cooling_down",
        outcome: null,
        context: {},
      }),
    );
    const res = await client().compactSession("s1");
    expect(res.reason).toBe("cooling_down");
  });
});

// ── skills ────────────────────────────────────────────────────────────────────

describe("listSkills()", () => {
  it("adds ?all=true when requested", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));
    await client().listSkills(true);
    expect(fetchMock.mock.calls[0][0]).toContain("all=true");
  });

  it("omits query param when all=false", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));
    await client().listSkills(false);
    expect(fetchMock.mock.calls[0][0]).not.toContain("all=true");
  });
});

// ── prompts ───────────────────────────────────────────────────────────────────

describe("updatePrompt()", () => {
  it("PUTs content to the named prompt endpoint", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({ name: "balanced", content: "new content", is_system: true }),
    );
    const res = await client().updatePrompt("balanced", "new content");
    expect(res.content).toBe("new content");
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toContain("/prompts/balanced");
    expect(init.method).toBe("PUT");
  });
});

// ── authorization header ──────────────────────────────────────────────────────

describe("setToken()", () => {
  it("attaches Bearer token to subsequent requests", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ status: "ok" }));
    const c = client();
    c.setToken("my-token");
    await c.health();
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect((init.headers as Record<string, string>)["Authorization"]).toBe(
      "Bearer my-token",
    );
  });

  it("omits Authorization header when token is null", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ status: "ok" }));
    const c = client();
    c.setToken(null);
    await c.health();
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(
      (init.headers as Record<string, string>)["Authorization"],
    ).toBeUndefined();
  });
});

// ── ApiError ──────────────────────────────────────────────────────────────────

describe("ApiError", () => {
  it("captures status and message", async () => {
    fetchMock.mockResolvedValueOnce(errJson(404, "not found"));
    try {
      await client().getPrompt("missing");
      expect.fail("should have thrown");
    } catch (e) {
      expect(e).toBeInstanceOf(ApiError);
      expect((e as ApiError).status).toBe(404);
      expect((e as ApiError).message).toBe("not found");
    }
  });

  it("falls back to statusText when response body is not JSON", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response("Bad Gateway", { status: 502, statusText: "Bad Gateway" }),
    );
    await expect(client().health()).rejects.toMatchObject({ status: 502 });
  });

  it("captures a structured {error, code} body's code, for a caller to branch on", async () => {
    fetchMock.mockResolvedValueOnce(errJson(409, "Picture support is not ready yet.", "vision_not_ready"));
    try {
      await client().getPrompt("missing");
      expect.fail("should have thrown");
    } catch (e) {
      expect(e).toBeInstanceOf(ApiError);
      expect((e as ApiError).code).toBe("vision_not_ready");
      expect((e as ApiError).body).toMatchObject({ code: "vision_not_ready" });
    }
  });

  it("leaves code undefined for a plain {error} body", async () => {
    fetchMock.mockResolvedValueOnce(errJson(500, "internal error"));
    try {
      await client().getPrompt("missing");
      expect.fail("should have thrown");
    } catch (e) {
      expect((e as ApiError).code).toBeUndefined();
    }
  });
});

// ── chatStream ────────────────────────────────────────────────────────────────

describe("chatStream()", () => {
  function makeStream(lines: string[]): ReadableStream<Uint8Array> {
    const encoder = new TextEncoder();
    return new ReadableStream({
      start(controller) {
        for (const line of lines) {
          controller.enqueue(encoder.encode(line + "\n"));
        }
        controller.close();
      },
    });
  }

  it("yields text events from SSE stream", async () => {
    const sseLines = [
      'data: {"type":"text","content":"Hello"}',
      'data: {"type":"text","content":" world"}',
      "data: [DONE]",
    ];
    fetchMock.mockResolvedValueOnce(
      new Response(makeStream(sseLines), { status: 200 }),
    );

    const events = [];
    for await (const ev of client().chatStream("hi")) {
      events.push(ev);
    }

    expect(events.filter((e) => e.type === "text")).toHaveLength(2);
    expect(events.find((e) => e.type === "done")).toBeDefined();
  });

  it("throws ApiError when chat endpoint returns a non-auth error", async () => {
    // A 401 re-authenticates instead; see the re-authentication suite.
    fetchMock.mockResolvedValueOnce(errJson(500, "internal error"));
    const gen = client().chatStream("hi");
    await expect(gen.next()).rejects.toBeInstanceOf(ApiError);
  });

  it("carries a refused turn's structured code — e.g. a 409 before any frame", async () => {
    fetchMock.mockResolvedValueOnce(errJson(409, "Picture support is not ready yet.", "vision_not_ready"));
    const gen = client().chatStream("look at this", undefined, undefined, undefined, [
      { data: "AAA", mime_type: "image/png" },
    ]);
    try {
      await gen.next();
      expect.fail("should have thrown");
    } catch (e) {
      expect(e).toBeInstanceOf(ApiError);
      expect((e as ApiError).status).toBe(409);
      expect((e as ApiError).code).toBe("vision_not_ready");
    }
  });

  /** An open body holds one of four server SSE permits; `releaseLock()` alone does not close it. */
  it("cancels the body when the consumer walks away mid-stream", async () => {
    const encoder = new TextEncoder();
    let cancelled: unknown = "not cancelled";
    // Never closes on its own: only a cancel can end this one.
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          encoder.encode('data: {"type":"text","content":"a"}\n'),
        );
      },
      cancel(reason) {
        cancelled = reason ?? null;
      },
    });
    fetchMock.mockResolvedValueOnce(new Response(body, { status: 200 }));

    for await (const ev of client().chatStream("hi")) {
      if (ev.type === "text") break; // exactly what a session switch does
    }

    expect(cancelled).not.toBe("not cancelled");
  });

  it("skips malformed SSE lines without throwing", async () => {
    const sseLines = [
      "data: not-valid-json",
      'data: {"type":"text","content":"ok"}',
    ];
    fetchMock.mockResolvedValueOnce(
      new Response(makeStream(sseLines), { status: 200 }),
    );

    const events = [];
    for await (const ev of client().chatStream("hi")) {
      events.push(ev);
    }
    expect(events.filter((e) => e.type === "text")).toHaveLength(1);
  });
});

// ── Schedule actions ──────────────────────────────────────────────────────────

describe("pauseSchedule()", () => {
  it("POSTs to /schedules/:id/pause", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await client().pauseSchedule("sched-1");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/schedules/sched-1/pause",
      expect.objectContaining({ method: "POST" }),
    );
  });
});

describe("resumeSchedule()", () => {
  it("POSTs to /schedules/:id/resume", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await client().resumeSchedule("sched-1");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/schedules/sched-1/resume",
      expect.objectContaining({ method: "POST" }),
    );
  });
});

describe("runScheduleNow()", () => {
  it("POSTs to /schedules/:id/run-now", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await client().runScheduleNow("sched-1");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/schedules/sched-1/run-now",
      expect.objectContaining({ method: "POST" }),
    );
  });
});

// ── GGUF model search & download ──────────────────────────────────────────────

describe("searchGgufModels()", () => {
  it("GETs /models/search/gguf?q=<query>", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ models: [] }));
    await client().searchGgufModels("gemma");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/search/gguf?q=gemma",
      expect.objectContaining({ method: "GET" }),
    );
  });

  it("encodes special characters in query", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ models: [] }));
    await client().searchGgufModels("gemma 4b");
    const url = fetchMock.mock.calls[0][0] as string;
    expect(url).toContain("q=gemma%204b");
  });

  it("returns model list", async () => {
    const models = [
      {
        id: "unsloth/gemma-4-E2B-it-GGUF",
        downloads: 5000,
        likes: 12,
        tags: ["gguf"],
        url: "https://huggingface.co/unsloth/gemma-4-E2B-it-GGUF",
      },
    ];
    fetchMock.mockResolvedValueOnce(okJson({ models }));
    const res = await client().searchGgufModels("gemma");
    expect(res.models).toHaveLength(1);
    expect(res.models[0].id).toBe("unsloth/gemma-4-E2B-it-GGUF");
  });
});

describe("listHfModelFiles()", () => {
  it("GETs /models/search/gguf/files?repo=<repo>", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ files: [] }));
    await client().listHfModelFiles("unsloth/gemma-4-E2B-it-GGUF");
    const url = fetchMock.mock.calls[0][0] as string;
    expect(url).toContain("/models/search/gguf/files");
    expect(url).toContain("repo=");
  });

  it("returns file list", async () => {
    const files = [
      {
        filename: "model.Q4_K_M.gguf",
        size_mb: 1400,
        url: "https://huggingface.co/...",
      },
    ];
    fetchMock.mockResolvedValueOnce(okJson({ files }));
    const res = await client().listHfModelFiles("unsloth/gemma-4-E2B-it-GGUF");
    expect(res.files).toHaveLength(1);
    expect(res.files[0].filename).toBe("model.Q4_K_M.gguf");
  });
});

describe("downloadModelFromUrl()", () => {
  it("POSTs to /models/download/url with correct body", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ status: "download_started" }));
    const res = await client().downloadModelFromUrl(
      "https://hf.co/file.gguf",
      "gguf",
      "file.gguf",
    );
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/download/url",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          url: "https://hf.co/file.gguf",
          category: "gguf",
          filename: "file.gguf",
        }),
      }),
    );
    expect(res.status).toBe("download_started");
  });

  it("says when the household left the add-on out, and says nothing when they did not", async () => {
    fetchMock.mockImplementation(() => Promise.resolve(okJson({ status: "downloading" })));
    await client().downloadModelFromUrl("https://hf.co/f.gguf", "gguf", "f.gguf", { pictures: false });
    expect(JSON.parse(fetchMock.mock.calls[0][1].body as string)).toEqual({
      url: "https://hf.co/f.gguf", category: "gguf", filename: "f.gguf", pictures: false,
    });
    fetchMock.mockClear();
    await client().downloadModelFromUrl("https://hf.co/f.gguf", "gguf", "f.gguf", {});
    expect(JSON.parse(fetchMock.mock.calls[0][1].body as string)).not.toHaveProperty("pictures");
  });
});

describe("downloadModel()", () => {
  const started = {
    status: "download_started",
    model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL",
    parts: [
      { part: "model", filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", size_bytes: 4_215_695_776 },
      { part: "pictures", filename: "mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf", size_bytes: 991_552_320 },
    ],
    pictures: "included",
    message: "Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)",
  };

  it("asks with no body unless told, so the add-on comes by default", async () => {
    fetchMock.mockResolvedValueOnce(okJson(started));
    const res = await client().downloadModel("gguf", "gemma-4-E4B-it-qat-UD-Q4_K_XL");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL/download",
      expect.objectContaining({ method: "POST", body: undefined }),
    );
    expect(res.message).toBe("Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)");
    expect(res.parts?.map((p) => p.part)).toEqual(["model", "pictures"]);
  });

  it("leaves the add-on out when told", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ ...started, pictures: "left_out" }));
    await client().downloadModel("gguf", "gemma-4-E4B-it-qat-UD-Q4_K_XL", { pictures: false });
    expect(JSON.parse(fetchMock.mock.calls[0][1].body as string)).toEqual({ pictures: false });
  });

  it("encodes the name, which LiteRT-LM files carry an extension in", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ status: "download_started" }));
    await client().downloadModel("litert", "gemma-4-E4B-it.litertlm");
    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://localhost:4000/api/v1/models/litert/gemma-4-E4B-it.litertlm/download",
    );
  });
});

describe("controlModelDownload()", () => {
  it("names the model, not a file, so the pond moves every part of it", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({
        status: "cancelled",
        files: [
          { filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", status: "cancelled" },
          { filename: "mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf", status: "cancelled" },
        ],
      }),
    );
    const res = await client().controlModelDownload("gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL", "cancel");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/download/control",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL", action: "cancel" }),
      }),
    );
    expect(res.files).toHaveLength(2);
  });

  it("carries the pond's refusal when nothing of the model can be told to do that", async () => {
    fetchMock.mockResolvedValueOnce(errJson(404, "gguf/x has no download that can be told to pause"));
    await expect(client().controlModelDownload("gguf/x", "pause")).rejects.toMatchObject({ status: 404 });
  });
});

describe("addPictures()", () => {
  it("POSTs to the model's own companions route, and returns what it will fetch", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({ status: "download_started", message: "Downloading picture support for Gemma 4 E2B (941 MB)" }),
    );
    const res = await client().addPictures("gguf", "gemma-4-E2B-it-qat-UD-Q4_K_XL");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL/companions/pictures",
      expect.objectContaining({ method: "POST" }),
    );
    expect(res.message).toMatch(/941 MB/);
  });

  it("carries the server's refusal for a model with no add-on", async () => {
    fetchMock.mockResolvedValueOnce(errJson(409, "Gemma 4 E4B reads text only", "text_only"));
    await expect(client().addPictures("litert", "gemma-4-E4B-it.litertlm")).rejects.toMatchObject({
      status: 409,
      code: "text_only",
    });
  });
});

describe("getActiveRoles()", () => {
  const roles = (over: Record<string, unknown>) =>
    okJson({
      chat: { provider: "local", model: "gemma-4-E2B-it-qat-UD-Q4_K_XL", model_id: "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL" },
      tool: { model: null },
      asr: { model_id: null },
      tts: { model_id: null },
      embedding: { model_id: null, model: "", provider: "fastembed" },
      ...over,
    });

  it("reads each slot's model, from its own fields or from its assignment", async () => {
    fetchMock.mockResolvedValueOnce(
      roles({ asr: { model_id: "whisper/base.en" }, tts: { model_id: "tts_kokoro/af_heart" } }),
    );
    const r = await client().getActiveRoles();
    expect(r.chat).toEqual({ provider: "local", model: "gemma-4-E2B-it-qat-UD-Q4_K_XL" });
    expect(r.asr).toEqual({ provider: "whisper", model: "base.en" });
    expect(r.tts).toEqual({ provider: "tts_kokoro", model: "af_heart" });
  });

  it("says nothing is chosen for conversation when the setting is empty, whatever an old assignment says", async () => {
    fetchMock.mockResolvedValueOnce(
      roles({ chat: { provider: "", model: "", model_id: "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL" } }),
    );
    expect((await client().getActiveRoles()).chat).toBeNull();
  });

  it("keeps memory's provider when it names no model, so the built-in one can be said", async () => {
    fetchMock.mockResolvedValueOnce(roles({}));
    expect((await client().getActiveRoles()).embedding).toEqual({ provider: "fastembed", model: "" });
  });
});

describe("getMemoryStatus()", () => {
  it("carries what a switch would free", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({ total_mb: 7620, available_for_llm_mb: 1000, loaded_model: null, reclaimable_mb: 2600 }),
    );
    expect((await client().getMemoryStatus()).reclaimable_mb).toBe(2600);
  });
});

describe("getDownloadProgress() entries", () => {
  it("name the model and the part they belong to, and say why one failed", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({
        downloads: [
          {
            filename: "mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf",
            category: "mmproj",
            downloaded_bytes: 0,
            total_bytes: 991_552_320,
            status: "error",
            model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL",
            part: "pictures",
            error: "HTTP 503",
          },
        ],
      }),
    );
    const [d] = (await client().getDownloadProgress()).downloads;
    expect(d.model_id).toBe("gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL");
    expect(d.part).toBe("pictures");
    expect(d.error).toBe("HTTP 503");
  });
});

describe("getDownloadProgress()", () => {
  it("GETs /models/download/progress", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ downloads: [] }));
    const res = await client().getDownloadProgress();
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/download/progress",
      expect.objectContaining({ method: "GET" }),
    );
    expect(res.downloads).toEqual([]);
  });

  it("returns in-progress downloads", async () => {
    const downloads = [
      {
        filename: "model.gguf",
        category: "gguf",
        downloaded_bytes: 42,
        total_bytes: 100,
        status: "downloading",
      },
    ];
    fetchMock.mockResolvedValueOnce(okJson({ downloads }));
    const res = await client().getDownloadProgress();
    expect(res.downloads[0].downloaded_bytes).toBe(42);
    expect(res.downloads[0].total_bytes).toBe(100);
  });
});

// ── deleteModel ───────────────────────────────────────────────────────────────

describe("deleteModel()", () => {
  it("DELETEs /models/{category}/{name}", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await client().deleteModel("gguf", "gemma-2b");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/gguf/gemma-2b",
      expect.objectContaining({ method: "DELETE" }),
    );
  });

  it("throws ApiError 409 when model is active", async () => {
    fetchMock.mockResolvedValueOnce(
      errJson(409, "model is active in role chat"),
    );
    await expect(
      client().deleteModel("gguf", "active-model"),
    ).rejects.toSatisfy(
      (e: unknown) => e instanceof ApiError && (e as ApiError).status === 409,
    );
  });
});

// ── listOllamaModels ──────────────────────────────────────────────────────────

describe("listOllamaModels()", () => {
  it("GETs /models/ollama", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ models: [] }));
    const res = await client().listOllamaModels();
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/ollama",
      expect.objectContaining({ method: "GET" }),
    );
    expect(res.models).toEqual([]);
  });

  it("returns models with name field", async () => {
    const models = [{ name: "llama3.2:3b", size: 2_000_000_000 }];
    fetchMock.mockResolvedValueOnce(okJson({ models }));
    const res = await client().listOllamaModels();
    expect(res.models[0].name).toBe("llama3.2:3b");
  });

  it("returns error string when Ollama not running", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({ models: [], error: "Ollama not running or not installed" }),
    );
    const res = await client().listOllamaModels();
    expect(res.error).toContain("Ollama");
  });
});

// ── pullOllamaModel ───────────────────────────────────────────────────────────

describe("pullOllamaModel()", () => {
  it("POSTs to /models/ollama/pull with model name in body", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ status: "pulling" }));
    const res = await client().pullOllamaModel("llama3.2:3b");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/ollama/pull",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ model: "llama3.2:3b" }),
      }),
    );
    expect(res.status).toBe("pulling");
  });
});

// ── wake-word calibration ─────────────────────────────────────────────────────

describe("calibrateWakeWord()", () => {
  it("POSTs multipart form to /voice/calibrate", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({
        transcript: "hey goose",
        normalized: "hey goose",
        all_variants: ["hey goose"],
        sample_count: 1,
        target_count: 5,
        complete: false,
      }),
    );
    const wav = new Uint8Array([0, 1, 2, 3]).buffer;
    const res = await client().calibrateWakeWord(wav);
    expect(res.sample_count).toBe(1);
    expect(res.all_variants).toEqual(["hey goose"]);
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/voice/calibrate",
      expect.objectContaining({ method: "POST" }),
    );
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(init.body).toBeInstanceOf(FormData);
  });

  it("throws ApiError on 422 (no speech detected)", async () => {
    fetchMock.mockResolvedValueOnce(errJson(422, "No speech detected"));
    const wav = new Uint8Array([0]).buffer;
    await expect(client().calibrateWakeWord(wav)).rejects.toSatisfy(
      (e: unknown) => e instanceof ApiError && (e as ApiError).status === 422,
    );
  });

  it("attaches Bearer token when set", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({
        transcript: "goose",
        normalized: "goose",
        all_variants: ["goose"],
        sample_count: 1,
        target_count: 5,
        complete: false,
      }),
    );
    const c = client();
    c.setToken("tok-123");
    await c.calibrateWakeWord(new Uint8Array([0]).buffer);
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect((init.headers as Record<string, string>)["Authorization"]).toBe(
      "Bearer tok-123",
    );
  });
});

describe("resetWakeWordCalibration()", () => {
  it("DELETEs /voice/calibrate", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ cleared: true }));
    await client().resetWakeWordCalibration();
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/voice/calibrate",
      expect.objectContaining({ method: "DELETE" }),
    );
  });
});

// ── searchLlamafileModels ─────────────────────────────────────────────────────

describe("searchLlamafileModels()", () => {
  it("GETs /models/search/llamafile without query", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ models: [] }));
    await client().searchLlamafileModels();
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/models/search/llamafile",
      expect.objectContaining({ method: "GET" }),
    );
  });

  it("appends ?q= when query provided", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ models: [] }));
    await client().searchLlamafileModels("gemma");
    const url = fetchMock.mock.calls[0][0] as string;
    expect(url).toContain("?q=gemma");
  });

  it("returns release list", async () => {
    const models = [
      {
        name: "gemma-2b-it.llamafile",
        size_mb: 1400,
        download_url: "https://github.com/...",
        tag: "0.9.1",
      },
    ];
    fetchMock.mockResolvedValueOnce(okJson({ models }));
    const res = await client().searchLlamafileModels("gemma");
    expect(res.models[0].name).toBe("gemma-2b-it.llamafile");
    expect(res.models[0].tag).toBe("0.9.1");
  });
});

describe("re-authentication after a rejected token", () => {
  const future = new Date(Date.now() + 3_600_000).toISOString();

  function seedStaleSession() {
    // Unexpired but server-rejected token (restart/rotation); the refresh token avoids re-pairing.
    localStorage.setItem("giap-session-token", "stale");
    localStorage.setItem("giap-refresh-token", "r1");
    localStorage.setItem("giap-token-expires-at", future);
  }

  afterEach(() => localStorage.clear());

  it("re-pairs once for a burst of concurrent 401s and reuses the token", async () => {
    seedStaleSession();
    let refreshCalls = 0;

    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/refresh")) {
        refreshCalls += 1;
        return okJson({
          accepted: true,
          session_token: "fresh",
          refresh_token: "r2",
          expires_at: future,
        });
      }
      const auth = (init?.headers as Record<string, string> | undefined)?.[
        "Authorization"
      ];
      return auth === "Bearer stale"
        ? errJson(401, "Invalid or expired token")
        : okJson([{ id: "d1", name: "Lamp", is_online: true }]);
    });

    const api = client();
    await Promise.all([
      api.listDevices(),
      api.listDevices(),
      api.listDevices(),
      api.listDevices(),
      api.listDevices(),
    ]);

    expect(refreshCalls).toBe(1);
  });

  it("re-authenticates the chat stream on a 401 instead of erroring", async () => {
    seedStaleSession();
    let refreshCalls = 0;
    const emptySse = () =>
      new Response(new ReadableStream({ start: (c) => c.close() }), {
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
      });

    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/refresh")) {
        refreshCalls += 1;
        return okJson({
          accepted: true,
          session_token: "fresh",
          refresh_token: "r2",
          expires_at: future,
        });
      }
      const auth = (init?.headers as Record<string, string> | undefined)?.[
        "Authorization"
      ];
      return auth === "Bearer stale"
        ? errJson(401, "Invalid or expired token")
        : emptySse();
    });

    const api = client();
    // eslint-disable-next-line @typescript-eslint/no-unused-vars -- draining the stream is the point; the events are not read
    for await (const _ of api.chatStream("hi", undefined, "stale")) {
      /* drain */
    }
    expect(refreshCalls).toBe(1);
  });

  it("surfaces the retry result once re-authenticated", async () => {
    seedStaleSession();
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/refresh")) {
        return okJson({
          accepted: true,
          session_token: "fresh",
          refresh_token: "r2",
          expires_at: future,
        });
      }
      const auth = (init?.headers as Record<string, string> | undefined)?.[
        "Authorization"
      ];
      return auth === "Bearer stale"
        ? errJson(401, "Invalid or expired token")
        : okJson([{ id: "d1", name: "Lamp", is_online: true }]);
    });

    const devices = await client().listDevices();
    expect(devices[0].name).toBe("Lamp");
  });
});

describe("per-instance client id", () => {
  const future = new Date(Date.now() + 3_600_000).toISOString();

  // Drive the full pair() flow, capturing the client_id sent to /handshake/init.
  function mockPairing(): () => string | undefined {
    let sentClientId: string | undefined;
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/pairing-code"))
        return okJson({ code: "123456", expires_at: future });
      if (url.includes("/handshake/init")) {
        sentClientId = JSON.parse(String(init?.body)).client_id;
        return okJson({ challenge: "ch", challenge_id: "cid" });
      }
      if (url.includes("/handshake/verify")) {
        return okJson({
          accepted: true,
          session_token: "t",
          refresh_token: "r",
          expires_at: future,
        });
      }
      return okJson({});
    });
    return () => sentClientId;
  }

  afterEach(() => localStorage.clear());

  it("mints a persisted pond-desktop-<id>, distinct from the shared default", async () => {
    localStorage.clear();
    const getId = mockPairing();
    await client().pair();

    const id = getId();
    expect(id).toMatch(/^pond-desktop-.+/);
    expect(id).not.toBe("pond-desktop"); // a shared id causes revocation churn
    expect(localStorage.getItem("giap-client-id")).toBe(id);
  });

  it("reuses the same id across instances (a restart pairs as the same client)", async () => {
    localStorage.clear();
    const first = mockPairing();
    await client().pair();
    const id1 = first();

    const second = mockPairing();
    await client().pair();
    expect(second()).toBe(id1);
  });
});

// Pairing registers the desktop as a device row; `is_online` ages out after five minutes of no beat.
describe("heartbeatSelf()", () => {
  const future = new Date(Date.now() + 3_600_000).toISOString();

  afterEach(() => localStorage.clear());

  it("beats the same device row that pairing registered", async () => {
    localStorage.clear();
    let sentClientId: string | undefined;
    const posted: string[] = [];
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.includes("/handshake/pairing-code"))
        return okJson({ code: "123456", expires_at: future });
      if (url.includes("/handshake/init")) {
        sentClientId = JSON.parse(String(init?.body)).client_id;
        return okJson({ challenge: "ch", challenge_id: "cid" });
      }
      if (url.includes("/handshake/verify"))
        return okJson({
          accepted: true,
          session_token: "t",
          refresh_token: "r",
          expires_at: future,
        });
      if (url.includes("/heartbeat")) posted.push(url);
      return okJson({});
    });

    const api = client();
    await api.pair();
    await api.heartbeatSelf();

    // A beat against any other row refreshes nothing.
    expect(sentClientId).toBeTruthy();
    expect(posted).toEqual([
      `http://localhost:4000/api/v1/devices/${sentClientId}/heartbeat`,
    ]);
  });
});

// One call redirects the singleton, so the shell can fix a fallback port without a reload.
describe("setBase", () => {
  it("follows the shell's server URL for requests it has not sent yet", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ status: "ok" }),
      text: async () => "{}",
    });
    vi.stubGlobal("fetch", fetchMock);

    const client = new PondApiClient("http://127.0.0.1:4000");
    client.setBase("http://127.0.0.1:4001");
    await client.health().catch(() => undefined);

    expect(String(fetchMock.mock.calls[0]?.[0])).toContain("127.0.0.1:4001");
    vi.unstubAllGlobals();
  });

  it("trims a trailing slash, the way the constructor does", async () => {
    const client = new PondApiClient("http://127.0.0.1:4000");
    client.setBase("http://127.0.0.1:4001/");
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ status: "ok" }),
      text: async () => "{}",
    });
    vi.stubGlobal("fetch", fetchMock);
    await client.health().catch(() => undefined);
    expect(String(fetchMock.mock.calls[0]?.[0])).not.toContain("//api");
    vi.unstubAllGlobals();
  });
});

describe("the host credential", () => {
  const credential = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJ-_01234";
  const sent = (path: string) =>
    fetchMock.mock.calls
      .filter(([url]) => String(url).includes(path))
      .map(([, init]) => (init?.headers as Record<string, string>)["X-Pond-Host-Credential"]);

  beforeEach(() => sessionStorage.setItem("giap-host-credential", credential));
  afterEach(() => sessionStorage.clear());

  it("goes only to the host-only routes", async () => {
    fetchMock.mockImplementation(async () => okJson({ code: "123456", state: "Stopped" }));
    const api = client();
    await api.getPairingCode();
    await api.remoteStatus();
    await api.health();
    expect(sent("/handshake/pairing-code")).toEqual([credential]);
    expect(sent("/remote-access")).toEqual([credential]);
    expect(sent("/health")).toEqual([undefined]);
  });

  it("goes with an onboarding reset and with speech synthesis", async () => {
    fetchMock.mockImplementation(async () => okJson({ onboarded: false, current_step: "Welcome" }));
    const api = client();
    await api.resetOnboarding();
    await api.synthesizeSpeech("hello");
    expect(sent("/onboard/reset")).toEqual([credential]);
    expect(sent("/api/v1/tts")).toEqual([credential]);
  });

  it("surfaces the refusal by name, so the UI can explain it", async () => {
    sessionStorage.clear();
    fetchMock.mockResolvedValue(errJson(403, "host_credential_required"));
    await expect(client().getPairingCode()).rejects.toMatchObject({
      status: 403,
      message: "host_credential_required",
    });
    expect(sent("/handshake/pairing-code")).toEqual([undefined]);
  });
});

// ── web search ────────────────────────────────────────────────────────────────

describe("web search", () => {
  const KEY = "BSA-test-key-0123456789";

  it("getWebSearch() GETs the state and returns it as the pond sent it", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ provider: "brave", configured: true, status: "quota" }));
    const res = await client().getWebSearch();
    expect(res).toEqual({ provider: "brave", configured: true, status: "quota" });
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/web-search",
      expect.objectContaining({ method: "GET" }),
    );
  });

  it("getWebSearch() hands back undefined, for the caller to guard, when an older pond sends a page", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response("<!doctype html>", { status: 200, headers: { "Content-Type": "text/html" } }),
    );
    expect(await client().getWebSearch()).toBeUndefined();
  });

  it("setWebSearchKey() PUTs the key in the body, never in the address", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ configured: true, status: "unverified" }));
    const res = await client().setWebSearchKey(KEY);
    expect(res).toEqual({ configured: true, status: "unverified" });

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://localhost:4000/api/v1/web-search/key");
    expect(url).not.toContain(KEY);
    expect(init.method).toBe("PUT");
    expect(JSON.parse(init.body as string)).toEqual({ key: KEY });
  });

  it("setWebSearchKey() rejects with the 422 and Brave's sentence, the key never echoed", async () => {
    fetchMock.mockResolvedValueOnce(errJson(422, "Brave did not accept this key."));
    const err = await client().setWebSearchKey(KEY).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(422);
    expect((err as ApiError).message).toBe("Brave did not accept this key.");
    expect((err as ApiError).body).toEqual({ error: "Brave did not accept this key." });
    expect(JSON.stringify(err)).not.toContain(KEY);
  });

  it("deleteWebSearchKey() DELETEs and takes the 204", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await expect(client().deleteWebSearchKey()).resolves.toBeUndefined();
    expect(fetchMock).toHaveBeenCalledWith(
      "http://localhost:4000/api/v1/web-search/key",
      expect.objectContaining({ method: "DELETE" }),
    );
  });
});
