import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent, waitFor, within } from "@testing-library/react";
import { ChatHubView } from "./ChatHub";
import { api } from "../../api/PondApiClient";
import { ApiError } from "../../api/types";
import type { ChatEvent, SessionMessage, VisionStatus } from "../../api/types";
import { __resetChatRunForTests, openSession } from "../../state/chatRunStore";
import { assistantRow, userRow } from "../../test-support/keptRows";
import { doneTask, STARTED_TEXT, TASK_ID, taskOf } from "../../longtasks/longTaskFixtures";

vi.mock("../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    getModelCapabilities: vi.fn().mockResolvedValue({
      thinking: false,
      vision: true,
      audio_input: false,
      context_window_tokens: 8192,
      structured_output: false,
      tool_calling: true,
    }),
    getWarmupStatus: vi.fn().mockResolvedValue({
      state: "skipped", reason: "test", model: "", started_unix_ms: 0,
      finished_unix_ms: null, elapsed_ms: 0,
    }),
    getVisionStatus: vi.fn().mockResolvedValue({
      model: "", state: { kind: "ready", bytes: null }, size_bytes: null, message: null,
    } satisfies VisionStatus),
    listSuggestions: vi.fn().mockResolvedValue({ suggestions: [] }),
    getSessionMessages: vi.fn(),
    getSessionAttachment: vi.fn(),
    chatStream: vi.fn(),
    setToken: vi.fn(),
    listLongTasks: vi.fn(),
    getLongTask: vi.fn(),
    getLongTaskArtifact: vi.fn(),
    getLongTaskReport: vi.fn(),
    cancelLongTask: vi.fn(),
  },
}));

vi.mock("../../state/useAppContext", () => ({
  useAppState: () => ({ serverOnline: true, sessionId: "sess-hub" }),
  useAppDispatch: () => vi.fn(),
}));

const confirmMock = vi.hoisted(() => vi.fn());
vi.mock("../../components/shared", async () => {
  const actual = (await vi.importActual("../../components/shared")) as Record<string, unknown>;
  return { ...actual, useConfirm: () => confirmMock };
});

const START_TOOL = "giap-tasks__task_start";

function makeStream(events: ChatEvent[]): AsyncGenerator<ChatEvent> {
  return (async function* () {
    for (const ev of events) yield ev;
  })();
}

function historyWithTask(): SessionMessage[] {
  return [
    userRow({ session_id: "sess-hub", content: "plan me a trip to Lamu" }),
    assistantRow({
      id: "asst-0",
      session_id: "sess-hub",
      content: "",
      tool_calls: [{ id: "call-9", name: START_TOOL, arguments: "{}" }],
    }),
    {
      id: "tool-9",
      session_id: "sess-hub",
      role: "tool",
      created_at: "",
      tool_call_id: "call-9",
      content: JSON.stringify({ tool_call_id: "call-9", tool: START_TOOL, content: STARTED_TEXT }),
    },
    assistantRow({ id: "asst-1", session_id: "sess-hub", content: "I have started that. It will take a while.", tool_calls: undefined }),
  ];
}

beforeEach(() => {
  vi.clearAllMocks();
  __resetChatRunForTests();
  vi.mocked(api.getSettings).mockResolvedValue({
    chat_provider: "local",
    chat_model: "gemma",
    user_name: "",
    show_turn_stats: false,
  } as never);
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
});

afterEach(() => {
  cleanup();
});

describe("ChatHubView, a long task the prime starts", () => {
  it("draws the card in the answer's row, while the turn is still live", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([
        { type: "tool_call", tool: START_TOOL, id: "call-1", input: { goal: "Plan a trip to Lamu" } },
        { type: "tool_result", tool: START_TOOL, id: "call-1", content: STARTED_TEXT },
        { type: "text", content: "I have started that." },
        { done: true, session_id: "sess-hub", type: "done" },
      ]),
    );
    render(<ChatHubView />);
    await screen.findByText(/Is the front door locked/);
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "plan me a trip to Lamu" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    expect(await screen.findByText("Plan a trip to Lamu")).toBeTruthy();
    expect(screen.getByText("Working: step 2 of 4")).toBeTruthy();
    expect(api.getLongTask).toHaveBeenCalledWith(TASK_ID);
    const card = screen.getByRole("group", { name: /^Long task:/ });
    expect(card.className).toContain("lt--hub");
    expect(card.closest(".ch-bubble-wrap")?.textContent).toContain("I have started that.");
  });

  it("shows the card again when the conversation is reopened", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue(historyWithTask());
    await openSession("sess-hub");
    render(<ChatHubView />);

    expect(await screen.findByText("Plan a trip to Lamu")).toBeTruthy();
    expect(api.listLongTasks).toHaveBeenCalledWith("sess-hub");
    const card = screen.getByRole("group", { name: /^Long task:/ });
    expect(card.closest(".ch-bubble-wrap")?.textContent).toContain("I have started that. It will take a while.");
  });

  it("lists a task no message points at under its own label", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      userRow({ session_id: "sess-hub" }),
      assistantRow({ session_id: "sess-hub", tool_calls: undefined }),
    ]);
    vi.mocked(api.listLongTasks).mockResolvedValue([doneTask()]);
    await openSession("sess-hub");
    render(<ChatHubView />);

    const group = await screen.findByRole("region", { name: "Long tasks in this conversation" });
    expect(group.textContent).toContain("Plan a trip to Lamu");
  });

  it("shows nothing on a pond that has no long tasks", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue(historyWithTask());
    vi.mocked(api.listLongTasks).mockRejectedValue(new ApiError(404, "Not Found"));
    await openSession("sess-hub");
    render(<ChatHubView />);

    await screen.findByText("I have started that. It will take a while.");
    await waitFor(() => expect(screen.queryByText("Checking on this task...")).toBeNull());
    expect(screen.queryByRole("group", { name: /^Long task:/ })).toBeNull();
    expect(screen.queryByRole("region", { name: "Long tasks in this conversation" })).toBeNull();
    expect(api.getLongTask).not.toHaveBeenCalled();
  });

  it("reads the result of a finished task, as markdown", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue(historyWithTask());
    vi.mocked(api.getLongTask).mockResolvedValue(doneTask());
    vi.mocked(api.getLongTaskArtifact).mockResolvedValue({
      id: "art-1",
      title: "Trip plan",
      kind: "result",
      summary: "",
      body: "- ferry at **7:00**",
    });
    await openSession("sess-hub");
    render(<ChatHubView />);

    await screen.findByText("Three days in Lamu, with ferry times.");
    fireEvent.click(screen.getByRole("button", { name: "Read the result" }));
    expect((await screen.findByText("7:00")).tagName).toBe("STRONG");
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
    await openSession("sess-hub");
    render(<ChatHubView />);

    await screen.findByText("Three days in Lamu, with ferry times.");
    fireEvent.click(screen.getByRole("button", { name: "Read the result" }));
    const dialog = await screen.findByRole("dialog", { name: "Trip plan" });
    expect(within(dialog).getByRole("heading", { name: "Ferry" })).toBeTruthy();
    expect(within(dialog).getByText("7:00").tagName).toBe("STRONG");
    expect(within(dialog).getByRole("link", { name: "Ferry co" }).getAttribute("href")).toBe("https://example.com/ferry");
    expect(api.getLongTaskReport).toHaveBeenCalledWith(TASK_ID);
    expect(api.getLongTaskArtifact).not.toHaveBeenCalled();
  });

  it("stops a running task after asking", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue(historyWithTask());
    vi.mocked(api.cancelLongTask).mockResolvedValue(undefined);
    await openSession("sess-hub");
    render(<ChatHubView />);

    await screen.findByText("Plan a trip to Lamu");
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf({ state: "cancelled" }));
    fireEvent.click(screen.getByRole("button", { name: /^Stop the task/ }));

    await screen.findByText("Stopped");
    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(api.cancelLongTask).toHaveBeenCalledWith(TASK_ID);
  });
});
