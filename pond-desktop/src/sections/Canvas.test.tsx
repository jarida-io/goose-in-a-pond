import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { Canvas } from "./Canvas";
import { api } from "../api/PondApiClient";
import type { ChatEvent } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    chatStream: vi.fn(),
    listTools: vi.fn().mockResolvedValue([]),
    getMcpResource: vi.fn(),
    transcribe: vi.fn(),
  },
}));

vi.mock("../state/useAppContext", () => ({
  useAppState: () => ({ serverOnline: true, sessionToken: "t", sessionId: null, serverUrl: "http://127.0.0.1:4000" }),
  useAppDispatch: () => vi.fn(),
}));

afterEach(cleanup);

const WAIT = "Your pond is busy with another conversation. Yours is next.";

describe("Canvas: a status never becomes the answer", () => {
  it("shows the waiting line apart from the answer, and the answer holds only the model's words", async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => { release = r; });
    vi.mocked(api.chatStream).mockReturnValue(
      (async function* () {
        yield { type: "status", content: "Agent working..." } as ChatEvent;
        yield { type: "status", content: WAIT, waiting: "another_conversation" } as ChatEvent;
        await gate;
        yield { type: "text", content: "Here you go." } as ChatEvent;
        yield { type: "done", done: true, session_id: "s1" } as ChatEvent;
      })() as never,
    );
    render(<Canvas />);
    fireEvent.change(screen.getByPlaceholderText("Ask anything…"), { target: { value: "hi" } });
    fireEvent.click(screen.getByLabelText("Send message"));

    const line = await screen.findByTestId("waiting-line");
    expect(line.textContent).toContain(WAIT);
    expect(screen.queryByText("Agent working...")).toBeNull();

    release();
    const answer = await screen.findByText("Here you go.");
    expect(answer.textContent).toBe("Here you go.");
    expect(document.body.textContent).not.toContain(WAIT);
    expect(screen.queryByTestId("waiting-line")).toBeNull();
  }, 30_000);
});
