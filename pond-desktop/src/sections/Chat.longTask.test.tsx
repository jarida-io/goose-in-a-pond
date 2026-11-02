import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, within } from "@testing-library/react";
import { Chat } from "./Chat";
import { api } from "../api/PondApiClient";
import { __resetChatRunForTests, openSession, setChatRunBridge } from "../state/chatRunStore";
import { assistantRow, userRow } from "../test-support/keptRows";
import { ApiError } from "../api/types";
import type { ChatEvent, SessionMessage, VisionStatus } from "../api/types";
import { doneTask, STARTED_TEXT, TASK_ID, taskOf } from "../longtasks/longTaskFixtures";

vi.mock("../api/PondApiClient", () => ({
  api: {
    chatStream: vi.fn(),
    listSessions: vi.fn(),
    getSessionMessages: vi.fn(),
    setToken: vi.fn(),
    getSettings: vi.fn().mockResolvedValue({ show_turn_stats: false, thinking_mode: "auto" }),
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
    getVisionStatus: vi.fn().mockResolvedValue({
      model: "", state: { kind: "ready", bytes: null }, size_bytes: null, message: null,
    } satisfies VisionStatus),
    getSessionAttachment: vi.fn(),
    compactSession: vi.fn(),
    retitleSession: vi.fn(),
    renameSession: vi.fn(),
    listLongTasks: vi.fn(),
    getLongTask: vi.fn(),
    getLongTaskArtifact: vi.fn(),
    getLongTaskReport: vi.fn(),
    cancelLongTask: vi.fn(),
  },
}));

const appState = vi.hoisted(() => ({ sessionId: null as string | null }));

vi.mock("../state/useAppContext", () => ({
  useAppState: () => ({
    serverOnline: true,
    sessionToken: "test-token",
    sessionId: appState.sessionId,
  }),
  useAppDispatch: () => vi.fn(),
}));

const confirmMock = vi.hoisted(() => vi.fn());
vi.mock("../components/shared", async () => {
  const actual = (await vi.importActual("../components/shared")) as Record<string, unknown>;
  return { ...actual, useConfirm: () => confirmMock };
});

function makeStream(events: ChatEvent[]): AsyncGenerator<ChatEvent> {
  return (async function* () {
    for (const ev of events) yield ev;
  })();
}

const START_TOOL = "giap-tasks__task_start";

/** A turn the prime ends by starting a long task: the stored rows are a tool row, then the reply. */
function historyWithTask(): SessionMessage[] {
  return [
    userRow({ content: "plan me a trip to Lamu" }),
    assistantRow({ id: "asst-0", content: "", tool_calls: [{ id: "call-9", name: START_TOOL, arguments: "{}" }] }),
    {
      id: "tool-9",
      session_id: "sess-1",
      role: "tool",
      created_at: "",
      tool_call_id: "call-9",
      content: JSON.stringify({ tool_call_id: "call-9", tool: START_TOOL, content: STARTED_TEXT }),
    },
    assistantRow({ id: "asst-1", content: "I have started that. It will take a while.", tool_calls: undefined }),
  ];
}

beforeEach(() => {
  vi.clearAllMocks();
  appState.sessionId = null;
  vi.mocked(api.listSessions).mockResolvedValue([]);
  vi.mocked(api.getSessionMessages).mockResolvedValue([]);
  vi.mocked(api.listLongTasks).mockResolvedValue([]);
  vi.mocked(api.getLongTask).mockResolvedValue(taskOf());
  vi.mocked(api.getLongTaskArtifact).mockReset();
  // An older pond has no report route; a test that wants the whole report says so.
  vi.mocked(api.getLongTaskReport).mockReset();
  vi.mocked(api.getLongTaskReport).mockResolvedValue(null);
  vi.mocked(api.cancelLongTask).mockReset();
  confirmMock.mockReset();
  confirmMock.mockResolvedValue(true);
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

describe("Chat, a long task the prime starts", () => {
  it("draws the card where the task_start call was, while the turn is still live", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "tool_call", tool: START_TOOL, id: "call-1", input: { goal: "Plan a trip to Lamu" } },
        { type: "tool_result", tool: START_TOOL, id: "call-1", content: STARTED_TEXT },
        { type: "text", content: "I have started that." },
        { done: true, session_id: "sess-1", type: "done" },
      ]),
    );
    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "plan me a trip to Lamu" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    expect(await screen.findByText("Plan a trip to Lamu")).toBeTruthy();
    expect(screen.getByText("Working: step 2 of 4")).toBeTruthy();
    expect(api.getLongTask).toHaveBeenCalledWith(TASK_ID);
    expect(screen.getByText("I have started that.")).toBeTruthy();
    const card = screen.getByRole("group", { name: /^Long task:/ });
    expect(card.closest(".ch-bubble-wrap")?.textContent).toContain("I have started that.");
  });

  it("ignores the same words from a tool that is not task_start", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "tool_call", tool: "giap-notes__read_note", id: "call-1", input: {} },
        { type: "tool_result", tool: "giap-notes__read_note", id: "call-1", content: STARTED_TEXT },
        { type: "text", content: "Here is what I found." },
        { done: true, session_id: "sess-1", type: "done" },
      ]),
    );
    render(<Chat />);
    await waitFor(() => expect(screen.getByLabelText("Message input")).toBeTruthy());
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "search" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    await screen.findByText("Here is what I found.");
    expect(screen.queryByRole("group", { name: /^Long task:/ })).toBeNull();
    expect(api.getLongTask).not.toHaveBeenCalled();
  });

  it("shows the card again when the conversation is reopened, from the tool row it stored", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue(historyWithTask());
    await openSession("sess-1");
    render(<Chat />);

    expect(await screen.findByText("Plan a trip to Lamu")).toBeTruthy();
    expect(api.listLongTasks).toHaveBeenCalledWith("sess-1");
    const card = screen.getByRole("group", { name: /^Long task:/ });
    expect(card.closest(".ch-bubble-wrap")?.textContent).toContain("I have started that. It will take a while.");
    expect(screen.queryByRole("region", { name: "Long tasks in this conversation" })).toBeNull();
  });

  it("draws one card, not two, when the list and a message both point at the task", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue(historyWithTask());
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf()]);
    await openSession("sess-1");
    render(<Chat />);

    await screen.findByText("Plan a trip to Lamu");
    expect(screen.getAllByText("Plan a trip to Lamu")).toHaveLength(1);
  });

  it("lists a task no message points at under its own label", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([userRow(), assistantRow({ tool_calls: undefined })]);
    vi.mocked(api.listLongTasks).mockResolvedValue([doneTask()]);
    await openSession("sess-1");
    render(<Chat />);

    const group = await screen.findByRole("region", { name: "Long tasks in this conversation" });
    expect(group.textContent).toContain("Plan a trip to Lamu");
    expect(group.textContent).toContain("Done");
  });

  it("shows nothing on a pond that has no long tasks", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue(historyWithTask());
    vi.mocked(api.listLongTasks).mockRejectedValue(new ApiError(404, "Not Found"));
    await openSession("sess-1");
    render(<Chat />);

    await screen.findByText("I have started that. It will take a while.");
    await waitFor(() => expect(screen.queryByText("Checking on this task...")).toBeNull());
    expect(api.listLongTasks).toHaveBeenCalled();
    expect(screen.queryByRole("group", { name: /^Long task:/ })).toBeNull();
    expect(screen.queryByRole("region", { name: "Long tasks in this conversation" })).toBeNull();
    expect(api.getLongTask).not.toHaveBeenCalled();
  });

  it("reads the result of a finished task in the chat, as markdown", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue(historyWithTask());
    vi.mocked(api.getLongTask).mockResolvedValue(doneTask());
    vi.mocked(api.getLongTaskArtifact).mockResolvedValue({
      id: "art-1",
      title: "Trip plan",
      kind: "result",
      summary: "",
      body: "- ferry at **7:00**",
    });
    await openSession("sess-1");
    render(<Chat />);

    expect(await screen.findByText("Three days in Lamu, with ferry times.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Read the result" }));
    const strong = await screen.findByText("7:00");
    expect(strong.tagName).toBe("STRONG");
    expect(api.getLongTaskArtifact).toHaveBeenCalledWith(TASK_ID, "art-1");
  });

  it("opens the whole report on its own sheet when the pond has one", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue(historyWithTask());
    vi.mocked(api.getLongTask).mockResolvedValue(doneTask());
    vi.mocked(api.getLongTaskReport).mockResolvedValue({
      id: TASK_ID,
      title: "Trip plan",
      state: "completed",
      kind: "general",
      markdown: "# Trip plan\n\nThree days.\n\n## Ferry\n\nLeaves at **7:00**.\n\n## Sources\n\n- [Ferry co](https://example.com/ferry)\n",
      sections: 1,
      sources: [{ title: "Ferry co", url: "https://example.com/ferry" }],
      calendar_events: 0,
    });
    await openSession("sess-1");
    render(<Chat />);

    await screen.findByText("Three days in Lamu, with ferry times.");
    fireEvent.click(screen.getByRole("button", { name: "Read the result" }));
    const dialog = await screen.findByRole("dialog", { name: "Trip plan" });
    expect(within(dialog).getByRole("heading", { name: "Ferry" })).toBeTruthy();
    expect(within(dialog).getByText("7:00").tagName).toBe("STRONG");
    expect(within(dialog).getByRole("link", { name: "Ferry co" }).getAttribute("href")).toBe("https://example.com/ferry");
    expect(api.getLongTaskReport).toHaveBeenCalledWith(TASK_ID);
    expect(api.getLongTaskArtifact).not.toHaveBeenCalled();
  });

  it("stops a running task after asking, and shows how it ended", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue(historyWithTask());
    vi.mocked(api.cancelLongTask).mockResolvedValue(undefined);
    await openSession("sess-1");
    render(<Chat />);

    await screen.findByText("Plan a trip to Lamu");
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf({ state: "cancelled" }));
    fireEvent.click(screen.getByRole("button", { name: /^Stop the task/ }));

    await screen.findByText("Stopped");
    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(api.cancelLongTask).toHaveBeenCalledWith(TASK_ID);
    expect(screen.queryByRole("button", { name: /^Stop the task/ })).toBeNull();
  });
});
