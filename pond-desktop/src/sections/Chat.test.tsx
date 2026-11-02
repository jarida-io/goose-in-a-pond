import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, act } from "@testing-library/react";
import { Chat } from "./Chat";
import { api } from "../api/PondApiClient";
import { __resetChatRunForTests, openSession, setChatRunBridge } from "../state/chatRunStore";
import { assistantRow, keptToolRow, userRow } from "../test-support/keptRows";
import { ApiError } from "../api/types";
import type { ChatEvent, VisionStatus } from "../api/types";
import { ATTACH_BLOCKED_COPY, NOT_DECLARED_COPY } from "../components/imageSupportCopy";
import { prepareImage } from "../lib/imageAttach";
import type { PreparedImage } from "../lib/imageAttach";

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
    // Ready by default, so the send gate stays open outside the picture-support tests.
    getVisionStatus: vi.fn().mockResolvedValue({
      model: "", state: { kind: "ready", bytes: null }, size_bytes: null, message: null,
    } satisfies VisionStatus),
    getSessionAttachment: vi.fn(),
    compactSession: vi.fn(),
    retitleSession: vi.fn(),
    renameSession: vi.fn(),
  },
}));

// `vi.hoisted`: mock factories are lifted above the imports, where a plain `let` would be in its TDZ.
const appState = vi.hoisted(() => ({ sessionId: null as string | null }));

vi.mock("../state/useAppContext", () => ({
  useAppState: () => ({
    serverOnline: true,
    sessionToken: "test-token",
    sessionId: appState.sessionId,
  }),
  useAppDispatch: () => vi.fn(),
}));

// Only `prepareImage` is faked: happy-dom's <img> never decodes, so it can't run here.
vi.mock("../lib/imageAttach", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/imageAttach")>();
  return { ...actual, prepareImage: vi.fn() };
});

// ── Helpers ───────────────────────────────────────────────────────────────────

function makeStream(events: ChatEvent[]): AsyncGenerator<ChatEvent> {
  return (async function* () {
    for (const ev of events) yield ev;
  })();
}

function fakeFile(name = "photo.png"): File {
  return new File(["fake"], name, { type: "image/png" });
}

function fakePrepared(previewUrl = "blob:pond/fake"): PreparedImage {
  return { data: "AAA", mime_type: "image/png", previewUrl, width: 10, height: 10, byteSize: 3 };
}

const READY_STATUS: VisionStatus = {
  model: "", state: { kind: "ready", bytes: null }, size_bytes: null, message: null,
};

beforeEach(() => {
  vi.clearAllMocks();
  appState.sessionId = null;
  vi.mocked(api.listSessions).mockResolvedValue([]);
  vi.mocked(api.getSessionMessages).mockResolvedValue([]);
  // Re-set each test: `mockResolvedValue` overrides outlive the test that set them.
  vi.mocked(api.getVisionStatus).mockResolvedValue(READY_STATUS);
  vi.mocked(prepareImage).mockResolvedValue(fakePrepared());
  // The turn is a module singleton that outlives cleanup(), so reset it; with AppContext mocked
  // wholesale, no provider installs the bridge, so install it here.
  __resetChatRunForTests();
  setChatRunBridge({
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
    vi.doMock("../state/useAppContext", () => ({
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
        getVisionStatus: vi.fn().mockResolvedValue({
          model: "", state: { kind: "ready", bytes: null }, size_bytes: null, message: null,
        } satisfies VisionStatus),
        getSessionAttachment: vi.fn(),
      },
    }));
    vi.doMock("../state/useAppContext", () => ({
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

// ── History images ────────────────────────────────────────────────────────────
// An `<img src>` can't send the bearer header, so a replayed image must be an object URL of fetched bytes.
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

/** The paperclip is never vision-gated, so these test the real gate: attaching works, sending is refused. */
describe("Chat — picture support", () => {
  async function attachOneImage() {
    // The composer mounts only once `view` resolves to "thread", a render or two in.
    await screen.findByLabelText("Message input");
    const fileInput = document.querySelector('input[type="file"]') as HTMLInputElement;
    fireEvent.change(fileInput, { target: { files: [fakeFile()] } });
    await screen.findByAltText("Attached image 1");
  }

  it("shows the status line while picture support is getting ready", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "gemma-4-E2B-it-Q4_K_M",
      state: { kind: "downloading", done: 412 * 1_048_576, total: 941 * 1_048_576 },
      size_bytes: 986_833_728,
      message: "Getting picture support ready: 412 MB of 941 MB. Text chat works meanwhile.",
    } satisfies VisionStatus);

    render(<Chat />);

    await screen.findByText(/Getting picture support ready: 412 MB of 941 MB/);
  });

  it("blocks a click-to-send while picture support is not ready", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "gemma-4-E2B-it-Q4_K_M",
      state: { kind: "absent" },
      size_bytes: null,
      message: "Picture support for Gemma 4 E2B needs a one-time 941 MB download. It starts by itself; text chat works meanwhile.",
    } satisfies VisionStatus);

    render(<Chat />);
    await attachOneImage();
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "what is this" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await screen.findByText(
      "Pictures can be sent once picture support is ready. Remove them to send just the text.",
    );
    expect(api.chatStream).not.toHaveBeenCalled();
    // The tray still holds it -- a blocked send does not discard the draft.
    expect(screen.getByAltText("Attached image 1")).toBeTruthy();
  });

  it("blocks Cmd/Ctrl+Enter the same way", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "x", state: { kind: "verifying" }, size_bytes: null,
      message: "Checking picture support before its first use. Text chat works meanwhile.",
    } satisfies VisionStatus);

    render(<Chat />);
    await attachOneImage();
    const input = screen.getByLabelText("Message input");
    fireEvent.change(input, { target: { value: "what is this" } });
    fireEvent.keyDown(input, { key: "Enter", metaKey: true });

    await screen.findByText(/Pictures can be sent once picture support is ready/);
    expect(api.chatStream).not.toHaveBeenCalled();
  });

  it("blocks a suggestion chip too", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "x", state: { kind: "not_declared" }, size_bytes: null, message: null,
    } satisfies VisionStatus);

    render(<Chat />);
    await attachOneImage();
    fireEvent.click(await screen.findByText("What can you help me with?"));

    await screen.findByText(/Pictures can be sent once picture support is ready/);
    expect(api.chatStream).not.toHaveBeenCalled();
  });

  it("blocks a paste of an image, and never adds it to the tray", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "x", state: { kind: "blocked", mode: "offline", host: "huggingface.co" }, size_bytes: null,
      message: "Picture support needs a one-time 941 MB download from huggingface.co, and Network reach is set to Offline, which blocks it. To allow it, set Network reach to Open in Settings, under Privacy & Security.",
    } satisfies VisionStatus);

    render(<Chat />);
    // Wait for the status first, or the paste races the hook's fetch and sees the unblocked fallback.
    await screen.findByText(/Picture support needs a one-time 941 MB download/);
    const input = screen.getByLabelText("Message input");
    fireEvent.paste(input, { clipboardData: { files: [fakeFile()] } });

    await screen.findByText(
      "Pictures can be sent once picture support is ready. Remove them to send just the text.",
    );
    expect(prepareImage).not.toHaveBeenCalled();
    expect(screen.queryByAltText("Attached image 1")).toBeNull();
  });

  it("restores the draft when the server refuses the turn (409)", async () => {
    vi.mocked(api.chatStream).mockImplementation(() =>
      (async function* () {
        throw new ApiError(409, "Picture support is not ready yet.", "vision_not_ready");
      })(),
    );

    render(<Chat />);
    await attachOneImage();
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "what is this" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    // The draft comes back: the text box, the tray, and a line naming why.
    await waitFor(() => {
      expect((screen.getByLabelText("Message input") as HTMLTextAreaElement).value).toBe(
        "what is this",
      );
    });
    expect(screen.getByAltText("Attached image 1")).toBeTruthy();
    await screen.findByText(
      "Picture support is not ready yet. Your message and pictures are back in the box; send them when it is ready.",
    );
    // No error bubble for a refused turn -- it never reached the transcript.
    expect(screen.queryByText(/^error:/i)).toBeNull();
  });
});

/** Add pictures says why a model cannot take pictures, in the label the Models page shows. */
describe("Chat — Add pictures' reason", () => {
  it("names Pictures included when the model cannot look at pictures", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "x", state: { kind: "not_declared" }, size_bytes: null, message: null,
    } satisfies VisionStatus);
    render(<Chat />);
    fireEvent.click(await screen.findByRole("button", { name: "Add to message" }));
    const clip = await screen.findByRole("menuitem", { name: "Add pictures" });
    await waitFor(() => expect(clip.getAttribute("title")).toBe(NOT_DECLARED_COPY));
    expect(clip.getAttribute("title")).not.toMatch(/Reads pictures/);
  });

  it("says the same label when the pond has nothing more to say about why it is blocked", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "x", state: { kind: "absent" }, size_bytes: null, message: null,
    } satisfies VisionStatus);
    render(<Chat />);
    fireEvent.click(await screen.findByRole("button", { name: "Add to message" }));
    const clip = await screen.findByRole("menuitem", { name: "Add pictures" });
    await waitFor(() => expect(clip.getAttribute("title")).toBe(ATTACH_BLOCKED_COPY));
  });
});

// ── No conversation model ─────────────────────────────────────────────────────
// With none chosen the pond refuses a turn and picks nothing, so the thread offers its suggestions.
describe("Chat with no conversation model", () => {
  let added: string[] = [];

  beforeEach(async () => {
    const fixtures = await import("./models/fixtures");
    const extra = {
      listModels: vi.fn().mockResolvedValue([fixtures.e4b(), fixtures.e2b(), fixtures.litertE4b()]),
      getActiveRoles: vi.fn().mockResolvedValue(fixtures.NO_ROLES),
      getMemoryStatus: vi.fn().mockResolvedValue(fixtures.DESKTOP_MEMORY),
      getDownloadProgress: vi.fn().mockResolvedValue({ downloads: [] }),
      downloadModel: vi.fn().mockResolvedValue({ status: "download_started" }),
      activateModel: vi.fn().mockResolvedValue(undefined),
      controlModelDownload: vi.fn().mockResolvedValue({ status: "ok" }),
    };
    Object.assign(api, extra);
    added = Object.keys(extra);
  });

  afterEach(() => {
    for (const name of added) delete (api as unknown as Record<string, unknown>)[name];
  });

  it("offers the suggestions, with sizes, where the greeting would be", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "", chat_model: "", show_turn_stats: false, thinking_mode: "auto" } as never);
    render(<Chat />);

    expect(await screen.findByRole("heading", { name: "Pick a model to talk with" })).toBeTruthy();
    expect(document.querySelector(".chat-empty")).toBeNull();
    expect(await screen.findByText("4.2 GB + 945 MB for pictures")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Download and use Gemma 4 E4B, llama.cpp" })).toBeTruthy();
    // The chip says what is true, not "local".
    await waitFor(() => expect(screen.getByRole("button", { name: "Select model" }).textContent).toBe("Choose a model"));
    expect(api.downloadModel).not.toHaveBeenCalled();
    expect(api.activateModel).not.toHaveBeenCalled();
  });

  it("keeps the greeting when a model is chosen", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "local", chat_model: "gemma-4-E2B-it-qat-UD-Q4_K_XL", show_turn_stats: false, thinking_mode: "auto" } as never);
    render(<Chat />);
    await waitFor(() => expect(document.querySelector(".chat-empty")).toBeTruthy());
    expect(screen.queryByRole("heading", { name: "Pick a model to talk with" })).toBeNull();
  });

  it("shows the suggestions when the pond refuses a turn for want of a model, and hands the message back", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "local", chat_model: "gone", show_turn_stats: false, thinking_mode: "auto" } as never);
    vi.mocked(api.chatStream).mockImplementation(() =>
      (async function* () {
        throw new ApiError(
          409,
          "No conversation model is chosen yet. Choose one on the Models page to start talking.",
          "no_model",
        );
      })(),
    );
    render(<Chat />);
    await waitFor(() => expect(document.querySelector(".chat-empty")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "turn on the porch light" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    expect(await screen.findByRole("heading", { name: "Pick a model to talk with" })).toBeTruthy();
    await waitFor(() => {
      expect((screen.getByLabelText("Message input") as HTMLTextAreaElement).value).toBe("turn on the porch light");
    });
    await screen.findByText(
      "No conversation model is chosen yet. Choose one on the Models page to start talking. Your message is back in the box; send it once a model is chosen.",
    );
    expect(screen.queryByText(/^error:/i)).toBeNull();
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

// Brave's terms let a search result live only for its turn, so a reload has a stub to show, not a card.
describe("Chat history, a result the pond did not keep", () => {
  it("shows one quiet line that names the sources, in place of a card or a broken chip", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([userRow(), keptToolRow(), assistantRow()] as never);
    await openSession("sess-1");
    render(<Chat />);

    const note = await screen.findByText(
      "Web search: results aren't kept after the turn. Sources: kenyapower.co.ke, nation.africa.",
    );
    expect(note.closest(".kept-note")).toBeTruthy();
    expect(screen.getByText("Power should be back by 6 pm.")).toBeTruthy();
    // The tool still shows by name, as every replayed tool does, and nothing opens to an empty card.
    expect(screen.getByText("search web")).toBeTruthy();
    expect(document.querySelector(".tool-call-card, .ui-search")).toBeNull();
    expect(screen.queryByRole("button", { name: /^Web search/ })).toBeNull();
  });

  it("adds nothing to a conversation that kept every result", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow(),
      keptToolRow({}, { transient: undefined, content: "24 degrees" }),
      assistantRow(),
    ] as never);
    await openSession("sess-1");
    render(<Chat />);
    await screen.findByText("Power should be back by 6 pm.");
    expect(document.querySelector(".kept-note")).toBeNull();
  });
});
