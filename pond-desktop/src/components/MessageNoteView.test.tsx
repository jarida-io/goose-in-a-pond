import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import type { MessageNote } from "../api/types";
import * as store from "../state/chatRunStore";
import { NoteFromLine, ReceiptChips, SuggestionOffer } from "./MessageNoteView";

vi.mock("../api/PondApiClient", () => ({ api: { answerOffer: vi.fn() } }));
vi.mock("../state/chatRunStore", () => ({ openSession: vi.fn(), patchMessage: vi.fn(), sendTurn: vi.fn() }));

const offer = (state: "open" | "done" | "dismissed" = "open"): MessageNote => ({
  suggestion: { noticed: "You ask for the weather at 8", action: "Make it a routine", prompt: "Set up a daily weather routine", state },
});

beforeEach(() => vi.clearAllMocks());
afterEach(cleanup);

describe("the note views", () => {
  it("shows the from line and a chip per receipt, with a reason for one not set", () => {
    render(
      <>
        <NoteFromLine note={{ from: { kind: "schedule", label: "Daily Briefing" } }} />
        <ReceiptChips note={{ receipts: [
          { kind: "reminder", outcome: "set", what: "call Sam" },
          { kind: "timer", outcome: "not_set", what: "10 minutes", reason: "No time was given" },
        ] }} />
      </>,
    );
    expect(screen.getByText("From your schedule: Daily Briefing")).toBeTruthy();
    expect(screen.getByText("Set: call Sam")).toBeTruthy();
    expect(screen.getByText("Not set: 10 minutes")).toBeTruthy();
    expect(screen.getByText("No time was given")).toBeTruthy();
  });

  it("shows nothing without a note", () => {
    const { container } = render(<><NoteFromLine note={undefined} /><ReceiptChips note={{}} /></>);
    expect(container.innerHTML).toBe("");
  });
});

describe("SuggestionOffer", () => {
  const props = { localId: 3, messageId: "m1", sessionId: "s1" };

  it("takes the offer, then sends the prompt as the person's own turn", async () => {
    vi.mocked(api.answerOffer).mockResolvedValue({ state: "done", prompt: "Set up it" });
    render(<SuggestionOffer {...props} note={offer()} />);
    fireEvent.click(screen.getByRole("button", { name: "Make it a routine" }));
    await waitFor(() => expect(store.sendTurn).toHaveBeenCalledWith({ text: "Set up it" }));
    expect(api.answerOffer).toHaveBeenCalledWith("s1", "m1", true);
    expect(store.patchMessage).toHaveBeenCalled();
  });

  it("passes with Not now and sends nothing", async () => {
    vi.mocked(api.answerOffer).mockResolvedValue({ state: "dismissed" });
    render(<SuggestionOffer {...props} note={offer()} />);
    fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    await waitFor(() => expect(api.answerOffer).toHaveBeenCalledWith("s1", "m1", false));
    expect(store.sendTurn).not.toHaveBeenCalled();
  });

  it("reloads the conversation on a 409", async () => {
    vi.mocked(api.answerOffer).mockRejectedValue(new ApiError(409, "answered"));
    render(<SuggestionOffer {...props} note={offer()} />);
    fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    await waitFor(() => expect(store.openSession).toHaveBeenCalledWith("s1"));
  });

  it("shows a label and no buttons once answered", () => {
    render(<SuggestionOffer {...props} note={offer("dismissed")} />);
    expect(screen.getByText("Passed over")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("holds the buttons until the message has been read back", () => {
    render(<SuggestionOffer {...props} messageId={undefined} note={offer()} />);
    expect((screen.getByRole("button", { name: "Not now" }) as HTMLButtonElement).disabled).toBe(true);
  });
});
