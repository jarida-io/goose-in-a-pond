import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, act } from "@testing-library/react";
import { Chat } from "./Chat";
import { api } from "../api/PondApiClient";
import { __resetChatRunForTests, setChatRunBridge } from "../state/chatRunStore";
import type { ChatEvent } from "../api/types";

// ── Mocks ─────────────────────────────────────────────────────────────────────

vi.mock("../api/PondApiClient", () => ({
  api: {
    chatStream: vi.fn(),
    listSessions: vi.fn(),
    getSessionMessages: vi.fn(),
    setToken: vi.fn(),
    getSettings: vi.fn().mockResolvedValue({ show_turn_stats: false, thinking_mode: "auto" }),
    // Terminal state so the WarmupBanner renders nothing and never re-polls.
    getWarmupStatus: vi.fn().mockResolvedValue({
      state: "skipped", reason: "test", model: "", started_unix_ms: 0,
      finished_unix_ms: null, elapsed_ms: 0,
    }),
    updateSettings: vi.fn().mockResolvedValue({}),
    getModelCapabilities: vi.fn().mockResolvedValue({
      thinking: false,
      vision: true,
      audio_input: false,
      context_window_tokens: 8192,
      structured_output: false,
      tool_calling: true,
    }),
    sessionAttachmentUrl: vi.fn((sessionId: string, attachmentId: string) => `/api/v1/sessions/${sessionId}/attachments/${attachmentId}`),
    compactSession: vi.fn(),
    retitleSession: vi.fn(),
    renameSession: vi.fn(),
  },
}));

// `vi.hoisted`: mock factories are lifted above the imports, where a plain `let` would be in its TDZ.
const appState = vi.hoisted(() => ({ sessionId: null as string | null }));

vi.mock("../state/AppContext", () => ({
  useAppState: () => ({
    serverOnline: true,
    sessionToken: "test-token",
    sessionId: appState.sessionId,
  }),
  useAppDispatch: () => vi.fn(),
}));

// ── Helpers ───────────────────────────────────────────────────────────────────

function makeStream(events: ChatEvent[]): AsyncGenerator<ChatEvent> {
  return (async function* () {
    for (const ev of events) yield ev;
  })();
}

beforeEach(() => {
  vi.clearAllMocks();
  appState.sessionId = null;
  vi.mocked(api.listSessions).mockResolvedValue([]);
  vi.mocked(api.getSessionMessages).mockResolvedValue([]);
  // The turn is a module singleton that outlives cleanup(), so reset it; with AppContext mocked
  // wholesale, no provider installs the bridge, so install it here.
  __resetChatRunForTests();
  setChatRunBridge({
    sessionToken: "test-token",
    serverOnline: true,
    onSessionId: vi.fn(),
    onResponseMeta: vi.fn(),
    onContextCard: vi.fn(),
  });
});

afterEach(() => {
  cleanup();
});

// ── Tests ──────────────────────────────────────────────────────────────────────

describe("Chat section", () => {
  it("renders empty state when no messages", async () => {
    render(<Chat />);
    await waitFor(() => {
      // The greeting rotates and is personalised, so assert the empty-state card instead.
      expect(document.querySelector(".chat-empty")).toBeTruthy();
    });
  });

  it("renders New chat button", async () => {
    render(<Chat />);
    await waitFor(() => {
      expect(screen.getByRole("button", { name: /new chat/i })).toBeTruthy();
    });
  });

  it("streams agent text into the agent bubble", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "text", content: "Hello, " },
        { type: "text", content: "world!" },
        { done: true, session_id: "sess-1", type: "done" },
      ]),
    );

    render(<Chat />);

    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    const input = screen.getByLabelText("Message input");
    fireEvent.change(input, { target: { value: "Hi there" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await waitFor(() => {
      expect(screen.getByText("Hello, world!")).toBeTruthy();
    });
  });

  it("shows a friendly status line for tool_call events instead of a raw card", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        {
          type: "tool_call",
          tool: "get_current_weather",
          result: { temperature: 22, description: "Sunny", location: "Nairobi" },
        },
        { type: "text", content: "It's sunny today." },
        { done: true, session_id: "sess-2", type: "done" },
      ]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Weather?" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await waitFor(() => {
      expect(screen.getByText("It's sunny today.")).toBeTruthy();
    });
    // Only the reply text is asserted: inline cards may render by design.
  });

  it("shows error text when error event received", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "error", error: "LLM unavailable" },
      ]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Hello" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await waitFor(() => {
      expect(screen.getByText(/error: llm unavailable/i)).toBeTruthy();
    });
  });

  it("shows error text when error emitted without type field", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      // Backend can emit {"error": "..."} with no type field
      makeStream([{ error: "llamafile request failed" } as ChatEvent]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Hello" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await waitFor(() => {
      expect(screen.getByText(/error: llamafile request failed/i)).toBeTruthy();
    });
  });

  it("does not run text after an error onto the end of the error sentence", async () => {
    // The server keeps streaming after an error frame, and the error arm overwrites while text appends.
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "error", error: "Could not resolve model config: missing provider" },
        { type: "text", content: "I could not produce a response to that." },
      ]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Hello" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await waitFor(() => {
      expect(screen.getByText(/^i could not produce a response to that\.$/i)).toBeTruthy();
    });

    // Per element, not document.body.textContent: flattening reads two correct bubbles as
    // "providerI could not", failing a working fix.
    const errorBubble = screen.getByText(
      /^error: could not resolve model config: missing provider$/i,
    );
    expect(errorBubble.textContent).not.toMatch(/could not produce/i);
  });

  it("New chat button clears messages", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "text", content: "Hi!" },
        { done: true, session_id: "sess-3", type: "done" },
      ]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    // Send a message so there's something to clear
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Hello" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await waitFor(() => expect(screen.getByText("Hi!")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: /new chat/i }));

    await waitFor(() => {
      // The greeting rotates and is personalised, so assert the empty-state card instead.
      expect(document.querySelector(".chat-empty")).toBeTruthy();
      expect(screen.queryByText("Hi!")).toBeNull();
    });
  });

  it("dispatches SET_SESSION_ID from done event", async () => {
    const dispatch = vi.fn();
    vi.doMock("../state/AppContext", () => ({
      useAppState: () => ({ serverOnline: true, sessionToken: "tok", sessionId: null }),
      useAppDispatch: () => dispatch,
    }));

    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "text", content: "Sure!" },
        { done: true, session_id: "new-session-id", model_role: "chat", type: "done" },
      ]),
    );

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Test" } });

    await act(async () => {
      fireEvent.click(screen.getByLabelText("Send message"));
    });

    await waitFor(() => expect(screen.getByText("Sure!")).toBeTruthy());

    expect(vi.mocked(api.chatStream)).toHaveBeenCalledTimes(1);
  });
});

// ── Context-pressure note ─────────────────────────────────────────────────────
// Must render with `show_turn_stats` off (the default) and on the message the frame belongs to.
describe("Chat — context pressure note (PAI-4 P7b)", () => {
  async function streamAndSend(events: ChatEvent[]) {
    vi.mocked(api.chatStream).mockReturnValue(makeStream(events));
    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "Hello" } });
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Send message"));
    });
  }

  /** The frame verbatim as `routes.rs` serialises it in the chat-stream generator. */
  const warningFrame: ChatEvent = {
    type: "context_warning",
    utilization_pct: 82.4,
    turns_remaining: 2,
    avg_growth_rate: 640,
    warning: "Context window 82% full (6750/8192 tokens). ~2 turns remaining.",
  } as unknown as ChatEvent;

  it("renders the note after a context_warning frame followed by done", async () => {
    await streamAndSend([
      { type: "text", content: "The greenhouse fans are on." },
      warningFrame,
      { done: true, session_id: "sess-ctx", type: "done" },
    ]);

    await waitFor(() => {
      expect(
        document.querySelector(".ctx-pressure"),
        "the chat section received context_warning and rendered nothing - the " +
          "server has emitted this frame under a default-true setting since " +
          "before PAI-4, and a frame with no consumer is a feature that does " +
          "not exist",
      ).toBeTruthy();
    });
    expect(screen.getByText(/82% full/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /compact now/i })).toBeTruthy();
  });

  it("enables the control with the session id that arrived on done", async () => {
    // The frame has no session id; on a first turn it comes with the later `done`. The text frame is
    // needed: a bubble with no text, cards or reasoning is suppressed, note and all.
    await streamAndSend([
      { type: "text", content: "The greenhouse fans are on." },
      warningFrame,
      { done: true, session_id: "sess-ctx", type: "done" },
    ]);

    await waitFor(() => expect(document.querySelector(".ctx-pressure")).toBeTruthy());
    expect(
      screen.getByRole("button", { name: /compact now/i }).hasAttribute("disabled"),
      "the Compact now button rendered disabled, so the note is decoration",
    ).toBe(false);
  });

  it("renders no note for a turn that never reported pressure", async () => {
    // Vacuity control for the tests above.
    await streamAndSend([
      { type: "text", content: "The greenhouse fans are on." },
      { done: true, session_id: "sess-ctx", type: "done" },
    ]);

    await waitFor(() => expect(screen.getByText("The greenhouse fans are on.")).toBeTruthy());
    expect(document.querySelector(".ctx-pressure")).toBeNull();
  });
});

// ── Reasoning survives the reload ─────────────────────────────────────────────
// Fresh modules because the top-level AppContext mock pins `sessionId: null`. The id changes after
// mount: the effect skips an id equal to the one it first saw, so mounting with it set loads nothing.
describe("Chat history — persisted reasoning (PAI-5 P6)", () => {
  async function renderWithHistory(messages: unknown[]) {
    vi.resetModules();
    const getSessionMessages = vi.fn().mockResolvedValue(messages);
    // doMock doesn't always win over the top-level mock (~1 run in 5), so point both at the same data.
    vi.mocked(api.getSessionMessages).mockResolvedValue(messages as never);
    const holder = { sessionId: null as string | null };
    vi.doMock("../api/PondApiClient", () => ({
      api: {
        chatStream: vi.fn(),
        listSessions: vi.fn().mockResolvedValue([]),
        getSessionMessages,
        setToken: vi.fn(),
        getSettings: vi.fn().mockResolvedValue({ show_turn_stats: false }),
        getModelCapabilities: vi.fn().mockResolvedValue({
          thinking: true,
          vision: true,
          audio_input: false,
          context_window_tokens: 8192,
          structured_output: false,
          tool_calling: true,
        }),
        sessionAttachmentUrl: vi.fn(() => "/api/v1/sessions/s/attachments/a"),
      },
    }));
    vi.doMock("../state/AppContext", () => ({
      useAppState: () => ({
        serverOnline: true,
        sessionToken: "tok",
        sessionId: holder.sessionId,
      }),
      useAppDispatch: () => vi.fn(),
    }));
    // The fresh Chat binds to this registry's chatRunStore, not the one beforeEach reset; left stale,
    // its "sess-1" from the previous test means the session effect sees nothing to follow.
    const store = await import("../state/chatRunStore");
    store.__resetChatRunForTests();
    const { Chat: FreshChat } = await import("./Chat");
    const { rerender } = render(<FreshChat />);
    // The sidebar-click path: an outside id that differs from the last one triggers the fetch.
    holder.sessionId = "sess-1";
    await act(async () => {
      rerender(<FreshChat />);
    });
    return getSessionMessages;
  }

  const assistantRow = (thinking?: string[]) => ({
    id: "m2",
    session_id: "sess-1",
    role: "assistant",
    content: "The porch light is on.",
    created_at: "2026-08-06T10:00:01Z",
    ...(thinking ? { thinking } : {}),
  });

  const userRow = {
    id: "m1",
    session_id: "sess-1",
    role: "user",
    content: "Is it on?",
    created_at: "2026-08-06T10:00:00Z",
  };

  it("replays stored reasoning into the thinking panel on reload", async () => {
    await renderWithHistory([
      userRow,
      assistantRow(["They said 'it' — probably the thermostat.", "No: the porch light."]),
    ]);

    await waitFor(() => expect(screen.getByText("The porch light is on.")).toBeTruthy());

    // Collapsed until opened. Queried by element: the composer's thinking-mode toggle also says "Thinking".
    const disclosure = document.querySelector(".think");
    expect(disclosure).toBeTruthy();
    // Past tense: a replayed transcript is never "still thinking".
    expect(disclosure!.textContent).toMatch(/thought for/i);

    fireEvent.click(screen.getByRole("button", { expanded: false, name: /thought for/i }));

    // Both passages: a refill that kept only the first block would pass a toggle-only check.
    expect(screen.getByText("They said 'it' — probably the thermostat.")).toBeTruthy();
    expect(screen.getByText("No: the porch light.")).toBeTruthy();
  });

  it("shows no thinking panel for a turn recorded without it", async () => {
    // The default: `persist_thinking` is off, so the server omits the field. Vacuity control for the above.
    await renderWithHistory([userRow, assistantRow()]);

    await waitFor(() => expect(screen.getByText("The porch light is on.")).toBeTruthy());
    expect(document.querySelector(".think")).toBeNull();
  });

  it("shows no thinking panel when the server sends an empty list", async () => {
    await renderWithHistory([userRow, assistantRow([])]);

    await waitFor(() => expect(screen.getByText("The porch light is on.")).toBeTruthy());
    expect(document.querySelector(".think")).toBeNull();
  });
});

// ── Leaving the section and coming back ───────────────────────────────────────

// GuiMode's section `switch` really unmounts Chat, so `unmount()` here is the sidebar press itself.
describe("navigating away mid-turn", () => {
  /** A stream held open, so "while Goose is still answering" is a real state. */
  function heldStream(events: ChatEvent[]) {
    let release!: () => void;
    const held = new Promise<void>((r) => { release = r; });
    const gen = (async function* () {
      for (const ev of events) yield ev;
      await held;
      yield { type: "text", content: " and back." } as ChatEvent;
      yield { done: true, session_id: "sess-nav", type: "done" } as ChatEvent;
    })();
    return { gen, release: () => release() };
  }

  async function sendAndLeave() {
    // Existing conversations, so the section would otherwise open on the wall, not an empty pond's thread.
    vi.mocked(api.listSessions).mockResolvedValue([
      { id: "s-1", title: "an older chat", created_at: "", updated_at: "" },
    ] as never);
    const stream = heldStream([{ type: "text", content: "Still going" }]);
    vi.mocked(api.chatStream).mockReturnValueOnce(stream.gen);

    const mounted = render(<Chat />);
    fireEvent.click(await screen.findByRole("button", { name: "New chat" }));
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "a long one" } });
    fireEvent.click(screen.getByLabelText(/send message|queue message/i));
    await waitFor(() => expect(screen.getByText(/Still going/)).toBeTruthy());

    mounted.unmount();
    return stream;
  }

  it("comes back to the answer that finished while it was away", async () => {
    const stream = await sendAndLeave();

    await act(async () => {
      stream.release();
      await new Promise((r) => setTimeout(r, 0));
    });

    render(<Chat />);
    // The thread, not the wall, with the half that arrived while unmounted.
    await waitFor(() => expect(screen.getByText(/Still going and back\./)).toBeTruthy());
    expect(screen.getByLabelText("Message input")).toBeTruthy();
  });

  it("comes back to a turn that is still running", async () => {
    await sendAndLeave();

    render(<Chat />);
    await waitFor(() => expect(screen.getByText(/Still going/)).toBeTruthy());
    // Still working: the composer says so, and it queues rather than sends.
    expect(
      (screen.getByLabelText("Message input") as HTMLTextAreaElement).placeholder,
    ).toMatch(/Queue a message/);
  });

  it("still opens on the wall when nothing was left running", async () => {
    vi.mocked(api.listSessions).mockResolvedValue([
      { id: "s-1", title: "an older chat", created_at: "", updated_at: "" },
    ] as never);

    render(<Chat />);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Open conversation/ })).toBeTruthy());
    expect(screen.queryByLabelText("Message input")).toBeNull();
  });

  it("goes back to the wall once the finished turn has been read", async () => {
    const stream = await sendAndLeave();
    await act(async () => {
      stream.release();
      await new Promise((r) => setTimeout(r, 0));
    });

    const first = render(<Chat />);
    await waitFor(() => expect(screen.getByText(/Still going and back\./)).toBeTruthy());
    first.unmount();

    vi.mocked(api.listSessions).mockResolvedValue([
      { id: "s-1", title: "an older chat", created_at: "", updated_at: "" },
    ] as never);
    render(<Chat />);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Open conversation/ })).toBeTruthy());
  });
});

// ── Message queuing ───────────────────────────────────────────────────────────

describe("message queuing", () => {
  /** A stream held open until `release()`, so "still answering" is a real state. */
  function heldStream(events: ChatEvent[]) {
    let release!: () => void;
    const held = new Promise<void>((r) => { release = r; });
    const gen = (async function* () {
      for (const ev of events) yield ev;
      await held;
      yield { done: true, session_id: "sess-q", type: "done" } as ChatEvent;
    })();
    return { gen, release: () => release() };
  }

  async function typeAndSend(text: string) {
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: text } });
    fireEvent.click(screen.getByLabelText(/send message|queue message/i));
  }

  it("keeps the composer live while a reply is streaming", async () => {
    const first = heldStream([{ type: "text", content: "thinking…" }]);
    vi.mocked(api.chatStream).mockReturnValueOnce(first.gen);

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());
    await typeAndSend("first");

    await waitFor(() => {
      expect((screen.getByLabelText("Message input") as HTMLTextAreaElement).disabled).toBe(false);
    });

    await act(async () => { first.release(); });
  });

  it("queues a message typed mid-reply and shows it as queued", async () => {
    const first = heldStream([{ type: "text", content: "working" }]);
    vi.mocked(api.chatStream).mockReturnValueOnce(first.gen);

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());
    await typeAndSend("first");
    await waitFor(() => expect(screen.getByText("working")).toBeTruthy());

    await typeAndSend("second");

    await waitFor(() => expect(screen.getByText("Queued")).toBeTruthy());
    expect(vi.mocked(api.chatStream)).toHaveBeenCalledTimes(1);

    await act(async () => { first.release(); });
  });

  it("drains the queue in order once the turn finishes", async () => {
    const first = heldStream([{ type: "text", content: "one" }]);
    vi.mocked(api.chatStream).mockReturnValueOnce(first.gen);

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());
    await typeAndSend("first");
    await waitFor(() => expect(screen.getByText("one")).toBeTruthy());

    await typeAndSend("second");
    await typeAndSend("third");
    await waitFor(() => expect(screen.getAllByText("Queued")).toHaveLength(2));

    vi.mocked(api.chatStream)
      .mockReturnValueOnce(makeStream([{ type: "text", content: "two" }, { done: true, session_id: "s", type: "done" }]))
      .mockReturnValueOnce(makeStream([{ type: "text", content: "three" }, { done: true, session_id: "s", type: "done" }]));

    await act(async () => { first.release(); });

    await waitFor(() => expect(vi.mocked(api.chatStream)).toHaveBeenCalledTimes(3), { timeout: 3000 });
    const sent = vi.mocked(api.chatStream).mock.calls.map((c) => c[0] as string);
    expect(sent).toEqual(["first", "second", "third"]);
    await waitFor(() => expect(screen.queryByText("Queued")).toBeNull());
  });

  it("does not queue an empty message", async () => {
    const first = heldStream([{ type: "text", content: "busy" }]);
    vi.mocked(api.chatStream).mockReturnValueOnce(first.gen);

    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());
    await typeAndSend("first");
    await waitFor(() => expect(screen.getByText("busy")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "   " } });
    fireEvent.click(screen.getByLabelText(/send message|queue message/i));
    expect(screen.queryByText("Queued")).toBeNull();

    await act(async () => { first.release(); });
  });
});

// ── Thinking toggle ───────────────────────────────────────────────────────────

describe("thinking toggle", () => {
  async function renderChat() {
    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());
    return screen.getByRole("switch", { name: /thinking mode/i });
  }

  it("reflects the stored thinking_mode", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ show_turn_stats: false, thinking_mode: "off" } as never);
    const toggle = await renderChat();
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("false"));
    expect(screen.getByText("Off")).toBeTruthy();
  });

  it("persists the new mode to the server", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ show_turn_stats: false, thinking_mode: "auto" } as never);
    const toggle = await renderChat();
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("true"));

    fireEvent.click(toggle);

    // The setting the agent actually reads, not a display preference.
    await waitFor(() =>
      expect(vi.mocked(api.updateSettings)).toHaveBeenCalledWith({ thinking_mode: "off" }));
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("false"));
  });

  it("restores the previous mode rather than collapsing it to auto", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ show_turn_stats: false, thinking_mode: "on" } as never);
    const toggle = await renderChat();
    await waitFor(() => expect(screen.getByText("On")).toBeTruthy());

    fireEvent.click(toggle);
    await waitFor(() => expect(screen.getByText("Off")).toBeTruthy());

    fireEvent.click(toggle);
    await waitFor(() =>
      expect(vi.mocked(api.updateSettings)).toHaveBeenLastCalledWith({ thinking_mode: "on" }));
  });

  it("reverts the control when the save fails", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ show_turn_stats: false, thinking_mode: "auto" } as never);
    vi.mocked(api.updateSettings).mockRejectedValueOnce(new Error("offline"));
    const toggle = await renderChat();
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("true"));

    fireEvent.click(toggle);

    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("true"));
    expect(screen.getByText("Auto")).toBeTruthy();
  });
});

describe("Chat — renaming this conversation", () => {
  const SESSION = { id: "sess-1", title: "so i was wondering whether", created_at: "", updated_at: "" };

  /** Chat opens on the wall, so walk a person's route into the thread. */
  async function openConversation() {
    appState.sessionId = "sess-1";
    vi.mocked(api.listSessions).mockResolvedValue([SESSION] as never);
    render(<Chat />);
    fireEvent.click(await screen.findByRole("button", { name: /Open conversation/ }));
    // The header carries the stored name once the thread is up.
    await screen.findByRole("button", { name: /Rename this conversation/ });
  }

  /** The server renamed it, so every later listing carries the new name. */
  function serverRenames(title: string) {
    vi.mocked(api.listSessions).mockResolvedValue([{ ...SESSION, title }] as never);
    return { session_id: "sess-1", outcome: "retitled", title };
  }

  it("asks the server for a better name and shows the one it gets", async () => {
    const NEW = "Wake word fires twice on the Jetson";
    await openConversation();
    vi.mocked(api.retitleSession).mockImplementation(async () => serverRenames(NEW) as never);

    fireEvent.click(screen.getByRole("button", { name: /Rename this conversation/ }));

    await screen.findByText(NEW);
    expect(vi.mocked(api.retitleSession)).toHaveBeenCalledWith("sess-1");
    // The history list is refetched too, so the panel agrees with the header.
    await waitFor(() => expect(vi.mocked(api.listSessions).mock.calls.length).toBeGreaterThan(1));
  });

  it("cannot be pressed twice while it is working", async () => {
    let release!: (v: unknown) => void;
    vi.mocked(api.retitleSession).mockReturnValue(new Promise((r) => { release = r; }) as never);
    await openConversation();

    fireEvent.click(screen.getByRole("button", { name: /Rename this conversation/ }));

    const busy = await screen.findByRole("button", { name: /Rename this conversation/ });
    await waitFor(() => expect((busy as HTMLButtonElement).disabled).toBe(true));
    fireEvent.click(busy);
    expect(vi.mocked(api.retitleSession)).toHaveBeenCalledTimes(1);

    await act(async () => {
      release(serverRenames("A settled name"));
    });
    await screen.findByText("A settled name");
  });

  it("keeps the old name when the rename fails", async () => {
    vi.mocked(api.retitleSession).mockRejectedValue(new Error("No language model is configured"));
    await openConversation();

    fireEvent.click(screen.getByRole("button", { name: /Rename this conversation/ }));

    // And the button is pressable again rather than stuck on "Renaming".
    await waitFor(() =>
      expect((screen.getByRole("button", { name: /Rename this conversation/ }) as HTMLButtonElement).disabled)
        .toBe(false));
    expect(screen.getByText("so i was wondering whether")).toBeTruthy();
  });

  it("is not offered when no conversation is open", async () => {
    render(<Chat />);
    await screen.findByRole("button", { name: /Rename this conversation/ });
    const button = screen.getByRole("button", { name: /Rename this conversation/ }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });
});

describe("Chat — the header's controls", () => {
  const SESSION = { id: "sess-1", title: "so i was wondering whether", created_at: "", updated_at: "" };

  async function openConversation() {
    appState.sessionId = "sess-1";
    vi.mocked(api.listSessions).mockResolvedValue([SESSION] as never);
    render(<Chat />);
    fireEvent.click(await screen.findByRole("button", { name: /Open conversation/ }));
    await screen.findByRole("button", { name: /Rename this conversation/ });
  }

  it("no longer offers a History dropdown beside New chat", async () => {
    await openConversation();
    expect(screen.queryByRole("button", { name: /history/i })).toBeNull();
    expect(screen.getByRole("button", { name: "New chat" })).toBeTruthy();
    expect(screen.getByRole("button", { name: /All conversations/ })).toBeTruthy();
  });

  // The only route to a user-owned title, the one kind no background pass overwrites.
  it("renames by typing into the title", async () => {
    await openConversation();

    fireEvent.click(screen.getByRole("button", { name: /Rename conversation:/ }));
    const box = screen.getByLabelText("Conversation name") as HTMLInputElement;
    expect(box.value).toBe("so i was wondering whether");

    fireEvent.change(box, { target: { value: "  Jetson deploy notes  " } });
    fireEvent.keyDown(box, { key: "Enter" });

    await waitFor(() =>
      expect(vi.mocked(api.renameSession)).toHaveBeenCalledWith("sess-1", "Jetson deploy notes"));
  });

  it("abandons the edit on Escape", async () => {
    await openConversation();
    fireEvent.click(screen.getByRole("button", { name: /Rename conversation:/ }));
    const box = screen.getByLabelText("Conversation name");

    fireEvent.change(box, { target: { value: "Something else" } });
    fireEvent.keyDown(box, { key: "Escape" });

    expect(screen.queryByLabelText("Conversation name")).toBeNull();
    expect(vi.mocked(api.renameSession)).not.toHaveBeenCalled();
  });

  // Likelier a slip than an instruction, and nothing could undo the lost name.
  it("treats an emptied name as a change of mind, not a rename", async () => {
    await openConversation();
    fireEvent.click(screen.getByRole("button", { name: /Rename conversation:/ }));
    const box = screen.getByLabelText("Conversation name");

    fireEvent.change(box, { target: { value: "   " } });
    fireEvent.blur(box);

    await waitFor(() => expect(screen.queryByLabelText("Conversation name")).toBeNull());
    expect(vi.mocked(api.renameSession)).not.toHaveBeenCalled();
  });

  // The placeholder title is "New Chat", right beside the real New chat button.
  it("does not make the placeholder title a control", async () => {
    render(<Chat />);
    await screen.findByRole("button", { name: "New chat" });
    expect(screen.queryByRole("button", { name: /Rename conversation:/ })).toBeNull();
    expect(screen.getAllByRole("button", { name: /new chat/i })).toHaveLength(1);
  });
});
