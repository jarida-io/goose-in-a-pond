import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ApiError, type QuestionBatch } from "../api/types";

const api = vi.hoisted(() => ({
  answerQuestion: vi.fn(),
  skipQuestion: vi.fn(),
  questionsEnough: vi.fn(),
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { QuestionCard } from "./QuestionCard";

const plain = (label: string) => ({ label, description: "" });

const BATCH: QuestionBatch = {
  id: "b-1",
  purpose: "anchor",
  anchor_id: "a-diani",
  title: "Questions about Diani trip",
  questions: [
    {
      id: "q-budget",
      header: "Budget",
      text: "What is the budget?",
      options: [
        { label: "Under KSh 50,000", description: "Buses and guest houses" },
        {
          label: "KSh 50,000 to 100,000",
          description: "A mid-range hotel",
          preview: { kind: "draft", text: "Day 1: SGR to Mombasa, then a matatu to Diani." },
        },
      ],
      recommended: 1,
      multi: false,
    },
    {
      id: "q-eat",
      header: "Stay",
      text: "What should the stays have?",
      options: [plain("A pool"), plain("A kitchen"), plain("Wi-Fi")],
      multi: true,
    },
  ],
};

function renderCard(batch: QuestionBatch = BATCH, canSkip = true) {
  const said: string[] = [];
  const done = vi.fn();
  render(<QuestionCard batch={batch} variant="classic" canSkip={canSkip} say={(t) => said.push(t)} onDone={done} />);
  return { said, done };
}

const send = () => screen.getByRole("button", { name: "Send answer" }) as HTMLButtonElement;
const radioWords = () => screen.getAllByRole("radio").map((r) => r.textContent ?? "");

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.answerQuestion.mockResolvedValue(undefined);
  api.skipQuestion.mockResolvedValue(undefined);
  api.questionsEnough.mockResolvedValue(undefined);
});

describe("a single-choice question", () => {
  it("shows its header, Goose's pick first and marked, and an Other that opens a text field", async () => {
    renderCard();
    expect(screen.getByText("Question 1 of 2")).toBeTruthy();
    expect(screen.getByText("Budget")).toBeTruthy();
    const words = radioWords();
    expect(words[0]).toContain("KSh 50,000 to 100,000");
    expect(words[0]).toContain("Recommended");
    expect(words[0]).toContain("A mid-range hotel");
    expect(words[1]).toContain("Under KSh 50,000");
    expect(words[2]).toBe("Other");
    expect(screen.queryByRole("textbox", { name: "Your answer" })).toBeNull();
    expect(send().disabled).toBe(true);

    fireEvent.click(screen.getAllByRole("radio")[2]);
    const field = screen.getByRole("textbox", { name: "Your answer" });
    expect(send().disabled).toBe(true);
    fireEvent.change(field, { target: { value: "  About KSh 120,000 " } });
    fireEvent.click(send());

    await waitFor(() => expect(api.answerQuestion).toHaveBeenCalledWith("q-budget", { choices: [], text: "About KSh 120,000" }));
    expect(await screen.findByText("What should the stays have?")).toBeTruthy();
    expect(screen.getByText("Question 2 of 2")).toBeTruthy();
  });

  it("previews the option chosen, and sends it by its words", async () => {
    renderCard();
    expect(screen.queryByText(/Day 1: SGR/)).toBeNull();
    fireEvent.click(screen.getAllByRole("radio")[0]);
    expect(screen.getByText(/Day 1: SGR/)).toBeTruthy();
    fireEvent.click(screen.getAllByRole("radio")[1]);
    expect(screen.queryByText(/Day 1: SGR/)).toBeNull();
    fireEvent.click(screen.getAllByRole("radio")[0]);
    fireEvent.click(send());
    await waitFor(() => expect(api.answerQuestion).toHaveBeenCalledWith("q-budget", { choices: ["KSh 50,000 to 100,000"] }));
  });

  it("lets Tab reach the options before one is chosen", () => {
    renderCard();
    const radios = screen.getAllByRole("radio");
    expect(radios.every((r) => r.tabIndex === -1)).toBe(true);
    const stop = radios[0].closest(".mg-q__radios") as HTMLElement;
    expect(stop.tabIndex).toBe(0);
    stop.focus();
    expect(document.activeElement).toBe(radios[0]);
  });
});

describe("a many-choice question", () => {
  it("takes several checkboxes and Other together", async () => {
    const { said, done } = renderCard({ ...BATCH, questions: [BATCH.questions[1]] });
    fireEvent.click(screen.getByRole("checkbox", { name: "A pool" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Wi-Fi" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "A pool" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Other" }));
    expect(send().disabled).toBe(true);
    fireEvent.change(screen.getByRole("textbox", { name: "Your answer" }), { target: { value: "Near the reef" } });
    fireEvent.click(send());

    await waitFor(() => expect(api.answerQuestion).toHaveBeenCalledWith("q-eat", { choices: ["Wi-Fi"], text: "Near the reef" }));
    expect(said).toEqual(["Answer sent."]);
    expect(done).toHaveBeenCalled();
  });
});

describe("the pager, skip, Done and refusals", () => {
  it("pages to another question without answering the first", () => {
    renderCard();
    fireEvent.click(screen.getByRole("button", { name: "Page 2" }));
    expect(screen.getByText("What should the stays have?")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Page 1" }));
    expect(screen.getByText("What is the budget?")).toBeTruthy();
  });

  it("skips one question and moves on", async () => {
    const { said } = renderCard();
    fireEvent.click(screen.getByRole("button", { name: "Skip" }));
    await waitFor(() => expect(api.skipQuestion).toHaveBeenCalledWith("q-budget"));
    expect(said).toEqual(["Skipped."]);
    expect(await screen.findByText("What should the stays have?")).toBeTruthy();
  });

  it("says Done for the whole batch", async () => {
    const { said, done } = renderCard();
    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(api.questionsEnough).toHaveBeenCalledWith("b-1"));
    expect(said).toEqual(["Goose goes ahead with what it has."]);
    expect(done).toHaveBeenCalled();
  });

  it("offers no Skip or Done to a member the anchor is shared with", () => {
    renderCard(BATCH, false);
    expect(screen.queryByRole("button", { name: "Skip" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Done" })).toBeNull();
  });

  it("moves on from a question answered elsewhere meanwhile", async () => {
    api.answerQuestion.mockRejectedValueOnce(new ApiError(409, "this question is no longer open"));
    const { said } = renderCard();
    fireEvent.click(screen.getAllByRole("radio")[1]);
    fireEvent.click(send());
    expect(await screen.findByText("What should the stays have?")).toBeTruthy();
    expect(said).toEqual(["This question is no longer open."]);
  });

  it("keeps the question and says what to do when the answer is refused", async () => {
    api.answerQuestion.mockRejectedValueOnce(new ApiError(422, "choose an option or write an answer"));
    renderCard();
    fireEvent.click(screen.getAllByRole("radio")[1]);
    fireEvent.click(send());
    expect((await screen.findByRole("alert")).textContent).toBe("Choose an option or write an answer.");
    expect(screen.getByText("What is the budget?")).toBeTruthy();
  });
});
