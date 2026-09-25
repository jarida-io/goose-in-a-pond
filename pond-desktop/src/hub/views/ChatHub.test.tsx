// The Hub's chat is a second rendering of the Chat section's conversation, not
// a second conversation, so it never loads history itself: a replayed image
// reaches it through `chatRunStore`. It used to render the bare attachment URL
// the store handed it, and that URL sits on the protected router, where an
// `<img src>` -- which cannot send the bearer header -- got a 401 on every pond
// without the loopback dev bypass. What it must render now is the object URL
// the store made from bytes the client fetched with its token.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent, waitFor } from "@testing-library/react";
import { ChatHubView } from "./ChatHub";
import { api } from "../../api/PondApiClient";
import { ApiError } from "../../api/types";
import type { VisionStatus } from "../../api/types";
import { __resetChatRunForTests, openSession } from "../../state/chatRunStore";
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
    // Ready by default, so tests that do not care about picture support see
    // the send gate stay open — see the "picture support" describe below for
    // the states that close it.
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

vi.mock("../../state/AppContext", () => ({
  useAppState: () => ({ serverOnline: true, sessionId: "sess-hub" }),
  useAppDispatch: () => vi.fn(),
}));

// Only `prepareImage` is faked — everything else (validateAttachmentSet, the
// MIME lists AttachmentTray itself reads) stays real. happy-dom's <img> never
// fires a real decode, so prepareImage cannot run end to end here; the tests
// below only need SOME PreparedImage to reach the composer's state, the way
// a real decode would.
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
  // Call history AND the resolved-value overrides a previous test set both
  // persist across tests otherwise — `mockResolvedValue` replaces the mock's
  // implementation for good, not just for the test that called it.
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

/**
 * Picture support's own status, and the send/paste gate it drives.
 *
 * The paperclip is never disabled for a vision reason (see ChatHub.tsx), so
 * these test the actual gate: the composer accepts an attachment into its
 * tray regardless of status, and only refuses to SEND it.
 */
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
    // Wait for the mocked status to actually land before pasting -- otherwise
    // the paste can race the hook's first fetch and land while gate.blocked
    // is still evaluating from the (unblocked) capabilities fallback.
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
