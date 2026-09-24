// The Hub's chat is a second rendering of the Chat section's conversation, not
// a second conversation, so it never loads history itself: a replayed image
// reaches it through `chatRunStore`. It used to render the bare attachment URL
// the store handed it, and that URL sits on the protected router, where an
// `<img src>` -- which cannot send the bearer header -- got a 401 on every pond
// without the loopback dev bypass. What it must render now is the object URL
// the store made from bytes the client fetched with its token.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { ChatHubView } from "./ChatHub";
import { api } from "../../api/PondApiClient";
import { __resetChatRunForTests, openSession } from "../../state/chatRunStore";

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

beforeEach(() => {
  __resetChatRunForTests();
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
