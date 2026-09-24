/**
 * The turn outlives the view.
 *
 * `GuiMode` swaps sections with a `switch`, so every sidebar press unmounts
 * `<Chat />`. These tests drive the store directly, with no component mounted,
 * because that is exactly the condition it exists for: a turn still streaming
 * while nothing is there to show it.
 *
 * The one that matters most — "keeps folding frames after the last subscriber
 * leaves" — does mount, through `useChatRun`, and then unmounts, because a real
 * subscriber leaving is the event under test and a hand-rolled stand-in for it
 * would be testing the stand-in. Before this store, the frames that arrived
 * after that point were decoded and thrown away: the answer arrived, was
 * written to the database, and was invisible to the person who asked for it.
 */

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
} from "./chatRunStore";
import type { ChatRunBridge } from "./chatRunStore";
import type { ChatEvent } from "../api/types";

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

/**
 * A stream held open until the test says otherwise.
 *
 * `push` delivers one frame, `end` closes it. Both resolve once the driver has
 * actually consumed the frame, so a test can unsubscribe at a known point in
 * the middle of a turn rather than racing it.
 */
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

function bridge(over: Partial<ChatRunBridge> = {}): ChatRunBridge {
  return {
    sessionToken: "test-token",
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
  // The real one returns `Promise<void>`; a bare `vi.fn()` returns undefined,
  // which only the callers that `await` it happen to survive.
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

    // A mounted surface, subscribed exactly the way a real one is.
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
    // The client holds its own token and refreshes it, so a bridgeless send
    // degrades to "the client authenticates itself", not to a failed send.
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
    // Finished, and nothing was mounted to show it -- still the thing you came
    // back for.
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
    // A URL on the same transcript that this store did not make, and so must
    // not claim to free. (History images used to be the example here; they are
    // object URLs the store owns now -- see "history images" below.)
    patchMessage(getChatRun().messages[1].id, {
      images: ["https://example.com/not-ours.png"],
    });

    resetConversation();

    expect(revoke).toHaveBeenCalledTimes(1);
    expect(revoke).toHaveBeenCalledWith("blob:pond/one");
    revoke.mockRestore();
  });
});

/**
 * Images on a replayed conversation.
 *
 * The attachment route is protected and accepts only a bearer header, which an
 * `<img src>` cannot send, so the bare URL this store used to hand both chat
 * surfaces answered 401 on every pond without the loopback dev bypass -- seen
 * against a live server started without it, 2026-09-24. The bytes now come
 * through the client and are shown through object URLs this store owns.
 */
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

  // Every blob is named for the attachment it holds, and every object URL for
  // the blob it was made from, so an assertion reads as which image went where.
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
    // ...and on screen as the URL made from its bytes. The attachment URL is
    // what used to be here.
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

/**
 * The window died and came back.
 *
 * There is no way to really reload inside a test, so these drive the seam the
 * reload goes through: a run pointer left in `localStorage` by the last window,
 * and a store that starts empty. What is under test is whether the app can pick
 * up a turn it was never around for.
 */
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
    // A surface decides which screen to open while it mounts, and the round
    // trip below has not happened yet. Landing on the wall and having the turn
    // appear behind it a second later is the failure this change exists to
    // remove, so the pointer alone has to be enough.
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

    // `hasLiveThread` already sent the surface to the thread on the strength of
    // the pointer, so leaving it empty would be worse than the wall it skipped.
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

    // Resumed from where the LAST window had read, not from the beginning:
    // replaying what is already on screen would write the answer out twice.
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

    // Deliberately not deferred to the end of the turn: the whole point is to
    // survive a reload that could happen in the next moment.
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

  /**
   * Measured against a live pond before this was wired: a client that stopped
   * reading at frame 2 had its run finish at frame 664, seventy-three seconds
   * later, and post-turn memory extraction then opened a further provider call
   * on the same single-slot engine. Bumping `runSeq` stops us writing the
   * frames down; it was never what stopped the model.
   */
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

  /**
   * The other three `openSession` callers are recovery, not abandonment:
   * `resumeActiveRun` lays down history before reattaching, and the
   * `replay_gap` / `run_evicted` arm reloads a conversation whose run is still
   * generating. Cancelling by default would have aborted the run each of them
   * exists to recover.
   */
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

  /**
   * The guard on the whole point of `resumable`. Leaving a turn deliberately
   * cancels it; the window going away does not, and nothing here runs on
   * unload — so a closed window still comes back to a finished answer.
   */
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

    // A stop the user asked for must not look like it failed because the
    // network did.
    await expect(abortRun()).resolves.toBeUndefined();
    expect(getChatRun().busy).toBe(false);
  });
});
