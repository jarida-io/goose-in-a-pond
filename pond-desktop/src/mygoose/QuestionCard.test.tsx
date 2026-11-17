import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ApiError, type QuestionBatch } from "../api/types";

const api = vi.hoisted(() => ({
  answerQuestion: vi.fn(),
  skipQuestion: vi.fn(),
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { QuestionCard } from "./QuestionCard";

const BATCH: QuestionBatch = {
  id: "b-1",
  purpose: "anchor",
  anchor_id: "a-diani",
  title: "Questions about Diani trip",
  questions: [
    { id: "q-budget", text: "What is the budget?", options: ["Under KSh 50,000", "KSh 50,000 to 100,000"], multi: false },
    { id: "q-eat", text: "What should the stays have?", options: ["A pool", "A kitchen", "Wi-Fi"], multi: true },
  ],
};

function renderCard(batch: QuestionBatch = BATCH, canSkip = true) {
  const said: string[] = [];
  const done = vi.fn();
  render(<QuestionCard batch={batch} variant="classic" canSkip={canSkip} say={(t) => said.push(t)} onDone={done} />);
  return { said, done };
}

const send = () => screen.getByRole("button", { name: "Send answer" }) as HTMLButtonElement;

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.answerQuestion.mockResolvedValue(undefined);
  api.skipQuestion.mockResolvedValue(undefined);
});

describe("a single-choice question", () => {
  it("offers its options and an Other that opens a text field, and sends the words as the answer", async () => {
    renderCard();
    expect(screen.getByText("Question 1 of 2")).toBeTruthy();
    expect(screen.getAllByRole("radio").map((r) => r.textContent)).toEqual(["Under KSh 50,000", "KSh 50,000 to 100,000", "Other"]);
    expect(screen.queryByRole("textbox", { name: "Your answer" })).toBeNull();
    expect(send().disabled).toBe(true);

    fireEvent.click(screen.getByRole("radio", { name: "Other" }));
    const field = screen.getByRole("textbox", { name: "Your answer" });
    expect(send().disabled).toBe(true);
    fireEvent.change(field, { target: { value: "  About KSh 120,000 " } });
    fireEvent.click(send());

    await waitFor(() => expect(api.answerQuestion).toHaveBeenCalledWith("q-budget", { choices: [], text: "About KSh 120,000" }));
    expect(await screen.findByText("What should the stays have?")).toBeTruthy();
    expect(screen.getByText("Question 2 of 2")).toBeTruthy();
  });

  it("sends a chosen option by its words, and drops the Other field when another is chosen", async () => {
    renderCard();
    fireEvent.click(screen.getByRole("radio", { name: "Other" }));
    fireEvent.click(screen.getByRole("radio", { name: "KSh 50,000 to 100,000" }));
    expect(screen.queryByRole("textbox", { name: "Your answer" })).toBeNull();
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
  it("takes several pills and Other together", async () => {
    const { said, done } = renderCard({ ...BATCH, questions: [BATCH.questions[1]] });
    fireEvent.click(screen.getByRole("button", { name: "A pool" }));
    fireEvent.click(screen.getByRole("button", { name: "Wi-Fi" }));
    fireEvent.click(screen.getByRole("button", { name: "A pool" }));
    fireEvent.click(screen.getByRole("button", { name: "Other" }));
    expect(screen.getByRole("button", { name: "Other" }).getAttribute("aria-pressed")).toBe("true");
    expect(send().disabled).toBe(true);
    fireEvent.change(screen.getByRole("textbox", { name: "Your answer" }), { target: { value: "Near the reef" } });
    fireEvent.click(send());

    await waitFor(() => expect(api.answerQuestion).toHaveBeenCalledWith("q-eat", { choices: ["Wi-Fi"], text: "Near the reef" }));
    expect(said).toEqual(["Answer sent."]);
    expect(done).toHaveBeenCalled();
  });
});

describe("skip and refusals", () => {
  it("skips one question and moves on", async () => {
    const { said } = renderCard();
    fireEvent.click(screen.getByRole("button", { name: "Skip" }));
    await waitFor(() => expect(api.skipQuestion).toHaveBeenCalledWith("q-budget"));
    expect(said).toEqual(["Skipped."]);
    expect(await screen.findByText("What should the stays have?")).toBeTruthy();
  });

  it("offers no Skip to a member the anchor is shared with", () => {
    renderCard(BATCH, false);
    expect(screen.queryByRole("button", { name: "Skip" })).toBeNull();
  });

  it("moves on from a question answered elsewhere meanwhile", async () => {
    api.answerQuestion.mockRejectedValueOnce(new ApiError(409, "this question is no longer open"));
    const { said } = renderCard();
    fireEvent.click(screen.getByRole("radio", { name: "Under KSh 50,000" }));
    fireEvent.click(send());
    expect(await screen.findByText("What should the stays have?")).toBeTruthy();
    expect(said).toEqual(["This question is no longer open."]);
  });

  it("keeps the question and says what to do when the answer is refused", async () => {
    api.answerQuestion.mockRejectedValueOnce(new ApiError(422, "choose an option or write an answer"));
    renderCard();
    fireEvent.click(screen.getByRole("radio", { name: "Under KSh 50,000" }));
    fireEvent.click(send());
    expect((await screen.findByRole("alert")).textContent).toBe("Choose an option or write an answer.");
    expect(screen.getByText("What is the budget?")).toBeTruthy();
  });
});
