/** Drives the store with nothing mounted, as happens whenever a sidebar press unmounts Chat. */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import {
  __resetChatRunForTests,
  setChatRunBridge,
  resumeActiveRun,
  abortRun,
  useChatRun,
  getChatRun,
  sendTurn,
  hasLiveThread,
  acknowledgeCompletion,
  resetConversation,
  openSession,
  followExternalSession,
  truncateFrom,
  patchMessage,
  takeRefusedDraft,
} from "./chatRunStore";
import type { ChatRunBridge } from "./chatRunStore";
import { ApiError } from "../api/types";
import type { ChatEvent } from "../api/types";
import type { PreparedImage } from "../lib/imageAttach";
import { OTHER_ID, STARTED_TEXT, TASK_ID } from "../longtasks/longTaskFixtures";
import {
  SEARCH_STUB,
  SEARCH_TOOL,
  assistantRow,
  keptToolRow,
  userRow,
} from "../test-support/keptRows";

vi.mock("../api/PondApiClient", () => ({
  api: {
    chatStream: vi.fn(),
    setToken: vi.fn(),
    getSessionMessages: vi.fn(),
    getActiveRun: vi.fn(),
    reattachRun: vi.fn(),
    cancelRun: vi.fn(),
    getSessionAttachment: vi.fn(),
  },
}));

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Yield the given frames, then finish. */
function stream(events: ChatEvent[]): AsyncGenerator<ChatEvent> {
  return (async function* () {
    for (const ev of events) yield ev;
  })();
}

/** Held-open stream; `push`/`end` resolve once the driver has consumed the frame. */
function deferredStream() {
  const pending: ChatEvent[] = [];
  let wake: (() => void) | null = null;
  let done = false;

  const gen = (async function* () {
    for (;;) {
      while (pending.length > 0) yield pending.shift()!;
      if (done) return;
      await new Promise<void>((r) => {
        wake = r;
      });
    }
  })();

  return {
    gen,
    async push(ev: ChatEvent) {
      pending.push(ev);
      wake?.();
      wake = null;
      await flush();
    },
    async end() {
      done = true;
      wake?.();
      wake = null;
      await flush();
    },
  };
}

/** Let every already-scheduled microtask and macrotask settle. */
async function flush(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await new Promise((r) => setTimeout(r, 0));
}

/** Named for its preview URL, so assertions read as which attachment went where. */
function fakePreparedImage(previewUrl: string): PreparedImage {
  return {
    data: "AAA",
    mime_type: "image/png",
    previewUrl,
    width: 10,
    height: 10,
    byteSize: 3,
  };
}

/** How `chatStream` fails when the server refuses a turn before the first SSE frame. */
function rejectedStream(err: unknown): AsyncGenerator<ChatEvent> {
  return (async function* () {
    throw err;
  })();
}

function bridge(over: Partial<ChatRunBridge> = {}): ChatRunBridge {
  return {
    serverOnline: true,
    onSessionId: vi.fn(),
    onResponseMeta: vi.fn(),
    onContextCard: vi.fn(),
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  __resetChatRunForTests();
  setChatRunBridge(bridge());
  vi.mocked(api.getSessionMessages).mockResolvedValue([]);
  vi.mocked(api.getActiveRun).mockResolvedValue(null);
  // stopServerRun chains `.catch` on it, so it must return a promise.
  vi.mocked(api.cancelRun).mockResolvedValue(undefined);
  localStorage.clear();
});

// ── Starting a turn ───────────────────────────────────────────────────────────

describe("starting a turn", () => {
  it("claims the turn before it awaits anything", () => {
    vi.mocked(api.chatStream).mockReturnValue(stream([]) as never);
    sendTurn({ text: "hello" });

    // Synchronously, on the same tick: two sends in one tick must not both run.
    const run = getChatRun();
    expect(run.busy).toBe(true);
    expect(run.messages.map((m) => m.role)).toEqual(["user", "agent"]);
    expect(run.messages[0].text).toBe("hello");
    expect(run.messages[1].streaming).toBe(true);
  });

  it("holds a second message rather than starting a second run", () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);

    sendTurn({ text: "first" });
    sendTurn({ text: "second" });

    expect(getChatRun().queued).toEqual(["second"]);
    expect(api.chatStream).toHaveBeenCalledTimes(1);
  });

  it("refuses a turn with neither words nor images", () => {
    sendTurn({ text: "   " });
    expect(api.chatStream).not.toHaveBeenCalled();
    expect(getChatRun().busy).toBe(false);
  });
});

// ── The reason this store exists ──────────────────────────────────────────────

describe("a turn nobody is watching", () => {
  it("keeps folding frames after the last subscriber leaves", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);

    const mounted = renderHook(() => useChatRun());

    sendTurn({ text: "why do geese fly in a V" });
    await held.push({ type: "text", content: "Because " } as ChatEvent);
    expect(mounted.result.current.messages[1].text).toBe("Because ");

    // The sidebar press.
    mounted.unmount();

    await held.push({ type: "text", content: "it saves " } as ChatEvent);
    await held.push({ type: "text", content: "energy." } as ChatEvent);
    await held.end();

    const run = getChatRun();
    expect(run.messages[1].text).toBe("Because it saves energy.");
    expect(run.messages[1].streaming).toBe(false);
    expect(run.busy).toBe(false);
  });

  it("drains its queue with nothing mounted", async () => {
    const first = deferredStream();
    vi.mocked(api.chatStream).mockReturnValueOnce(first.gen as never);
    vi.mocked(api.chatStream).mockReturnValueOnce(
      stream([{ type: "text", content: "and second" } as ChatEvent]) as never,
    );

    sendTurn({ text: "first" });
    sendTurn({ text: "second" });
    await first.push({ type: "text", content: "first answer" } as ChatEvent);
    await first.end();

    expect(api.chatStream).toHaveBeenCalledTimes(2);
    expect(vi.mocked(api.chatStream).mock.calls[1][0]).toBe("second");
    expect(getChatRun().queued).toEqual([]);
    expect(getChatRun().busy).toBe(false);
  });
});

// ── The bridge ────────────────────────────────────────────────────────────────

describe("talking back to the app", () => {
  it("forwards the session and the model that answered", async () => {
    const b = bridge();
    setChatRunBridge(b);
    vi.mocked(api.chatStream).mockReturnValue(
      stream([
        {
          done: true,
          session_id: "sess-9",
          model_role: "chat",
          model_name: "gemma",
          usage: { prompt_tokens: 5, completion_tokens: 7 },
        } as ChatEvent,
      ]) as never,
    );

    sendTurn({ text: "hi" });
    await flush();

    expect(b.onSessionId).toHaveBeenCalledWith("sess-9");
    expect(b.onResponseMeta).toHaveBeenCalledWith({
      modelName: "gemma",
      modelRole: "chat",
      completionTokens: 7,
    });
    expect(getChatRun().sessionId).toBe("sess-9");
  });

  it("forwards a tool call as a context card", async () => {
    const b = bridge();
    setChatRunBridge(b);
    vi.mocked(api.chatStream).mockReturnValue(
      stream([
        {
          type: "tool_call",
          tool: "get_current_weather",
          id: "t1",
        } as ChatEvent,
      ]) as never,
    );

    sendTurn({ text: "weather?" });
    await flush();

    expect(b.onContextCard).toHaveBeenCalledTimes(1);
    expect(getChatRun().messages[1].cards?.[0].tool).toBe(
      "get_current_weather",
    );
    expect(getChatRun().messages[1].status).toBe("Checking the weather…");
  });

  it("still sends when no provider is mounted to bridge it", async () => {
    // The client holds and refreshes its own token, so a bridgeless send still authenticates.
    __resetChatRunForTests();
    vi.mocked(api.chatStream).mockReturnValue(
      stream([
        { type: "text", content: "fine" } as ChatEvent,
        { done: true, session_id: "s" } as ChatEvent,
      ]) as never,
    );

    expect(() => sendTurn({ text: "hi" })).not.toThrow();
    await flush();
    expect(vi.mocked(api.chatStream).mock.calls[0][2]).toBeUndefined();
    expect(getChatRun().messages[1].text).toBe("fine");
  });
});

// ── An offline server ─────────────────────────────────────────────────────────

describe("a queue held through an outage", () => {
  it("waits for the server rather than dropping what was typed", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValueOnce(held.gen as never);

    sendTurn({ text: "first" });
    sendTurn({ text: "second" });

    setChatRunBridge(bridge({ serverOnline: false }));
    await held.end();

    expect(api.chatStream).toHaveBeenCalledTimes(1);
    expect(getChatRun().queued).toEqual(["second"]);

    vi.mocked(api.chatStream).mockReturnValueOnce(stream([]) as never);
    setChatRunBridge(bridge({ serverOnline: true }));
    await flush();

    expect(api.chatStream).toHaveBeenCalledTimes(2);
    expect(getChatRun().queued).toEqual([]);
  });
});

// ── Where a surface lands when it opens ───────────────────────────────────────

describe("hasLiveThread", () => {
  it("is true while writing, stays true until somebody has read it", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);

    expect(hasLiveThread()).toBe(false);

    sendTurn({ text: "hi" });
    expect(hasLiveThread()).toBe(true);

    await held.end();
    expect(hasLiveThread()).toBe(true);

    acknowledgeCompletion();
    expect(hasLiveThread()).toBe(false);
  });

  it("comes back for the next turn", async () => {
    vi.mocked(api.chatStream).mockReturnValue(stream([]) as never);
    sendTurn({ text: "one" });
    await flush();
    acknowledgeCompletion();
    expect(hasLiveThread()).toBe(false);

    vi.mocked(api.chatStream).mockReturnValue(stream([]) as never);
    sendTurn({ text: "two" });
    await flush();
    expect(hasLiveThread()).toBe(true);
  });
});

// ── A conversation that moved on ──────────────────────────────────────────────

describe("a run the conversation has left behind", () => {
  it("writes nothing into the conversation that replaced it", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);

    sendTurn({ text: "old question" });
    await held.push({ type: "text", content: "old ans" } as ChatEvent);

    resetConversation();
    expect(getChatRun().messages).toEqual([]);

    await held.push({ type: "text", content: "wer" } as ChatEvent);
    await held.end();

    expect(getChatRun().messages).toEqual([]);
    expect(getChatRun().busy).toBe(false);
  });
});

// ── Object URLs ───────────────────────────────────────────────────────────────

describe("image previews", () => {
  it("revokes only the previews it created", () => {
    const revoke = vi
      .spyOn(URL, "revokeObjectURL")
      .mockImplementation(() => {});
    vi.mocked(api.chatStream).mockReturnValue(deferredStream().gen as never);

    sendTurn({
      text: "what is this",
      images: [{ data: "AAA", mime_type: "image/png" }],
      previewUrls: ["blob:pond/one"],
    });
    // A history image: an http URL this store didn't create.
    patchMessage(getChatRun().messages[1].id, {
      images: ["https://example.com/not-ours.png"],
    });

    resetConversation();

    expect(revoke).toHaveBeenCalledTimes(1);
    expect(revoke).toHaveBeenCalledWith("blob:pond/one");
    revoke.mockRestore();
  });
});

/** Any status but 408 (a client timeout also yields it); refusals precede `persist_user_message`. */
describe("a refused turn", () => {
  it("restores the draft on a 409 and does not touch the transcript or ownedPreviews", async () => {
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    vi.mocked(api.chatStream).mockReturnValue(
      rejectedStream(
        new ApiError(409, "Picture support is not ready yet.", "vision_not_ready"),
      ) as never,
    );

    sendTurn({
      text: "what is this",
      attachments: [fakePreparedImage("blob:pond/refused")],
    });
    await flush();

    const run = getChatRun();
    expect(run.messages).toEqual([]);
    expect(run.busy).toBe(false);
    // Not a completed turn -- nothing ran.
    expect(run.completedTurns).toBe(0);
    // Handed back, not freed: the composer's tray needs this preview again.
    expect(revoke).not.toHaveBeenCalled();

    const draft = takeRefusedDraft();
    expect(draft).toEqual({
      text: "what is this",
      attachments: [fakePreparedImage("blob:pond/refused")],
      message: "Picture support is not ready yet.",
      code: "vision_not_ready",
    });
    // Taken once -- a second read (StrictMode's double effect) gets nothing.
    expect(takeRefusedDraft()).toBeNull();

    revoke.mockRestore();
  });

  it("publishes the code from an ApiError with no attachments too", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      rejectedStream(new ApiError(415, "Picture 1 could not be read.", "image_unreadable")) as never,
    );

    sendTurn({ text: "look at this", attachments: [fakePreparedImage("blob:pond/bad")] });
    await flush();

    expect(takeRefusedDraft()?.code).toBe("image_unreadable");
  });

  it("does NOT restore on a client timeout (408) -- the server may still be running it", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      rejectedStream(new ApiError(408, "Request timed out")) as never,
    );

    sendTurn({ text: "slow one", attachments: [fakePreparedImage("blob:pond/timeout")] });
    await flush();

    const run = getChatRun();
    expect(run.messages).toHaveLength(2);
    expect(run.messages[1].error).toBe(true);
    expect(run.completedTurns).toBe(1);
    expect(run.refusedDraft).toBeNull();
    expect(takeRefusedDraft()).toBeNull();
  });

  it("does NOT restore on a plain network error", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      rejectedStream(new TypeError("Failed to fetch")) as never,
    );

    sendTurn({ text: "offline", attachments: [fakePreparedImage("blob:pond/offline")] });
    await flush();

    const run = getChatRun();
    expect(run.messages[1].error).toBe(true);
    expect(run.completedTurns).toBe(1);
    expect(takeRefusedDraft()).toBeNull();
  });
});

/** `<img src>` can't send the bearer header, so replayed images are fetched and shown as owned object URLs. */
describe("history images", () => {
  /** A user row carrying `ids` as attachments, shaped as the history read sends it. */
  function rowWithImages(sessionId: string, ...ids: string[]) {
    return {
      id: `m-${sessionId}`,
      session_id: sessionId,
      role: "user",
      content: "what is this",
      created_at: "",
      images: ids.map((id) => ({
        id,
        mime_type: "image/png",
        byte_size: 3,
        url: `/api/v1/sessions/${sessionId}/attachments/${id}`,
      })),
    };
  }

  // Blobs and object URLs are named for their attachment, so assertions read as which image went where.
  const named = new Map<Blob, string>();
  const blobFor = (id: string) => {
    const b = new Blob([id], { type: "image/png" });
    named.set(b, id);
    return b;
  };
  let create: ReturnType<typeof vi.spyOn>;
  let revoke: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    named.clear();
    create = vi
      .spyOn(URL, "createObjectURL")
      .mockImplementation(
        (b) => `blob:pond/${named.get(b as Blob) ?? "unknown"}`,
      );
    revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    vi.mocked(api.getSessionAttachment).mockImplementation(async (_s, id) =>
      blobFor(id),
    );
  });

  afterEach(() => {
    create.mockRestore();
    revoke.mockRestore();
  });

  it("fetches through the client and shows an object URL, never the attachment URL", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      rowWithImages("s-img", "a1"),
    ] as never);

    await openSession("s-img");
    await flush();

    // Through the client, which is what carries the bearer token...
    expect(api.getSessionAttachment).toHaveBeenCalledWith("s-img", "a1");
    // ...and on screen as the URL made from its bytes.
    expect(getChatRun().messages[0].images).toEqual(["blob:pond/a1"]);
  });

  it("owns them: leaving the conversation revokes them", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      rowWithImages("s-img", "a1"),
    ] as never);
    await openSession("s-img");
    await flush();

    resetConversation();

    expect(revoke).toHaveBeenCalledWith("blob:pond/a1");
  });

  it("shows the text without waiting for the images", async () => {
    let land!: () => void;
    const held = new Promise<void>((r) => {
      land = r;
    });
    vi.mocked(api.getSessionAttachment).mockImplementation(async (_s, id) => {
      await held;
      return blobFor(id);
    });
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      rowWithImages("s-img", "a1"),
    ] as never);

    await openSession("s-img");

    expect(getChatRun().loadingSession).toBe(false);
    expect(getChatRun().messages[0].text).toBe("what is this");
    expect(getChatRun().messages[0].images).toBeUndefined();

    land();
    await flush();
    expect(getChatRun().messages[0].images).toEqual(["blob:pond/a1"]);
  });

  it("makes no URL for bytes that land after the conversation moved on", async () => {
    let land!: () => void;
    const held = new Promise<void>((r) => {
      land = r;
    });
    vi.mocked(api.getSessionAttachment).mockImplementation(async (_s, id) => {
      await held;
      return blobFor(id);
    });
    vi.mocked(api.getSessionMessages).mockResolvedValueOnce([
      rowWithImages("s-img", "a1"),
    ] as never);
    await openSession("s-img");

    // Somewhere else before the image arrived.
    vi.mocked(api.getSessionMessages).mockResolvedValueOnce([
      {
        id: "m-other",
        session_id: "s-other",
        role: "user",
        content: "something else",
        created_at: "",
      },
    ] as never);
    await openSession("s-other");
    land();
    await flush();

    // Made now, a URL would be shown by nothing and so revoked by nothing...
    expect(create).not.toHaveBeenCalled();
    // ...and it must not turn up on the conversation that replaced its own.
    expect(getChatRun().messages[0].text).toBe("something else");
    expect(getChatRun().messages[0].images).toBeUndefined();
  });

  it("leaves out an image it cannot fetch and keeps the rest in order", async () => {
    let releaseFirst!: () => void;
    const firstHeld = new Promise<void>((r) => {
      releaseFirst = r;
    });
    vi.mocked(api.getSessionAttachment).mockImplementation(async (_s, id) => {
      if (id === "gone") throw new Error("404 Attachment bytes are no longer available");
      // The first image lands last, so order cannot come from arrival.
      if (id === "a1") await firstHeld;
      return blobFor(id);
    });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      rowWithImages("s-img", "a1", "gone", "a3"),
    ] as never);

    await openSession("s-img");
    await flush();
    releaseFirst();
    await flush();

    expect(getChatRun().messages[0].images).toEqual([
      "blob:pond/a1",
      "blob:pond/a3",
    ]);
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });

  it("loads them for a session followed from outside, too", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      rowWithImages("s-deep", "a1"),
    ] as never);

    await followExternalSession("s-deep");
    await flush();

    expect(getChatRun().messages[0].images).toEqual(["blob:pond/a1"]);
  });
});

// ── Editing ───────────────────────────────────────────────────────────────────

describe("truncateFrom", () => {
  it("drops the message and everything after it", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      stream([{ type: "text", content: "answer" } as ChatEvent]) as never,
    );
    sendTurn({ text: "question" });
    await flush();

    const userId = getChatRun().messages[0].id;
    truncateFrom(userId);
    expect(getChatRun().messages).toEqual([]);
  });
});

// ── Surviving a reload ────────────────────────────────────────────────────────

/** Drives the reload seam: a run pointer in localStorage and an empty store. */
describe("resuming a run this window never started", () => {
  const POINTER = {
    sessionId: "sess-live",
    runId: "run-7",
    epoch: "epoch-a",
    lastSeq: 4,
  };

  function leaveAPointer(over: Partial<typeof POINTER> = {}) {
    localStorage.setItem(
      "giap-chat-run",
      JSON.stringify({ ...POINTER, ...over }),
    );
  }

  it("does nothing at all on an ordinary cold start", async () => {
    expect(await resumeActiveRun()).toBe(false);
    expect(api.getActiveRun).not.toHaveBeenCalled();
    expect(hasLiveThread()).toBe(false);
  });

  it("sends the surface to the thread before the server has even answered", async () => {
    // Surfaces pick a screen while mounting, before any round trip, so the pointer must suffice.
    leaveAPointer();
    expect(hasLiveThread()).toBe(true);
  });

  it("still opens the conversation when the run turns out to be gone", async () => {
    leaveAPointer();
    vi.mocked(api.getActiveRun).mockResolvedValue(null);
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      {
        id: "m1",
        session_id: "sess-live",
        role: "user",
        content: "still here",
        created_at: "",
      },
    ] as never);

    await resumeActiveRun();

    // hasLiveThread already chose the thread from the pointer, so it must not open empty.
    expect(getChatRun().messages[0].text).toBe("still here");
  });

  it("picks up a turn that is still being written", async () => {
    leaveAPointer();
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      {
        id: "m1",
        session_id: "sess-live",
        role: "user",
        content: "why a V",
        created_at: "",
      },
    ] as never);
    vi.mocked(api.getActiveRun).mockResolvedValue({
      run_id: "run-7",
      session_id: "sess-live",
      state: "running",
      started_at: "",
      first_seq: 1,
      last_seq: 6,
      epoch: "epoch-a",
    } as never);
    const held = deferredStream();
    vi.mocked(api.reattachRun).mockReturnValue(held.gen as never);

    const resumed = resumeActiveRun();
    await flush();

    // From the last window's read position, not from 0.
    expect(vi.mocked(api.reattachRun).mock.calls[0].slice(0, 3)).toEqual([
      "run-7",
      4,
      "epoch-a",
    ]);
    expect(getChatRun().busy).toBe(true);
    expect(getChatRun().messages[0].text).toBe("why a V");

    await held.push({ type: "text", content: "it saves energy." } as ChatEvent);
    await held.push({ done: true, session_id: "sess-live" } as ChatEvent);
    await held.end();
    await resumed;

    const run = getChatRun();
    expect(run.messages[1].text).toBe("it saves energy.");
    expect(run.busy).toBe(false);
    expect(localStorage.getItem("giap-chat-run")).toBeNull();
  });

  it("opens the finished thread rather than tailing a turn that is already over", async () => {
    leaveAPointer();
    vi.mocked(api.getActiveRun).mockResolvedValue({
      run_id: "run-7",
      session_id: "sess-live",
      state: "finished",
      started_at: "",
      first_seq: 1,
      last_seq: 9,
      epoch: "epoch-a",
    } as never);

    expect(await resumeActiveRun()).toBe(true);
    expect(api.reattachRun).not.toHaveBeenCalled();
    expect(hasLiveThread()).toBe(true);
    expect(localStorage.getItem("giap-chat-run")).toBeNull();
  });

  it("gives up quietly when the server has restarted underneath it", async () => {
    leaveAPointer();
    vi.mocked(api.getActiveRun).mockResolvedValue({
      run_id: "run-7",
      session_id: "sess-live",
      state: "running",
      started_at: "",
      first_seq: 1,
      last_seq: 6,
      // A different process. The run this pointer names died with the last one.
      epoch: "epoch-b",
    } as never);

    expect(await resumeActiveRun()).toBe(false);
    expect(api.reattachRun).not.toHaveBeenCalled();
    expect(
      localStorage.getItem("giap-chat-run"),
      "a pointer to a run that cannot exist must not be tried again",
    ).toBeNull();
  });

  it("gives up quietly when the run is simply gone", async () => {
    leaveAPointer();
    vi.mocked(api.getActiveRun).mockResolvedValue(null);
    expect(await resumeActiveRun()).toBe(false);
    expect(localStorage.getItem("giap-chat-run")).toBeNull();
  });
});

describe("remembering the run", () => {
  it("writes the pointer down as soon as the turn names itself", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);

    sendTurn({ text: "hi" });
    await held.push({
      type: "run_started",
      run_id: "run-9",
      session_id: "sess-x",
      epoch: "epoch-a",
      seq: 1,
    } as ChatEvent);

    const pointer = JSON.parse(localStorage.getItem("giap-chat-run") ?? "{}");
    expect(pointer.runId).toBe("run-9");
    expect(pointer.epoch).toBe("epoch-a");
  });

  it("asks for the turn to be resumable in the first place", () => {
    vi.mocked(api.chatStream).mockReturnValue(stream([]) as never);
    sendTurn({ text: "hi" });
    expect(vi.mocked(api.chatStream).mock.calls[0][5]).toBe(true);
  });

  it("reloads the conversation rather than showing half an answer as whole", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);
    sendTurn({ text: "hi" });
    await held.push({ done: true, session_id: "sess-gap" } as ChatEvent);
    await held.end();

    vi.mocked(api.getSessionMessages).mockResolvedValue([
      {
        id: "m1",
        session_id: "sess-gap",
        role: "user",
        content: "hi",
        created_at: "",
      },
    ] as never);
    const gapped = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(gapped.gen as never);
    sendTurn({ text: "again" });
    await gapped.push({ type: "text", content: "partial" } as ChatEvent);
    await gapped.push({
      type: "replay_gap",
      requested_after_seq: 2,
      first_available_seq: 40,
      advice: "reload_session_messages",
    } as ChatEvent);
    await gapped.end();

    expect(api.getSessionMessages).toHaveBeenCalledWith("sess-gap");
  });
});

describe("stopping on purpose", () => {
  it("tells the server, because hanging up no longer does", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);
    sendTurn({ text: "hi" });
    await held.push({
      type: "run_started",
      run_id: "run-11",
      session_id: "s",
      epoch: "e",
      seq: 1,
    } as ChatEvent);

    await abortRun();

    expect(api.cancelRun).toHaveBeenCalledWith("run-11");
    expect(getChatRun().busy).toBe(false);
    expect(localStorage.getItem("giap-chat-run")).toBeNull();
  });

  /** Bumping `runSeq` only stops local writes; the model keeps generating unless cancelled. */
  it("stops the run when a new chat abandons it, as its doc has always said", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);
    sendTurn({ text: "hi" });
    await held.push({
      type: "run_started",
      run_id: "run-20",
      session_id: "s",
      epoch: "e",
      seq: 1,
    } as ChatEvent);

    resetConversation();

    expect(api.cancelRun).toHaveBeenCalledWith("run-20");
  });

  it("stops the run when another conversation is opened over it", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);
    sendTurn({ text: "hi" });
    await held.push({
      type: "run_started",
      run_id: "run-21",
      session_id: "s",
      epoch: "e",
      seq: 1,
    } as ChatEvent);

    await openSession("some-other-session", { stopCurrentRun: true });

    expect(api.cancelRun).toHaveBeenCalledWith("run-21");
  });

  /** Resume and replay-gap recovery re-read a live run, so cancelling by default would kill it. */
  it("does not stop the run when a session is merely re-read", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);
    sendTurn({ text: "hi" });
    await held.push({
      type: "run_started",
      run_id: "run-23",
      session_id: "s",
      epoch: "e",
      seq: 1,
    } as ChatEvent);

    await openSession("s");

    expect(api.cancelRun).not.toHaveBeenCalled();
  });

  /** Leaving on purpose cancels; a window going away must not (nothing runs on unload). */
  it("does not stop a run just because the last subscriber unmounted", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);
    const view = renderHook(() => useChatRun());
    sendTurn({ text: "hi" });
    await held.push({
      type: "run_started",
      run_id: "run-22",
      session_id: "s",
      epoch: "e",
      seq: 1,
    } as ChatEvent);

    view.unmount();
    await flush();

    expect(api.cancelRun).not.toHaveBeenCalled();
  });

  it("still clears locally when the server cannot be told", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);
    sendTurn({ text: "hi" });
    await held.push({
      type: "run_started",
      run_id: "run-12",
      session_id: "s",
      epoch: "e",
      seq: 1,
    } as ChatEvent);
    vi.mocked(api.cancelRun).mockRejectedValue(new Error("offline"));

    await expect(abortRun()).resolves.toBeUndefined();
    expect(getChatRun().busy).toBe(false);
  });
});

/** Brave's terms let the pond hold a search result only for the turn, so history keeps a stub. */
describe("a web search result the pond does not keep", () => {
  const TITLE = "Kenya Power outage schedule";
  const SNIPPET = "Planned maintenance in Westlands from 9 am to 5 pm.";
  const DOMAIN = "kenyapower.co.ke";

  function searchTurn(transient: boolean): ChatEvent[] {
    return [
      { type: "tool_call", tool: SEARCH_TOOL, id: "call-1" } as ChatEvent,
      {
        type: "tool_result",
        tool: SEARCH_TOOL,
        id: "call-1",
        content: `Web results for: power\n${SNIPPET}`,
        ui: {
          card_type: "search",
          data: {
            query: "when does the power come back",
            results: [{ title: TITLE, url: `https://${DOMAIN}/outages`, domain: DOMAIN, snippet: SNIPPET }],
          },
        },
        ...(transient ? { transient: true } : {}),
      } as ChatEvent,
      { type: "text", content: "Power should be back by 6 pm.", token: "Power should be back by 6 pm." } as ChatEvent,
      { done: true, session_id: "sess-1", model_role: "chat" } as ChatEvent,
    ];
  }

  function everythingStored(): string {
    const dump = (s: Storage) => JSON.stringify(Object.fromEntries(Object.keys(s).map((k) => [k, s.getItem(k)])));
    return dump(localStorage) + dump(sessionStorage);
  }

  it("shows the card for the turn, and writes none of it to web storage", async () => {
    vi.mocked(api.chatStream).mockReturnValue(stream(searchTurn(true)) as never);
    sendTurn({ text: "when does the power come back?" });
    await flush();

    const card = getChatRun().messages[1].cards?.[0];
    expect(card?.renderHint).toBe("search");
    expect(JSON.stringify(card?.data)).toContain(TITLE);

    const stored = everythingStored();
    expect(stored).not.toContain(TITLE);
    expect(stored).not.toContain(SNIPPET);
    expect(stored).not.toContain(DOMAIN);
  });

  it("keeps a reattachable run's pointer free of the result, as it must be of any content", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);
    sendTurn({ text: "q" });
    await held.push({ type: "run_started", run_id: "run-1", session_id: "sess-1", epoch: "e", seq: 1 } as ChatEvent);
    for (const ev of searchTurn(true).slice(0, 2)) await held.push(ev);

    const pointer = JSON.parse(localStorage.getItem("giap-chat-run") ?? "{}");
    expect(Object.keys(pointer).sort()).toEqual(["epoch", "lastSeq", "runId", "sessionId"]);
    expect(everythingStored()).not.toContain(SNIPPET);
    await held.end();
  });

  it("drops the card when the conversation is reopened, and shows one quiet line in its place", async () => {
    vi.mocked(api.chatStream).mockReturnValue(stream(searchTurn(true)) as never);
    sendTurn({ text: "when does the power come back?" });
    await flush();

    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow(),
      keptToolRow(),
      assistantRow(),
    ]);
    await openSession("sess-1");

    const [asked, answered] = getChatRun().messages;
    expect(asked.role).toBe("user");
    expect(answered.cards).toBeUndefined();
    expect(answered.text).toBe("Power should be back by 6 pm.");
    expect(answered.keptResults).toEqual([
      { callId: "call-1", tool: SEARCH_TOOL, stub: SEARCH_STUB },
    ]);
    expect(JSON.stringify(getChatRun().messages)).not.toContain(SNIPPET);
  });

  it("gives each turn its own notes, and none to a turn that kept its results whole", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow({ id: "u1" }),
      keptToolRow({ id: "t1" }),
      assistantRow({ id: "a1", content: "First answer." }),
      userRow({ id: "u2", content: "and the weather?" }),
      keptToolRow(
        { id: "t2", tool_call_id: "call-2" },
        { tool_call_id: "call-2", tool: "get_current_weather", transient: undefined, content: "24 degrees" },
      ),
      assistantRow({ id: "a2", content: "24 degrees." }),
    ]);
    await openSession("sess-1");

    const agents = getChatRun().messages.filter((m) => m.role === "agent");
    expect(agents.map((m) => m.keptResults?.length ?? 0)).toEqual([1, 0]);
  });

  it("does not carry a note over to the next answer when its own turn showed nothing", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow({ id: "u1" }),
      keptToolRow({ id: "t1" }),
      // A turn that called a tool and said nothing is not shown, so its note has no bubble to sit under.
      assistantRow({ id: "a1", content: "" }),
      userRow({ id: "u2", content: "thanks" }),
      assistantRow({ id: "a2", content: "You're welcome.", tool_calls: undefined }),
    ]);
    await openSession("sess-1");

    const agents = getChatRun().messages.filter((m) => m.role === "agent");
    expect(agents).toHaveLength(1);
    expect(agents[0].keptResults).toBeUndefined();
  });

  it("reads the same stub from a history that marks the row itself", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow(),
      keptToolRow({ content: SEARCH_STUB, transient: true }),
      assistantRow(),
    ]);
    await openSession("sess-1");
    expect(getChatRun().messages[1].keptResults?.[0].stub).toBe(SEARCH_STUB);
  });
});

// A long task the prime starts is found in the tool's own text, live and in a stored row.
describe("a long task the prime starts", () => {
  const START_TOOL = "giap-tasks__task_start";

  function startRow(id: string, text: string, tool = START_TOOL) {
    return {
      id: `tool-${id}`,
      session_id: "sess-1",
      role: "tool" as const,
      created_at: "",
      tool_call_id: `call-${id}`,
      content: JSON.stringify({ tool_call_id: `call-${id}`, tool, content: text }),
    };
  }

  const startedText = (id: string) => `Started task ${id}. It runs in the background.`;

  it("is found in the result of a live turn, on the answer that follows", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      stream([
        { type: "tool_call", tool: START_TOOL, id: "t1" },
        { type: "tool_result", tool: START_TOOL, id: "t1", content: STARTED_TEXT },
        { type: "text", content: "Started." },
      ]) as never,
    );
    sendTurn({ text: "plan a trip" });
    await flush();
    expect(getChatRun().messages[1].longTaskIds).toEqual([TASK_ID]);
    expect(getChatRun().messages[1].text).toBe("Started.");
  });

  it("is found once however many times the result is repeated, and again for a second task", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      stream([
        { type: "tool_call", tool: START_TOOL, id: "t1" },
        { type: "tool_result", tool: START_TOOL, id: "t1", content: STARTED_TEXT },
        { type: "tool_result", tool: START_TOOL, id: "t1", content: STARTED_TEXT },
        { type: "tool_call", tool: START_TOOL, id: "t2" },
        { type: "tool_result", tool: START_TOOL, id: "t2", content: startedText(OTHER_ID) },
      ]) as never,
    );
    sendTurn({ text: "plan two trips" });
    await flush();
    expect(getChatRun().messages[1].longTaskIds).toEqual([TASK_ID, OTHER_ID]);
  });

  it("is not found in the same words from another tool", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      stream([
        { type: "tool_call", tool: "giap-notes__read_note", id: "t1" },
        { type: "tool_result", tool: "giap-notes__read_note", id: "t1", content: STARTED_TEXT },
      ]) as never,
    );
    sendTurn({ text: "read my note" });
    await flush();
    expect(getChatRun().messages[1].longTaskIds).toBeUndefined();
  });

  it("is found in a stored tool row, and goes to the answer of its own turn", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow({ id: "u1" }),
      startRow("1", STARTED_TEXT),
      assistantRow({ id: "a1", content: "Started.", tool_calls: undefined }),
      userRow({ id: "u2", content: "thanks" }),
      assistantRow({ id: "a2", content: "You're welcome.", tool_calls: undefined }),
    ]);
    await openSession("sess-1");
    const agents = getChatRun().messages.filter((m) => m.role === "agent");
    expect(agents.map((m) => m.longTaskIds)).toEqual([[TASK_ID], undefined]);
  });

  it("is found in a stored row whose content is the text itself", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow({ id: "u1" }),
      { ...startRow("1", ""), content: STARTED_TEXT },
      assistantRow({ id: "a1", content: "Started.", tool_calls: undefined }),
    ]);
    await openSession("sess-1");
    expect(getChatRun().messages[1].longTaskIds).toEqual([TASK_ID]);
  });

  it("reaches the answer past an empty step of the same turn, but not into the next question's answer", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow({ id: "u1" }),
      startRow("1", STARTED_TEXT),
      assistantRow({ id: "a0", content: "" }),
      assistantRow({ id: "a1", content: "Started.", tool_calls: undefined }),
      userRow({ id: "u2", content: "and another" }),
      startRow("2", startedText(OTHER_ID)),
      assistantRow({ id: "a2", content: "" }),
      userRow({ id: "u3", content: "thanks" }),
      assistantRow({ id: "a3", content: "You're welcome.", tool_calls: undefined }),
    ]);
    await openSession("sess-1");
    const agents = getChatRun().messages.filter((m) => m.role === "agent");
    expect(agents.map((m) => m.longTaskIds)).toEqual([[TASK_ID], undefined]);
  });

  it("is not found in a stored row of another tool", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow({ id: "u1" }),
      startRow("1", STARTED_TEXT, "giap-notes__read_note"),
      assistantRow({ id: "a1", content: "Here it is.", tool_calls: undefined }),
    ]);
    await openSession("sess-1");
    expect(getChatRun().messages[1].longTaskIds).toBeUndefined();
  });
});

// Deep research: the pond starts the task itself and streams the frames a model-made call would.
describe("a deep research turn", () => {
  const START_TOOL = "giap-tasks__task_start";
  // The call id carries a uuid of its own, which is not the task's.
  const CALL_ID = `deep-research-${OTHER_ID}`;
  const QUESTION = "What changed in Kenya's tax rules this year?";
  const POND_RESULT = `Started task ${TASK_ID}. It runs in the background and will report here when it is done.`;

  function pondFrames(): ChatEvent[] {
    return [
      { type: "tool_call", tool: START_TOOL, id: CALL_ID, input: { goal: QUESTION } },
      { type: "tool_result", tool: START_TOOL, id: CALL_ID, content: POND_RESULT },
      { type: "text", content: "I have started the research. " },
      { type: "text", content: "I will report here when it is done." },
      { done: true, session_id: "sess-dr", type: "done" },
    ];
  }

  it("asks the pond for research, and only when the turn is one", () => {
    vi.mocked(api.chatStream).mockReturnValue(stream([]) as never);
    sendTurn({ text: QUESTION, deepResearch: true });
    const call = vi.mocked(api.chatStream).mock.calls[0];
    expect(call[0]).toBe(QUESTION);
    expect(call[5]).toBe(true);
    expect(call[6]).toBe(true);
  });

  it("leaves it off an ordinary turn", () => {
    vi.mocked(api.chatStream).mockReturnValue(stream([]) as never);
    sendTurn({ text: "hello" });
    expect(vi.mocked(api.chatStream).mock.calls[0][6]).toBeUndefined();
  });

  it("anchors the card to the pond's own task_start frames, and takes the task from the result, not the call id", async () => {
    vi.mocked(api.chatStream).mockReturnValue(stream(pondFrames()) as never);
    sendTurn({ text: QUESTION, deepResearch: true });
    await flush();

    const [user, agent] = getChatRun().messages;
    expect(user.text).toBe(QUESTION);
    expect(agent.longTaskIds).toEqual([TASK_ID]);
    expect(agent.text).toBe("I have started the research. I will report here when it is done.");
    expect(agent.cards).toHaveLength(1);
    expect(agent.cards?.[0]).toMatchObject({ tool: START_TOOL, callId: CALL_ID });
    expect(agent.status).toBeUndefined();
    expect(agent.streaming).toBe(false);
    expect(getChatRun().completedTurns).toBe(1);
  });

  it("anchors the card while the turn is still streaming, before the reply has any words", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);
    sendTurn({ text: QUESTION, deepResearch: true });
    await held.push(pondFrames()[0]);
    expect(getChatRun().messages[1].status).toBe("Getting that started…");
    await held.push(pondFrames()[1]);

    const agent = getChatRun().messages[1];
    expect(agent.longTaskIds).toEqual([TASK_ID]);
    expect(agent.streaming).toBe(true);
    expect(agent.text).toBe("");
    await held.end();
  });

  it("anchors it again when the conversation is reopened, from the stored tool row", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow({ id: "u1", content: QUESTION }),
      assistantRow({
        id: "a0",
        content: "",
        tool_calls: [{ id: CALL_ID, name: START_TOOL, arguments: JSON.stringify({ goal: QUESTION }) }],
      }),
      {
        id: "t1",
        session_id: "sess-1",
        role: "tool",
        created_at: "",
        tool_call_id: CALL_ID,
        content: JSON.stringify({ tool_call_id: CALL_ID, tool: START_TOOL, content: POND_RESULT }),
      },
      assistantRow({ id: "a1", content: "I have started the research.", tool_calls: undefined }),
    ]);
    await openSession("sess-1");

    const { messages } = getChatRun();
    expect(messages.map((m) => m.role)).toEqual(["user", "agent"]);
    expect(messages[1].longTaskIds).toEqual([TASK_ID]);
    expect(messages[1].text).toBe("I have started the research.");
  });

  it("is never queued behind a reply that is still streaming", async () => {
    const held = deferredStream();
    vi.mocked(api.chatStream).mockReturnValue(held.gen as never);
    sendTurn({ text: "first" });
    sendTurn({ text: QUESTION, deepResearch: true });

    expect(getChatRun().queued).toEqual([]);
    expect(api.chatStream).toHaveBeenCalledTimes(1);
    await held.end();
  });

  describe("when the pond refuses it before saving anything", () => {
    it.each([
      [409, "deep_research_off", "Deep research is switched off on this pond."],
      [403, "deep_research_unidentified", "The pond cannot tell who is asking."],
    ] as const)("hands the question back on a %i, with the reason from the body", async (status, reason, message) => {
      vi.mocked(api.chatStream).mockReturnValue(
        rejectedStream(new ApiError(status, message, undefined, { error: message, reason })) as never,
      );
      sendTurn({ text: QUESTION, deepResearch: true });
      await flush();

      const run = getChatRun();
      expect(run.messages).toEqual([]);
      expect(run.busy).toBe(false);
      expect(run.completedTurns).toBe(0);
      expect(takeRefusedDraft()).toEqual({
        text: QUESTION,
        attachments: [],
        message,
        code: reason,
        deepResearch: true,
      });
    });

    it("prefers a code the body has to a reason", async () => {
      vi.mocked(api.chatStream).mockReturnValue(
        rejectedStream(new ApiError(409, "No model.", "no_model", { error: "No model.", code: "no_model", reason: "other" })) as never,
      );
      sendTurn({ text: QUESTION, deepResearch: true });
      await flush();
      expect(takeRefusedDraft()?.code).toBe("no_model");
    });

    it("says the turn was research even when it was refused for some other reason", async () => {
      vi.mocked(api.chatStream).mockReturnValue(
        rejectedStream(new ApiError(409, "No model.", "no_model")) as never,
      );
      sendTurn({ text: QUESTION, deepResearch: true });
      await flush();
      expect(takeRefusedDraft()).toMatchObject({ code: "no_model", deepResearch: true });
    });

    it("does not say so for an ordinary turn", async () => {
      vi.mocked(api.chatStream).mockReturnValue(
        rejectedStream(new ApiError(409, "No model.", "no_model")) as never,
      );
      sendTurn({ text: "hello" });
      await flush();
      expect("deepResearch" in (takeRefusedDraft() ?? {})).toBe(false);
    });
  });
});
