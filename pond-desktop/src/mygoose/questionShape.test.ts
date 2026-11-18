import { describe, expect, it } from "vitest";
import { askedQuestions, isTheBatchAsked, shownOptions } from "./questionShape";

describe("questionShape", () => {
  it("puts Goose's pick first and marks it", () => {
    const shown = shownOptions({
      header: "Dates",
      text: "When?",
      options: [
        { label: "Friday", description: "" },
        { label: "Saturday", description: "" },
      ],
      recommended: 1,
      multi: false,
    });
    expect(shown.map((o) => [o.label, o.recommended])).toEqual([
      ["Saturday", true],
      ["Friday", false],
    ]);
  });

  it("reads a tool call's questions leniently and drops what it cannot show", () => {
    const asked = askedQuestions({
      questions: [
        { header: "Dates", text: "When?", options: ["Friday", { label: "Saturday", description: "A long weekend" }], recommended: 0 },
        { question: "Budget?", options: [{ label: "Under 20k" }] },
        { text: "", options: ["A"] },
        "not a question",
      ],
    });
    expect(asked).toEqual([
      {
        header: "Dates",
        text: "When?",
        options: [
          { label: "Friday", description: "" },
          { label: "Saturday", description: "A long weekend" },
        ],
        recommended: 0,
        multi: false,
      },
      { header: "", text: "Budget?", options: [{ label: "Under 20k", description: "" }], recommended: undefined, multi: false },
    ]);
    expect(askedQuestions(undefined)).toEqual([]);
  });

  it("knows the stored batch a call asked, though code refused some of it", () => {
    const asked = askedQuestions({ questions: [{ text: "When?", options: ["A"] }, { text: "Budget?", options: ["B"] }] });
    expect(isTheBatchAsked(asked, [{ text: " when? " }])).toBe(true);
    expect(isTheBatchAsked(asked, [{ text: "When?" }, { text: "Where?" }])).toBe(false);
    expect(isTheBatchAsked(asked, [])).toBe(false);
  });
});
