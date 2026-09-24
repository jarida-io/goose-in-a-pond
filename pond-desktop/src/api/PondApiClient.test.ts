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

function errJson(status: number, message: string): Response {
  return new Response(JSON.stringify({ error: message }), {
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

// PAI-5 P6. What a live pond-server returned for one turn recorded with
// `persist_thinking` on -- captured over real HTTP on 2026-09-24, ids shortened.
// It goes through the REAL mapping from a raw fetch body. The replay tests in
// `Chat.test.tsx` mock `getSessionMessages` itself, which skips the one piece
// of code that dropped `thinking`; that is how every reloaded conversation lost
// its reasoning while those tests stayed green.
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
    // Both passages, in order: a mapping that kept only the first would pass
    // a check on presence.
    expect(msgs[1].thinking).toEqual([
      "It is a 1x1 PNG.",
      "So: one pixel, white.",
    ]);
  });

  it("drops nothing else the server sent", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ messages: [userRow, replyRow] }));
    const msgs = await client().getSessionMessages("sess-1");
    // Against the wire rows themselves, so any field the mapping forgets
    // fails here rather than in some screen that quietly shows less.
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
    // The endpoint answers 200 with a status/reason pair for everything short
    // of a server fault, so the client must NOT model a refusal as an error —
    // being refused is the common path.
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
    // A 500 is a genuine failure and still throws; a 401 is handled separately
    // (re-authenticate and retry) — see the re-authentication suite.
    fetchMock.mockResolvedValueOnce(errJson(500, "internal error"));
    const gen = client().chatStream("hi");
    await expect(gen.next()).rejects.toBeInstanceOf(ApiError);
  });

  /**
   * The server's SSE handler holds one of four `sse_semaphore` permits and an
   * `AttachGuard` for as long as the response body is open. `releaseLock()`
   * alone does not close it, so an abandoned turn kept both — measured against
   * a live pond, four abandoned streams made every later send return
   * `503 Too many concurrent streams` in under 2 ms, until the browser
   * happened to garbage-collect the Response.
   */
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

// ── searchLlamafileModels ─────────────────────────────────────────────────────

// ── wake-word calibration ────────────────────────────────────────��───────────

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
    // Verify body is FormData (not JSON)
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
    // The client thinks its token is valid (future expiry), but the server
    // rejects it — exactly the post-restart / rotated-token case. A refresh
    // token is present so re-auth resolves without a full pairing.
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
      // The stale token is rejected; the refreshed one succeeds.
      return auth === "Bearer stale"
        ? errJson(401, "Invalid or expired token")
        : okJson([{ id: "d1", name: "Lamp", is_online: true }]);
    });

    const api = client();
    // Five concurrent calls all carry the stale token and 401 together.
    await Promise.all([
      api.listDevices(),
      api.listDevices(),
      api.listDevices(),
      api.listDevices(),
      api.listDevices(),
    ]);

    // Coalesced: one refresh for the whole burst, not one per request.
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
      // The chat stream is rejected on the stale token, accepted on the fresh one.
      return auth === "Bearer stale"
        ? errJson(401, "Invalid or expired token")
        : emptySse();
    });

    // Consuming the stream must not throw — it recovers and completes.
    const api = client();
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
    expect(id).not.toBe("pond-desktop"); // the old shared id that caused revocation churn
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

// One call redirects the whole singleton, which is what lets the shell correct
// a fallback port without reloading the renderer.
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
