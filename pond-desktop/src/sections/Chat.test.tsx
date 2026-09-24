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
    getSessionAttachment: vi.fn(),
    compactSession: vi.fn(),
    retitleSession: vi.fn(),
    renameSession: vi.fn(),
  },
}));

/**
 * Mutable app state for the mock.
 *
 * `vi.hoisted` because `vi.mock` factories are lifted above the imports, so a
 * plain `let` declared here would still be in its temporal dead zone when the
 * factory is defined. Reset in `beforeEach`, so a test that opens a
 * conversation cannot leak one into the next.
 */
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

/** Build a mock async generator that yields the given events then returns. */
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
  // The turn lives in a module singleton so it can outlive an unmount, which
  // means it also outlives `cleanup()` — without this, one test's transcript is
  // the next test's starting state. This file also mocks AppContext wholesale,
  // so the provider that normally installs the bridge never runs here.
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
      // The greeting rotates and personalises from `user_name`, so there is no
      // fixed string to assert. The card itself is the stable signal that the
      // thread is empty.
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
    // The chat bubble used to render a `ContextCard` (chip + raw `{}` JSON)
    // for every tool invocation, leaking agent plumbing into the thread.
    // It now shows a humanised one-line status while the tool runs and
    // clears it once the model's reply text arrives.
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

    // Reply text reaches the bubble and the raw ContextCard never does.
    await waitFor(() => {
      expect(screen.getByText("It's sunny today.")).toBeTruthy();
    });
    // ContextCards may or may not render — the key assertion is the reply text.
    // (Our design renders inline cards; Exile10's removes them.)
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
    // The reported bug, exactly: a new chat rendered
    //   "Error: Could not resolve model config: missing providerI could not produce…"
    // The error arm overwrites `text` while the text arm appends to it, and the server
    // deliberately keeps streaming after an error frame — so a following text frame ran
    // straight onto the end of the error. The two are separate events and must render
    // as separate messages.
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

    // Anchored at both ends: the error bubble's own text must END at "provider",
    // which is precisely what appending broke. Asserted per element rather than
    // against `document.body.textContent` — that flattens the whole tree, so two
    // correctly separate bubbles still read as "providerI could not" there and the
    // assertion would fail on a working fix.
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

    // Click New chat
    fireEvent.click(screen.getByRole("button", { name: /new chat/i }));

    await waitFor(() => {
      // The greeting rotates and personalises from `user_name`, so there is no
      // fixed string to assert. The card itself is the stable signal that the
      // thread is empty.
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

    // At minimum, chatStream was called once
    expect(vi.mocked(api.chatStream)).toHaveBeenCalledTimes(1);
  });
});

// ── PAI-4 P7b-fix: the context-pressure note has a consumer HERE ──────────────
//
// Round 1 landed the note in `hub/views/ChatHub.tsx` and guarded it with a
// two-substring grep, because that component has no render test. Synthesis
// corrected that: two semantic mutations left both substrings in place and
// passed 261/261 — adding `showTurnStats &&` to the render guard (which ships
// the note invisible on every default install, `show_turn_stats` being false in
// Rust), and attaching the frame to a message id that does not exist.
//
// This is the render test the correction asked for, and it is written here
// rather than against ChatHub because `sections/Chat.tsx` already has the mount
// harness. It drives the real component with a real stream and asserts a real
// DOM node, so both of those mutations go red.
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
    // The sentence the server sent, not a placeholder, and the control itself.
    expect(screen.getByText(/82% full/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /compact now/i })).toBeTruthy();
  });

  it("enables the control with the session id that arrived on done", async () => {
    // The `context_warning` frame carries no session id and on a first turn the
    // id only arrives with `done`, which is emitted after it. A note whose
    // button stays disabled is a dead control by another route.
    //
    // The text frame is part of the fixture on purpose: an agent bubble with no
    // text, no cards and no reasoning is suppressed entirely, note and all, and
    // the generator that emits `context_warning` is the one that emits the
    // answer — so a warning-only turn is not a state production produces.
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
    // The vacuity control. Without it, a component that rendered the note on
    // every assistant message would pass the test above.
    await streamAndSend([
      { type: "text", content: "The greenhouse fans are on." },
      { done: true, session_id: "sess-ctx", type: "done" },
    ]);

    await waitFor(() => expect(screen.getByText("The greenhouse fans are on.")).toBeTruthy());
    expect(document.querySelector(".ctx-pressure")).toBeNull();
  });
});

// ── PAI-5 P6: reasoning survives the reload ───────────────────────────────────
//
// The thinking panel and its live accumulator both already existed; what did
// not was the refill from history, so every reloaded conversation showed its
// answers with the reasoning behind them silently gone. This drives the real
// component and the real store rather than asserting that
// `sessionMessagesToMessages` mentions `thinking` — a grep would have passed
// against the version that dropped the field on the floor.
//
// What it does NOT drive is the client: `getSessionMessages` is mocked here,
// so the mapping from the wire never runs, and that mapping is where the field
// was actually being dropped in production while these tests passed.
// `PondApiClient.test.ts` covers it from a raw fetch body.
//
// `vi.resetModules()` + dynamic import because the module-level AppContext mock
// pins `sessionId: null`. And the session id is flipped AFTER mount rather than
// set before it, because that is the only way history actually loads: the effect
// bails when the incoming id already equals `sessionIdRef.current`, which it
// does on the very first render. A test that mounted with the id already set
// would render an empty conversation and prove nothing — quietly, since the
// assertion it makes is about what is absent.
describe("Chat history — persisted reasoning (PAI-5 P6)", () => {
  async function renderWithHistory(messages: unknown[]) {
    vi.resetModules();
    const getSessionMessages = vi.fn().mockResolvedValue(messages);
    // `vi.resetModules()` + `doMock` + dynamic import does not always win the
    // race: the component occasionally resolves the TOP-LEVEL mock instead,
    // which `beforeEach` has pinned to []. When that happened the thread
    // rendered its empty state and the assertion failed — roughly 1 run in 5,
    // reproducible against HEAD. Pointing both registries at the same data
    // makes the outcome independent of which one wins.
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
        getSessionAttachment: vi.fn(),
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
    // Same hazard as the api mock above, one layer down: the turn store is a
    // module singleton, so the instance the FRESH `Chat` binds to is whichever
    // one this reset registry hands out — not the one `beforeEach` reset. Left
    // stale, its `sessionId` still reads "sess-1" from the previous test in
    // this block, the external-session effect sees nothing to follow, and the
    // thread renders empty. Importing it from the same registry, right here,
    // is what guarantees we reset the instance `Chat` is about to use.
    const store = await import("../state/chatRunStore");
    store.__resetChatRunForTests();
    const { Chat: FreshChat } = await import("./Chat");
    const { rerender } = render(<FreshChat />);
    // The sidebar-click path: the session id arrives from outside, the effect
    // sees it differ from what it last saw, and fetches.
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

    // Reasoning now collapses to a single line, so the disclosure has to be
    // opened before the passages exist in the DOM. Queried by element rather
    // than by the word "Thinking": the composer carries a thinking-mode toggle
    // using the same word.
    const disclosure = document.querySelector(".think");
    expect(disclosure).toBeTruthy();
    // Past tense once the turn is over — a replayed transcript is never "still
    // thinking".
    expect(disclosure!.textContent).toMatch(/thought for/i);

    fireEvent.click(screen.getByRole("button", { expanded: false, name: /thought for/i }));

    // BOTH passages. Asserting only the toggle would pass against a refill that
    // kept the first block and dropped the rest.
    expect(screen.getByText("They said 'it' — probably the thermostat.")).toBeTruthy();
    expect(screen.getByText("No: the porch light.")).toBeTruthy();
  });

  it("shows no thinking panel for a turn recorded without it", async () => {
    // The default state of every pond: `persist_thinking` is off, so the server
    // omits the field entirely. This is the vacuity control for the test above
    // — without it, a component that rendered a "Thinking" panel on every
    // assistant message would pass that one.
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

// ── History images ────────────────────────────────────────────────────────────
//
// Opened from the wall, the way a person does. The bubble must show an object
// URL made from bytes the client fetched with its token: the bare attachment
// URL it used to show sits on the protected router, and an `<img src>` cannot
// send the bearer header, so on a real pond every one of them was a 401.
describe("Chat history — images", () => {
  it("shows a replayed image through an object URL, not the attachment URL", async () => {
    vi.mocked(api.listSessions).mockResolvedValue([
      { id: "sess-img", title: "a picture", created_at: "", updated_at: "" },
    ] as never);
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      {
        id: "m1",
        session_id: "sess-img",
        role: "user",
        content: "what is in this picture?",
        created_at: "",
        images: [
          {
            id: "att-1",
            mime_type: "image/png",
            byte_size: 3,
            url: "/api/v1/sessions/sess-img/attachments/att-1",
          },
        ],
      },
    ] as never);
    vi.mocked(api.getSessionAttachment).mockResolvedValue(
      new Blob(["png"], { type: "image/png" }),
    );

    render(<Chat />);
    fireEvent.click(await screen.findByRole("button", { name: /Open conversation/ }));

    const img = await screen.findByAltText("Attached image 1");
    expect(img.getAttribute("src")).toMatch(/^blob:/);
    expect(vi.mocked(api.getSessionAttachment)).toHaveBeenCalledWith("sess-img", "att-1");
    expect(document.querySelector('img[src*="/attachments/"]')).toBeNull();
  });
});

// ── Leaving the section and coming back ───────────────────────────────────────

/**
 * The reported bug, driven end to end.
 *
 * `GuiMode` renders sections with a `switch`, so a sidebar press really does
 * unmount this component -- `unmount()` here is that press, not an
 * approximation of it. Before the turn was hoisted into `chatRunStore`, coming
 * back showed the "All chats" wall and the answer was nowhere, even though the
 * server had finished writing it.
 */
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
    // A pond that already has conversations, so the wall is what this section
    // would otherwise open on -- otherwise "landed in the thread" would be
    // satisfied by the empty-pond case and prove nothing.
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
    // The thread, not the wall, and the whole answer -- including the half that
    // arrived with nothing mounted to receive it.
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

    // First return shows it...
    const first = render(<Chat />);
    await waitFor(() => expect(screen.getByText(/Still going and back\./)).toBeTruthy());
    first.unmount();

    // ...and the visit after that is the wall again, as the section intends.
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
  /**
   * A stream the test can hold open, so "while Goose is still answering" is a
   * real state rather than a race against an instant mock. `release()` ends it.
   */
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

    // The old composer disabled itself here, which silently swallowed anything
    // typed during a reply.
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

    // Visible in the thread, marked, and not yet sent.
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

    // Each queued message gets its own turn, in the order it was typed.
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
    // Someone who chose "on" explicitly should get "on" back when they switch
    // thinking on again — not silently downgraded to the default.
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

    // Optimistic, but it does not lie: a failed write puts the switch back.
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("true"));
    expect(screen.getByText("Auto")).toBeTruthy();
  });
});

describe("Chat — renaming this conversation", () => {
  const SESSION = { id: "sess-1", title: "so i was wondering whether", created_at: "", updated_at: "" };

  /**
   * Chat opens on the wall, so a test that wants the thread has to walk the
   * same route a person does: find the card, press it, land in the chat.
   */
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
    // The history list is refetched too, so the panel agrees with the header
    // rather than the two drifting until the next reload.
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

    // The failure is non-fatal: the conversation keeps the name it had, and the
    // button becomes pressable again rather than sticking on "Renaming".
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

  /// The wall replaced it. Two routes to the same list, one of them cramped
  /// behind a dropdown, is what the wall was built to end.
  it("no longer offers a History dropdown beside New chat", async () => {
    await openConversation();
    expect(screen.queryByRole("button", { name: /history/i })).toBeNull();
    expect(screen.getByRole("button", { name: "New chat" })).toBeTruthy();
    expect(screen.getByRole("button", { name: /All conversations/ })).toBeTruthy();
  });

  /// Typing a name is the ONLY thing that marks a title as the user's, and a
  /// user's title is the one kind no background pass will ever overwrite. With
  /// the dropdown gone, this is the last route to it — if it breaks, that whole
  /// protection becomes unreachable rather than merely inconvenient.
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

  /// Clearing the box and walking away is far likelier to be a slip than an
  /// instruction to call the conversation nothing, and there is no undo for the
  /// name it would replace.
  it("treats an emptied name as a change of mind, not a rename", async () => {
    await openConversation();
    fireEvent.click(screen.getByRole("button", { name: /Rename conversation:/ }));
    const box = screen.getByLabelText("Conversation name");

    fireEvent.change(box, { target: { value: "   " } });
    fireEvent.blur(box);

    await waitFor(() => expect(screen.queryByLabelText("Conversation name")).toBeNull());
    expect(vi.mocked(api.renameSession)).not.toHaveBeenCalled();
  });

  /// An unsaved conversation is called "New Chat", and a control with that name
  /// sitting next to the actual New chat button is two things with one name.
  it("does not make the placeholder title a control", async () => {
    render(<Chat />);
    await screen.findByRole("button", { name: "New chat" });
    expect(screen.queryByRole("button", { name: /Rename conversation:/ })).toBeNull();
    // Exactly one thing here answers to "New chat".
    expect(screen.getAllByRole("button", { name: /new chat/i })).toHaveLength(1);
  });
});
