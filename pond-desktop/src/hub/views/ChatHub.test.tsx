// The Hub renders the Chat section's conversation, so replayed images arrive via `chatRunStore`
// as object URLs of token-fetched bytes: a bare attachment URL 401s in `<img src>`.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent, waitFor } from "@testing-library/react";
import { ChatHubView } from "./ChatHub";
import { api } from "../../api/PondApiClient";
import { ApiError } from "../../api/types";
import type { VisionStatus } from "../../api/types";
import { __resetChatRunForTests, openSession } from "../../state/chatRunStore";
import { assistantRow, keptToolRow, userRow } from "../../test-support/keptRows";
import { ATTACH_BLOCKED_COPY, NOT_DECLARED_COPY } from "../../components/imageSupportCopy";
import { prepareImage } from "../../lib/imageAttach";
import type { PreparedImage } from "../../lib/imageAttach";

vi.mock("../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn().mockResolvedValue({ user_name: "", show_turn_stats: false }),
    getModelCapabilities: vi.fn().mockResolvedValue({
      thinking: false,
      vision: true,
      audio_input: false,
      context_window_tokens: 8192,
      structured_output: false,
      tool_calling: true,
    }),
    // Terminal state so the WarmupBanner renders nothing and never re-polls.
    getWarmupStatus: vi.fn().mockResolvedValue({
      state: "skipped", reason: "test", model: "", started_unix_ms: 0,
      finished_unix_ms: null, elapsed_ms: 0,
    }),
    // Ready by default, so the send gate stays open outside the "picture support" tests.
    getVisionStatus: vi.fn().mockResolvedValue({
      model: "", state: { kind: "ready", bytes: null }, size_bytes: null, message: null,
    } satisfies VisionStatus),
    listSuggestions: vi.fn().mockResolvedValue({ suggestions: [] }),
    getSessionMessages: vi.fn(),
    getSessionAttachment: vi.fn(),
    chatStream: vi.fn(),
    setToken: vi.fn(),
  },
}));

vi.mock("../../state/useAppContext", () => ({
  useAppState: () => ({ serverOnline: true, sessionId: "sess-hub" }),
  useAppDispatch: () => vi.fn(),
}));

// Only `prepareImage` is faked: happy-dom's <img> never decodes, so it can't run here.
vi.mock("../../lib/imageAttach", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../lib/imageAttach")>();
  return { ...actual, prepareImage: vi.fn() };
});

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
  // Re-set each test: `mockResolvedValue` overrides outlive the test that set them.
  vi.clearAllMocks();
  __resetChatRunForTests();
  vi.mocked(api.getVisionStatus).mockResolvedValue(READY_STATUS);
  vi.mocked(prepareImage).mockResolvedValue(fakePrepared());
});

afterEach(() => {
  cleanup();
});

describe("ChatHubView — history images", () => {
  it("renders the store's object URL for a replayed image, not the attachment URL", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([
      {
        id: "m1",
        session_id: "sess-hub",
        role: "user",
        content: "what is in this picture?",
        created_at: "",
        images: [
          {
            id: "att-1",
            mime_type: "image/png",
            byte_size: 3,
            url: "/api/v1/sessions/sess-hub/attachments/att-1",
          },
        ],
      },
    ] as never);
    vi.mocked(api.getSessionAttachment).mockResolvedValue(
      new Blob(["png"], { type: "image/png" }),
    );

    // Opened elsewhere, which is the only way the Hub ever shows history.
    await openSession("sess-hub");
    render(<ChatHubView />);

    const img = await screen.findByAltText("Attached image 1");
    expect(img.getAttribute("src")).toMatch(/^blob:/);
    expect(vi.mocked(api.getSessionAttachment)).toHaveBeenCalledWith("sess-hub", "att-1");
    expect(document.querySelector('img[src*="/attachments/"]')).toBeNull();
  });
});

/** The paperclip is never vision-gated, so these test the real gate: attaching works, sending is refused. */
describe("ChatHubView — picture support", () => {
  async function attachOneImage() {
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

    render(<ChatHubView />);

    await screen.findByText(/Getting picture support ready: 412 MB of 941 MB/);
  });

  it("blocks a click-to-send while picture support is not ready", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "gemma-4-E2B-it-Q4_K_M",
      state: { kind: "absent" },
      size_bytes: null,
      message: "Picture support for Gemma 4 E2B needs a one-time 941 MB download. It starts by itself; text chat works meanwhile.",
    } satisfies VisionStatus);

    render(<ChatHubView />);
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

  it("blocks Enter the same way", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "x", state: { kind: "verifying" }, size_bytes: null,
      message: "Checking picture support before its first use. Text chat works meanwhile.",
    } satisfies VisionStatus);

    render(<ChatHubView />);
    await attachOneImage();
    const input = screen.getByLabelText("Message input");
    fireEvent.change(input, { target: { value: "what is this" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await screen.findByText(/Pictures can be sent once picture support is ready/);
    expect(api.chatStream).not.toHaveBeenCalled();
  });

  it("blocks a suggestion chip too", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "x", state: { kind: "not_declared" }, size_bytes: null, message: null,
    } satisfies VisionStatus);

    render(<ChatHubView />);
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

    render(<ChatHubView />);
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

    render(<ChatHubView />);
    await attachOneImage();
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "what is this" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    // The draft comes back: the text box, the tray, and a line naming why.
    await waitFor(() => {
      expect((screen.getByLabelText("Message input") as HTMLInputElement).value).toBe(
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

/** With none chosen the pond refuses a turn and picks nothing, so the screen offers its suggestions. */
describe("ChatHubView — the paperclip's reason", () => {
  it("names Pictures included when the model cannot look at pictures", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "x", state: { kind: "not_declared" }, size_bytes: null, message: null,
    } satisfies VisionStatus);
    render(<ChatHubView />);
    const clip = await screen.findByRole("button", { name: "Attach image" });
    await waitFor(() => expect(clip.getAttribute("title")).toBe(NOT_DECLARED_COPY));
    expect(clip.getAttribute("title")).not.toMatch(/Reads pictures/);
  });

  it("says the same label when the pond has nothing more to say about why it is blocked", async () => {
    vi.mocked(api.getVisionStatus).mockResolvedValue({
      model: "x", state: { kind: "absent" }, size_bytes: null, message: null,
    } satisfies VisionStatus);
    render(<ChatHubView />);
    const clip = await screen.findByRole("button", { name: "Attach image" });
    await waitFor(() => expect(clip.getAttribute("title")).toBe(ATTACH_BLOCKED_COPY));
  });
});

describe("ChatHubView — no conversation model", () => {
  let added: string[] = [];

  beforeEach(async () => {
    const fixtures = await import("../../sections/models/fixtures");
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

  it("offers the suggestions with their sizes, instead of a conversation it cannot hold", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "", chat_model: "", show_turn_stats: false } as never);
    render(<ChatHubView />);

    expect(await screen.findByRole("heading", { name: "Pick a model to talk with" })).toBeTruthy();
    expect(await screen.findByText("4.2 GB + 945 MB for pictures")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Download and use Gemma 4 E4B, llama.cpp" })).toBeTruthy();
    // The sample conversation would promise what the pond cannot do.
    expect(screen.queryByText(/Is the front door locked/)).toBeNull();
    expect(api.downloadModel).not.toHaveBeenCalled();
    expect(api.activateModel).not.toHaveBeenCalled();
  });

  it("keeps the sample conversation when a model is chosen", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "local", chat_model: "gemma", show_turn_stats: false } as never);
    render(<ChatHubView />);
    expect(await screen.findByText(/Is the front door locked/)).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Pick a model to talk with" })).toBeNull();
  });

  it("shows the suggestions when a turn is refused for want of a model, and hands the message back", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "local", chat_model: "gone", show_turn_stats: false } as never);
    vi.mocked(api.chatStream).mockImplementation(() =>
      (async function* () {
        throw new ApiError(409, "No conversation model is chosen yet. Choose one on the Models page to start talking.", "no_model");
      })(),
    );
    render(<ChatHubView />);
    await screen.findByText(/Is the front door locked/);

    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: "lights on" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    expect(await screen.findByRole("heading", { name: "Pick a model to talk with" })).toBeTruthy();
    await waitFor(() => {
      expect((screen.getByLabelText("Message input") as HTMLInputElement).value).toBe("lights on");
    });
    await screen.findByText(/Your message is back in the box; send it once a model is chosen\./);
  });
});

describe("ChatHubView, a result the pond did not keep", () => {
  it("shows one quiet line under the answer, naming the sources", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([userRow(), keptToolRow(), assistantRow()] as never);
    await openSession("sess-hub");
    render(<ChatHubView />);

    const note = await screen.findByText(
      "Web search: results aren't kept after the turn. Sources: kenyapower.co.ke, nation.africa.",
    );
    expect(note.closest(".kept-note")).toBeTruthy();
    expect(screen.getByText("Power should be back by 6 pm.")).toBeTruthy();
  });

  it("puts the line under the answer once, and nowhere under the question", async () => {
    vi.mocked(api.getSessionMessages).mockResolvedValue([userRow(), keptToolRow(), assistantRow()] as never);
    await openSession("sess-hub");
    render(<ChatHubView />);
    await screen.findByText("Power should be back by 6 pm.");
    expect(document.querySelectorAll(".kept-note")).toHaveLength(1);
  });
});

describe("ChatHubView, the web's results on screen", () => {
  const THUMB = "https://imgs.search.brave.com/p/rs:fit:500:0:0/g:ce/aHR0cHM6";

  function turn(tool: string, ui: Record<string, unknown>) {
    vi.mocked(api.chatStream).mockImplementation(() =>
      (async function* () {
        yield { type: "tool_call", tool, id: "t1" };
        yield { type: "tool_result", tool, id: "t1", content: "done", transient: true, ui };
        yield { type: "text", content: "Here they are." };
        yield { done: true, session_id: "sess-hub" };
      })() as never,
    );
  }

  async function ask(text: string) {
    render(<ChatHubView />);
    fireEvent.change(screen.getByLabelText("Message input"), { target: { value: text } });
    fireEvent.click(screen.getByLabelText("Send message"));
    await screen.findByText("Here they are.");
  }

  it("shows the pictures a turn put on the screen, under the answer", async () => {
    turn("giap-knowledge__show_pictures", {
      card_type: "pictures",
      data: {
        query: "lions at dawn",
        pictures: [
          { title: "Lions", url: "https://a.example/lions", domain: "a.example", thumbnail: THUMB, width: 500, height: 333 },
        ],
      },
    });
    await ask("show me lions");
    const region = await screen.findByRole("region", { name: "Pictures result" });
    expect(region.querySelector("img")?.getAttribute("src")).toBe(THUMB);
    expect(screen.getByRole("link", { name: /Lions/ }).getAttribute("href")).toBe("https://a.example/lions");
  });

  it("shows a web search's card too, with the pages and the news Brave added", async () => {
    turn("giap-knowledge__search_web", {
      card_type: "search",
      data: {
        query: "the library",
        results: [{ title: "Library hours", url: "https://lib.example/hours", domain: "lib.example", snippet: "9 to 1." }],
        news: [{ title: "A new reading room", url: "https://news.example/room", domain: "news.example", publisher: "News", date: "2026-10-26", thumbnail: null, breaking: false }],
      },
    });
    await ask("when does the library open");
    const region = await screen.findByRole("region", { name: "Web search result" });
    expect(region.textContent).toContain("Library hours");
    expect(region.textContent).toContain("A new reading room");
  });

  it("draws no web card for a turn whose last tool was something else", async () => {
    turn("giap-weather__get_current_weather", { card_type: "weather", data: { temp: 21 } });
    await ask("weather?");
    expect(screen.queryByRole("region", { name: /result$/ })).toBeNull();
  });
});
