import { describe, it, expect } from "vitest";
import { splitSpeechUnits, normalizeForSpeech, stripMarkdown, stripMarkupForSpeech } from "./webAudioUtils";

describe("splitSpeechUnits", () => {
  it("splits opening clause after minimum words when allowClauses is true", () => {
    const partial = "I would be glad to help with that, and here are the";
    const [units1, rem1] = splitSpeechUnits(partial, true);
    expect(units1).toEqual(["I would be glad to help with that,"]);
    expect(rem1).toBe("and here are the");

    const full = "I would be glad to help with that, and here are the details.";
    const [units2, rem2] = splitSpeechUnits(full, true);
    expect(units2).toEqual([
      "I would be glad to help with that,",
      "and here are the details."
    ]);
    expect(rem2).toBe("");
  });

  it("keeps short clauses with the sentence", () => {
    const text = "Sure, let's start the project.";
    const [units, rem] = splitSpeechUnits(text, true);
    expect(units).toEqual(["Sure, let's start the project."]);
    expect(rem).toBe("");
  });

  it("does not split on numeric commas", () => {
    const text = "There are 1,000 units ready.";
    const [units, rem] = splitSpeechUnits(text, true);
    expect(units).toEqual(["There are 1,000 units ready."]);
    expect(rem).toBe("");
  });

  it("does not split on numeric decimals or abbreviations", () => {
    const text = "Dr. Smith noted the temperature was 25.3 degrees Celsius.";
    const [units, rem] = splitSpeechUnits(text, true);
    expect(units).toEqual(["Dr. Smith noted the temperature was 25.3 degrees Celsius."]);
    expect(rem).toBe("");
  });

  it("does not split on streaming decimal trailing period or empty clause tail", () => {
    const partialDec = "The temperature is 25.";
    const [units1, rem1] = splitSpeechUnits(partialDec, true);
    expect(units1).toEqual([]);
    expect(rem1).toBe("The temperature is 25.");

    const partialClause = "I would be glad to help with that,";
    const [units2, rem2] = splitSpeechUnits(partialClause, true);
    expect(units2).toEqual([]);
    expect(rem2).toBe("I would be glad to help with that,");
  });

  it("splits on terminals and newlines", () => {
    const text = "First sentence! Second sentence? Third sentence.\nFourth line.";
    const [units, rem] = splitSpeechUnits(text, false);
    expect(units).toEqual([
      "First sentence!",
      "Second sentence?",
      "Third sentence.",
      "Fourth line."
    ]);
    expect(rem).toBe("");
  });

  it("does not split on short unpunctuated header or single newline", () => {
    const text = "Weather Updates\nThe temperature is 25.3 degrees Celsius.";
    const [units, rem] = splitSpeechUnits(text, true);
    expect(units).toEqual(["Weather Updates\nThe temperature is 25.3 degrees Celsius."]);
    expect(rem).toBe("");
  });

  it("collapses huge whitespace and tabs at boundary", () => {
    const text = "First sentence.\t\t\n\n   Second sentence.";
    const [units, rem] = splitSpeechUnits(text, false);
    expect(units).toEqual(["First sentence.", "Second sentence."]);
    expect(rem).toBe("");
  });

  it("splits paragraph break when word count is sufficient", () => {
    const text = "This is a long introductory paragraph without period\n\nSecond paragraph.";
    const [units, rem] = splitSpeechUnits(text, false);
    expect(units).toEqual([
      "This is a long introductory paragraph without period",
      "Second paragraph."
    ]);
    expect(rem).toBe("");
  });
});

describe("normalizeForSpeech", () => {
  it("normalizes Kenyan currency prefix and suffix", () => {
    expect(normalizeForSpeech("dollar to KSH rate")).toBe("dollar to Kenyan shillings rate");
    expect(normalizeForSpeech("exchange is 129.83 KES")).toBe("exchange is 129.83 Kenyan shillings");
    expect(normalizeForSpeech("costs KSh 130 per unit")).toBe("costs 130 Kenyan shillings per unit");
  });
});

describe("stripMarkupForSpeech", () => {
  // Replies a guest's voice turns began with on the nano (2026-11-02), verbatim.
  it("never speaks a tool call written as text", () => {
    for (const leaked of [
      `<giap-knowledge__search_web question="what to pack for a day hike" />`,
      `<giap-knowledge__get_wikipedia_article topic="Why is the sky blue?"/>`,
      `<knowledge__get_wikipedia_article topic="why is the sky blue?"/`,
      `<call>giap-knowledge__get_wikipedia_article{topic:"crocodile vs alligator"}`,
      `<giap-knowledge__get_wikipedia_article topic="Why is the sky blue?`,
    ]) {
      expect(stripMarkdown(leaked), leaked).toBe("");
    }
  });

  it("drops an echoed scaffold block with its body and keeps the answer", () => {
    expect(
      stripMarkdown("<answer-contract>\nReply with the result.\n</answer-contract>\nThe sky scatters blue light."),
    ).toBe("The sky scatters blue light.");
  });

  it("leaves comparisons and dunder names alone", () => {
    expect(stripMarkupForSpeech("3 < 5 and 7 > 2")).toBe("3 < 5 and 7 > 2");
    expect(stripMarkupForSpeech("The __init__ method runs first.")).toBe("The __init__ method runs first.");
    expect(stripMarkupForSpeech("I <3 tea.")).toBe("I <3 tea.");
  });
});
