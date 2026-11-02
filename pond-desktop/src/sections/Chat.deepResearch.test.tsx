import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, act, within } from "@testing-library/react";
import { Chat } from "./Chat";
import { api } from "../api/PondApiClient";
import { __resetChatRunForTests, getChatRun, openSession, setChatRunBridge } from "../state/chatRunStore";
import { assistantRow, userRow } from "../test-support/keptRows";
import { ApiError } from "../api/types";
import type { ChatEvent, DeepResearchAvailability, SessionMessage, VisionStatus } from "../api/types";
import { NOT_DECLARED_COPY } from "../components/imageSupportCopy";
import {
  DEEP_RESEARCH_NEEDS_NO_PICTURES,
  DEEP_RESEARCH_UNIDENTIFIED,
  PICTURES_NOT_IN_RESEARCH,
  RESEARCH_PLACEHOLDER,
  RESEARCH_REFUSED_CLAUSE,
  RESEARCH_WAITS,
} from "../components/deepResearchCopy";
import { prepareImage } from "../lib/imageAttach";
import type { PreparedImage } from "../lib/imageAttach";
import { OTHER_ID, STARTED_TEXT, TASK_ID, taskOf } from "../longtasks/longTaskFixtures";

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
    getVisionStatus: vi.fn(),
    getSessionAttachment: vi.fn(),
    compactSession: vi.fn(),
    retitleSession: vi.fn(),
    renameSession: vi.fn(),
    listLongTasks: vi.fn(),
    getLongTask: vi.fn(),
    getLongTaskArtifact: vi.fn(),
    cancelLongTask: vi.fn(),
    getDeepResearchAvailability: vi.fn(),
  },
}));

const appState = vi.hoisted(() => ({ sessionId: null as string | null }));

// The research card's Stop confirms; the app's Root provides the dialog, these tests do not.
const confirmMock = vi.hoisted(() => vi.fn());
vi.mock("../components/shared", async () => {
  const actual = (await vi.importActual("../components/shared")) as Record<string, unknown>;
  return { ...actual, useConfirm: () => confirmMock };
});

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

const START_TOOL = "giap-tasks__task_start";
const QUESTION = "What changed in Kenya's tax rules this year?";
const CALL_ID = `deep-research-${OTHER_ID}`;

const READY: VisionStatus = { model: "", state: { kind: "ready", bytes: null }, size_bytes: null, message: null };

function makeStream(events: ChatEvent[]): AsyncGenerator<ChatEvent> {
  return (async function* () {
    for (const ev of events) yield ev;
  })();
}

/** What the pond streams for a research turn: the call a model would make, then a short reply. */
function pondFrames(): ChatEvent[] {
  return [
    { type: "tool_call", tool: START_TOOL, id: CALL_ID, input: { goal: QUESTION } },
    { type: "tool_result", tool: START_TOOL, id: CALL_ID, content: STARTED_TEXT },
    { type: "text", content: "I have started the research." },
    { done: true, session_id: "sess-1", type: "done" },
  ];
}

function refusing(error: ApiError) {
  vi.mocked(api.chatStream).mockImplementation(() =>
    (async function* () {
      throw error;
    })(),
  );
}

function offer(answer: DeepResearchAvailability | null) {
  vi.mocked(api.getDeepResearchAvailability).mockResolvedValue(answer);
}

function fakePrepared(): PreparedImage {
  return { data: "AAA", mime_type: "image/png", previewUrl: "blob:pond/fake", width: 10, height: 10, byteSize: 3 };
}

beforeEach(() => {
  vi.clearAllMocks();
  appState.sessionId = null;
  vi.mocked(api.listSessions).mockResolvedValue([]);
  vi.mocked(api.getSessionMessages).mockResolvedValue([]);
  vi.mocked(api.getVisionStatus).mockResolvedValue(READY);
  vi.mocked(api.listLongTasks).mockResolvedValue([]);
  vi.mocked(api.getLongTask).mockResolvedValue(taskOf());
  vi.mocked(prepareImage).mockResolvedValue(fakePrepared());
  offer({ available: true });
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

const field = () => screen.getByLabelText("Message input") as HTMLTextAreaElement;
const menu = () => screen.queryByRole("menu", { name: "Add to message" });

async function mounted() {
  render(<Chat />);
  await screen.findByLabelText("Message input");
}

/** Lets the availability answer land, so a menu without the item is the pond's answer and not a wait for it. */
async function settled() {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

async function openMenu() {
  fireEvent.click(await screen.findByRole("button", { name: "Add to message" }));
}

async function startResearchMode() {
  await openMenu();
  fireEvent.click(await screen.findByRole("menuitemcheckbox", { name: "Deep research" }));
}

function type(text: string) {
  fireEvent.change(field(), { target: { value: text } });
}

const chip = () => document.querySelector(".rchip");

describe("Chat, the plus menu", () => {
  it("holds Add pictures and Deep research, and no paperclip", async () => {
    await mounted();
    await openMenu();
    expect(screen.getByRole("menuitem", { name: "Add pictures" })).toBeTruthy();
    const item = await screen.findByRole("menuitemcheckbox", { name: "Deep research" });
    expect(item.getAttribute("aria-disabled")).toBeNull();
    expect(screen.queryByRole("button", { name: "Attach image" })).toBeNull();
  });

  it("holds only Add pictures when the household has switched long tasks off", async () => {
    offer({ available: false, reason: "off" });
    await mounted();
    await settled();
    await openMenu();
    expect(screen.getByRole("menuitem", { name: "Add pictures" })).toBeTruthy();
    expect(screen.queryByRole("menuitemcheckbox")).toBeNull();
  });

  it.each([
    ["an older pond", null],
    ["a pond that cannot be asked", "fails"],
  ] as const)("holds only Add pictures for %s", async (_name, how) => {
    if (how === null) offer(null);
    else vi.mocked(api.getDeepResearchAvailability).mockRejectedValue(new ApiError(500, "boom"));
    await mounted();
    await settled();
    await openMenu();
    expect(api.getDeepResearchAvailability).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("menuitem", { name: "Add pictures" })).toBeTruthy();
    expect(screen.queryByRole("menuitemcheckbox")).toBeNull();
  });

  it("shows Deep research disabled, with one line saying why, when the pond cannot tell who is asking", async () => {
    offer({ available: false, reason: "unidentified" });
    await mounted();
    await openMenu();
    const item = await screen.findByRole("menuitemcheckbox", { name: "Deep research" });
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByText(DEEP_RESEARCH_UNIDENTIFIED)).toBeTruthy();
    fireEvent.click(item);
    expect(chip()).toBeNull();
  });

  it("asks the pond once for the mount, however often the menu opens", async () => {
    await mounted();
    for (let i = 0; i < 3; i += 1) {
      await openMenu();
      await screen.findByRole("menuitemcheckbox", { name: "Deep research" });
      fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    }
    expect(api.getDeepResearchAvailability).toHaveBeenCalledTimes(1);
  });
});

describe("Chat, Add pictures keeps every rule the paperclip had", () => {
  it("opens the file picker", async () => {
    await mounted();
    const picker = document.querySelector('input[type="file"]') as HTMLInputElement;
    const open = vi.spyOn(picker, "click").mockImplementation(() => {});
    await openMenu();
    fireEvent.click(screen.getByRole("menuitem", { name: "Add pictures" }));
    expect(open).toHaveBeenCalledTimes(1);
  });

  it("says why when the model cannot read pictures: in its title, and under the composer once pressed", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "x", state: { kind: "not_declared" }, size_bytes: null, message: null,
    });
    await mounted();
    await openMenu();
    const item = screen.getByRole("menuitem", { name: "Add pictures" });
    await waitFor(() => expect(item.getAttribute("title")).toBe(NOT_DECLARED_COPY));
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(document.querySelector(".vision-status")).toBeNull();

    fireEvent.click(item);
    await waitFor(() => expect(document.querySelector(".vision-status")?.textContent).toContain(NOT_DECLARED_COPY));
  });

  it("still adds a picked picture to the tray", async () => {
    await mounted();
    const picker = document.querySelector('input[type="file"]') as HTMLInputElement;
    fireEvent.change(picker, { target: { files: [new File(["fake"], "photo.png", { type: "image/png" })] } });
    expect(await screen.findByAltText("Attached image 1")).toBeTruthy();
  });
});

describe("Chat, deep research mode", () => {
  it("shows a chip and the research prompt, and puts the cursor in the box", async () => {
    await mounted();
    await startResearchMode();

    expect(chip()?.textContent).toContain("Deep research");
    expect(field().placeholder).toBe(RESEARCH_PLACEHOLDER);
    expect(document.activeElement).toBe(field());
    expect(screen.getByLabelText("Start deep research")).toBeTruthy();
    expect(screen.queryByLabelText("Send message")).toBeNull();
    expect(menu()).toBeNull();
  });

  it("leaves from the chip's x, and the cursor goes back to the box", async () => {
    await mounted();
    await startResearchMode();
    fireEvent.click(screen.getByRole("button", { name: "Leave deep research" }));

    expect(chip()).toBeNull();
    expect(field().placeholder).toBe("Message Goose…");
    expect(document.activeElement).toBe(field());
    expect(screen.getByLabelText("Send message")).toBeTruthy();
  });

  it("leaves on Escape in the box", async () => {
    await mounted();
    await startResearchMode();
    fireEvent.keyDown(field(), { key: "Escape" });
    expect(chip()).toBeNull();
  });

  it("leaves on Escape from the chip itself", async () => {
    await mounted();
    await startResearchMode();
    screen.getByRole("button", { name: "Leave deep research" }).focus();
    fireEvent.keyDown(screen.getByRole("button", { name: "Leave deep research" }), { key: "Escape" });
    expect(chip()).toBeNull();
  });

  it("closes an open menu with Escape before it leaves the mode", async () => {
    await mounted();
    await startResearchMode();
    fireEvent.pointerDown(screen.getByRole("button", { name: "Add to message" }));
    await openMenu();
    expect(menu()).not.toBeNull();

    fireEvent.keyDown(field(), { key: "Escape" });
    expect(menu()).toBeNull();
    expect(chip()).not.toBeNull();

    fireEvent.keyDown(field(), { key: "Escape" });
    expect(chip()).toBeNull();
  });

  it("leaves from the menu, where it shows a tick", async () => {
    await mounted();
    await startResearchMode();
    await openMenu();
    const item = await screen.findByRole("menuitemcheckbox", { name: "Deep research" });
    expect(item.getAttribute("aria-checked")).toBe("true");
    fireEvent.click(item);
    expect(chip()).toBeNull();
  });

  it("ends when a new chat is opened", async () => {
    await mounted();
    await startResearchMode();
    fireEvent.click(screen.getByRole("button", { name: "New chat" }));
    expect(chip()).toBeNull();
  });

  it("ends when the conversation changes underneath it", async () => {
    await mounted();
    await act(async () => {
      await openSession("sess-a");
    });
    await startResearchMode();
    await act(async () => {
      await openSession("sess-b");
    });
    expect(chip()).toBeNull();
  });

  it("does not end when a new chat gets its first session id", async () => {
    let go!: () => void;
    const gate = new Promise<void>((r) => { go = r; });
    vi.mocked(api.chatStream).mockReturnValueOnce(
      (async function* () {
        yield { type: "text", content: "Thinking it over" } as ChatEvent;
        await gate;
        yield { type: "run_started", run_id: "run-1", epoch: "e1", session_id: "sess-new" } as ChatEvent;
        yield { done: true, session_id: "sess-new", type: "done" } as ChatEvent;
      })(),
    );
    await mounted();
    type("hi");
    fireEvent.click(screen.getByLabelText(/send message|queue message/i));
    await screen.findByText(/Thinking it over/);
    await startResearchMode();

    await act(async () => {
      go();
      await new Promise((r) => setTimeout(r, 0));
    });
    await waitFor(() => expect(getChatRun().sessionId).toBe("sess-new"));
    expect(chip()).not.toBeNull();
  });

  it("is a fresh, ordinary composer on the next visit", async () => {
    await mounted();
    await startResearchMode();
    cleanup();
    await mounted();
    expect(chip()).toBeNull();
    expect(field().placeholder).toBe("Message Goose…");
  });
});

describe("Chat, sending a research question", () => {
  it("carries the flag with the question, then the mode ends", async () => {
    vi.mocked(api.chatStream).mockReturnValue(makeStream(pondFrames()));
    await mounted();
    await startResearchMode();
    type(QUESTION);
    fireEvent.click(screen.getByLabelText("Start deep research"));

    await screen.findByText("I have started the research.");
    const call = vi.mocked(api.chatStream).mock.calls[0];
    expect(call[0]).toBe(QUESTION);
    expect(call[6]).toBe(true);
    expect(chip()).toBeNull();
    expect(field().value).toBe("");
    expect(field().placeholder).toBe("Message Goose…");
  });

  it("sends with Cmd+Enter as well", async () => {
    vi.mocked(api.chatStream).mockReturnValue(makeStream(pondFrames()));
    await mounted();
    await startResearchMode();
    type(QUESTION);
    fireEvent.keyDown(field(), { key: "Enter", metaKey: true });

    await screen.findByText("I have started the research.");
    expect(vi.mocked(api.chatStream).mock.calls[0][6]).toBe(true);
  });

  it("makes the next message an ordinary one", async () => {
    vi.mocked(api.chatStream)
      .mockReturnValueOnce(makeStream(pondFrames()))
      .mockReturnValueOnce(makeStream([{ type: "text", content: "Hello." }, { done: true, session_id: "sess-1", type: "done" }]));
    await mounted();
    await startResearchMode();
    type(QUESTION);
    fireEvent.click(screen.getByLabelText("Start deep research"));
    await screen.findByText("I have started the research.");

    type("and what about Uganda?");
    await waitFor(() => expect(screen.getByLabelText("Send message")).toBeTruthy());
    fireEvent.click(screen.getByLabelText("Send message"));
    await screen.findByText("Hello.");
    expect(vi.mocked(api.chatStream).mock.calls[1][0]).toBe("and what about Uganda?");
    expect(vi.mocked(api.chatStream).mock.calls[1][6]).toBeUndefined();
  });

  it("cannot send an empty question", async () => {
    await mounted();
    await startResearchMode();
    expect((screen.getByLabelText("Start deep research") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.keyDown(field(), { key: "Enter", metaKey: true });
    expect(api.chatStream).not.toHaveBeenCalled();
    expect(chip()).not.toBeNull();
  });

  it("holds the question while an answer is still streaming, and says so, instead of queueing it as an ordinary message", async () => {
    let release!: () => void;
    const held = new Promise<void>((r) => { release = r; });
    vi.mocked(api.chatStream).mockReturnValueOnce(
      (async function* () {
        yield { type: "text", content: "Still going" } as ChatEvent;
        await held;
        yield { done: true, session_id: "sess-1", type: "done" } as ChatEvent;
      })(),
    );
    vi.mocked(api.chatStream).mockReturnValueOnce(makeStream(pondFrames()));
    await mounted();
    type("a long one");
    fireEvent.click(screen.getByLabelText(/send message|queue message/i));
    await screen.findByText(/Still going/);

    await startResearchMode();
    type(QUESTION);
    const send = screen.getByLabelText("Start deep research") as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    expect(screen.getByText(RESEARCH_WAITS)).toBeTruthy();
    fireEvent.keyDown(field(), { key: "Enter", metaKey: true });
    expect(api.chatStream).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("Queued")).toBeNull();

    await act(async () => {
      release();
      await new Promise((r) => setTimeout(r, 0));
    });
    await waitFor(() => expect((screen.getByLabelText("Start deep research") as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByLabelText("Start deep research"));
    await waitFor(() => expect(api.chatStream).toHaveBeenCalledTimes(2));
    expect(vi.mocked(api.chatStream).mock.calls[1][6]).toBe(true);
  });

  it("sends a suggestion chip as an ordinary message, and keeps the mode for the box", async () => {
    vi.mocked(api.chatStream).mockReturnValue(
      makeStream([{ type: "text", content: "I can help with plenty." }, { done: true, session_id: "sess-1", type: "done" }]),
    );
    await mounted();
    await startResearchMode();
    fireEvent.click(await screen.findByText("What can you help me with?"));

    await screen.findByText("I can help with plenty.");
    expect(vi.mocked(api.chatStream).mock.calls[0][0]).toBe("What can you help me with?");
    expect(vi.mocked(api.chatStream).mock.calls[0][6]).toBeUndefined();
    expect(chip()).not.toBeNull();
  });
});

describe("Chat, pictures and research", () => {
  async function addPicture() {
    const picker = document.querySelector('input[type="file"]') as HTMLInputElement;
    fireEvent.change(picker, { target: { files: [new File(["fake"], "photo.png", { type: "image/png" })] } });
    await screen.findByAltText("Attached image 1");
  }

  it("turns Add pictures off while the mode is on, with the reason in view", async () => {
    await mounted();
    await startResearchMode();
    await openMenu();
    const item = screen.getByRole("menuitem", { name: "Add pictures" });
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByText(PICTURES_NOT_IN_RESEARCH)).toBeTruthy();

    const picker = document.querySelector('input[type="file"]') as HTMLInputElement;
    const open = vi.spyOn(picker, "click").mockImplementation(() => {});
    fireEvent.click(item);
    expect(open).not.toHaveBeenCalled();
  });

  it("refuses a pasted picture in research mode, and says why", async () => {
    await mounted();
    await startResearchMode();
    fireEvent.paste(field(), { clipboardData: { files: [new File(["fake"], "photo.png", { type: "image/png" })] } });

    expect((await screen.findByRole("alert")).textContent).toBe(PICTURES_NOT_IN_RESEARCH);
    expect(prepareImage).not.toHaveBeenCalled();
    expect(screen.queryByAltText("Attached image 1")).toBeNull();
  });

  it("turns Deep research off while a picture waits in the tray, with the reason in view", async () => {
    await mounted();
    await addPicture();
    await openMenu();
    const item = await screen.findByRole("menuitemcheckbox", { name: "Deep research" });
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByText(DEEP_RESEARCH_NEEDS_NO_PICTURES)).toBeTruthy();
    fireEvent.click(item);
    expect(chip()).toBeNull();
    expect(screen.getByAltText("Attached image 1")).toBeTruthy();
  });

  it("offers it again once the picture is removed", async () => {
    await mounted();
    await addPicture();
    fireEvent.click(screen.getByRole("button", { name: "Remove attached image 1" }));
    await startResearchMode();
    expect(chip()).not.toBeNull();
  });
});

describe("Chat, a research question the pond refuses", () => {
  const OFF = "Deep research is switched off on this pond.";
  const GUEST = "The pond cannot tell who is asking.";

  async function ask() {
    await mounted();
    await startResearchMode();
    type(QUESTION);
    fireEvent.click(screen.getByLabelText("Start deep research"));
  }

  it("hands the question back on a 409 for off, as the no-model refusal does, and drops the item", async () => {
    refusing(new ApiError(409, OFF, undefined, { error: OFF, reason: "deep_research_off" }));
    await ask();

    await waitFor(() => expect(field().value).toBe(QUESTION));
    expect((await screen.findByRole("alert")).textContent).toBe(OFF + RESEARCH_REFUSED_CLAUSE);
    expect(chip()).toBeNull();
    expect(screen.queryByText(/^error:/i)).toBeNull();

    await openMenu();
    expect(screen.getByRole("menuitem", { name: "Add pictures" })).toBeTruthy();
    expect(screen.queryByRole("menuitemcheckbox")).toBeNull();
  });

  it("hands the question back on a 403 for a caller it cannot place, and shows the item disabled", async () => {
    refusing(new ApiError(403, GUEST, undefined, { error: GUEST, reason: "deep_research_unidentified" }));
    await ask();

    await waitFor(() => expect(field().value).toBe(QUESTION));
    expect((await screen.findByRole("alert")).textContent).toBe(GUEST + RESEARCH_REFUSED_CLAUSE);
    expect(chip()).toBeNull();

    await openMenu();
    const item = screen.getByRole("menuitemcheckbox", { name: "Deep research" });
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(within(item).getByText(DEEP_RESEARCH_UNIDENTIFIED)).toBeTruthy();
  });

  it("puts the mode back when it was refused for some other reason", async () => {
    refusing(new ApiError(413, "That message is too long.", "message_too_large"));
    await ask();

    await waitFor(() => expect(field().value).toBe(QUESTION));
    expect(chip()).not.toBeNull();
    expect(field().placeholder).toBe(RESEARCH_PLACEHOLDER);
  });
});

describe("Chat, the card a research turn starts", () => {
  it("is anchored to the pond's own task_start frames and called Deep research", async () => {
    vi.mocked(api.chatStream).mockReturnValue(makeStream(pondFrames()));
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf({ kind: "research", goal: QUESTION }));
    await mounted();
    await startResearchMode();
    type(QUESTION);
    fireEvent.click(screen.getByLabelText("Start deep research"));

    const card = await screen.findByRole("group", { name: `Deep research: ${QUESTION}` });
    expect(api.getLongTask).toHaveBeenCalledWith(TASK_ID);
    expect(card.closest(".ch-bubble-wrap")?.textContent).toContain("I have started the research.");
    expect(screen.queryByRole("group", { name: /^Long task:/ })).toBeNull();
  });

  it("is anchored again when the conversation is reopened", async () => {
    const rows: SessionMessage[] = [
      userRow({ content: QUESTION }),
      assistantRow({ id: "asst-0", content: "", tool_calls: [{ id: CALL_ID, name: START_TOOL, arguments: JSON.stringify({ goal: QUESTION }) }] }),
      {
        id: "tool-9",
        session_id: "sess-1",
        role: "tool",
        created_at: "",
        tool_call_id: CALL_ID,
        content: JSON.stringify({ tool_call_id: CALL_ID, tool: START_TOOL, content: STARTED_TEXT }),
      },
      assistantRow({ id: "asst-1", content: "I have started the research.", tool_calls: undefined }),
    ];
    vi.mocked(api.getSessionMessages).mockResolvedValue(rows);
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf({ kind: "research", goal: QUESTION }));
    await openSession("sess-1");
    render(<Chat />);

    const card = await screen.findByRole("group", { name: `Deep research: ${QUESTION}` });
    expect(card.closest(".ch-bubble-wrap")?.textContent).toContain("I have started the research.");
  });
});
