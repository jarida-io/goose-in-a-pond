import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { QuestionBatch } from "../api/types";
import type { ContextCard } from "../state/reducer";

const api = vi.hoisted(() => ({
  listQuestionBatches: vi.fn(),
  answerQuestion: vi.fn(),
  skipQuestion: vi.fn(),
  questionsEnough: vi.fn(),
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { AskQuestionsCard } from "./AskQuestionsCard";

const INPUT = {
  questions: [
    {
      header: "Dates",
      text: "When do you want to go?",
      options: [
        { label: "12-15 Dec", description: "A long weekend" },
        { label: "19-22 Dec", description: "Before Christmas" },
      ],
      recommended: 1,
    },
  ],
};

const STORED: QuestionBatch = {
  id: "b-chat",
  purpose: "chat",
  anchor_id: null,
  session_id: "s-1",
  round: 1,
  title: "Trip planning",
  state: "open",
  questions: [{ id: "q-dates", ...INPUT.questions[0], multi: false }],
};

function card(result?: string): ContextCard {
  return {
    id: 1,
    tool: "giap-questions__ask",
    callId: "call-1",
    data: result === undefined ? {} : { result },
    timestamp_ms: 0,
    input: INPUT,
  };
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.answerQuestion.mockResolvedValue(undefined);
});

describe("AskQuestionsCard", () => {
  it("draws the questions from the call at once, then the stored batch to answer", async () => {
    let found: (batches: QuestionBatch[]) => void = () => {};
    api.listQuestionBatches.mockReturnValueOnce(new Promise((resolve) => (found = resolve)));
    render(<AskQuestionsCard card={card()} sessionId="s-1" />);
    expect(screen.getByText("When do you want to go?")).toBeTruthy();
    expect(screen.getByText("Getting the questions ready...")).toBeTruthy();
    expect(screen.queryByRole("radio")).toBeNull();

    found([STORED]);
    expect(await screen.findByText("Trip planning")).toBeTruthy();
    expect(api.listQuestionBatches).toHaveBeenCalledWith("s-1");
    fireEvent.click(screen.getAllByRole("radio")[0]);
    fireEvent.click(screen.getByRole("button", { name: "Send answer" }));
    await waitFor(() => expect(api.answerQuestion).toHaveBeenCalledWith("q-dates", { choices: ["19-22 Dec"] }));
  });

  it("shows the answers once the tool has them", () => {
    render(<AskQuestionsCard card={card("Dates: 19-22 Dec")} sessionId="s-1" />);
    expect(screen.getByText("Your answers")).toBeTruthy();
    expect(screen.getByText("Dates: 19-22 Dec")).toBeTruthy();
    expect(api.listQuestionBatches).not.toHaveBeenCalled();
  });
});
