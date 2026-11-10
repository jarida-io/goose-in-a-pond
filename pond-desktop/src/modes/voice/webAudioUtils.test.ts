import { describe, it, expect } from "vitest";
import { ReplySplitter, splitText, estimateTokens, UNIT_TOKEN_BUDGET, normalizeForSpeech, stripMarkdown, stripMarkupForSpeech, encodeWav, spokenClip } from "./webAudioUtils";
import type { SpeechUnit } from "./webAudioUtils";

const texts = (units: SpeechUnit[]) => units.map((u) => u.text);
const bounds = (units: SpeechUnit[]) => units.map((u) => u.boundary);

/** A reply as the model streams it: each word, then the whitespace after it. */
function stream(reply: string, behind = false): SpeechUnit[] {
  const s = new ReplySplitter();
  const units: SpeechUnit[] = [];
  for (const token of reply.match(/\S+|\s+/g) ?? []) units.push(...s.push(token, behind));
  return [...units, ...s.finish()];
}

// Mirrors the splitter tests in crates/pond-voice/src/text.rs.
describe("ReplySplitter", () => {
  it("splits a whole text at sentences", () => {
    const units = splitText("Hello world. How are you? I'm fine!\nNew paragraph.");
    expect(texts(units)).toEqual(["Hello world.", "How are you?", "I'm fine!", "New paragraph."]);
    expect(bounds(units)).toEqual(["sentence", "sentence", "sentence", "end"]);
  });

  it("ends the first unit at its first clause after three words", () => {
    const units = splitText("I would be glad to help with that, and here are the details.");
    expect(texts(units)).toEqual(["I would be glad to help with that,", "and here are the details."]);
    expect(bounds(units)).toEqual(["clause", "end"]);
    expect(texts(splitText("Sure, let's start the project."))).toEqual(["Sure, let's start the project."]);
  });

  it("keeps later sentences whole unless the caller is behind", () => {
    const reply = "Sure. It is sunny, and the high is 26 degrees, with wind from the east.";
    expect(texts(stream(reply))).toEqual([
      "Sure.",
      "It is sunny, and the high is 26 degrees, with wind from the east.",
    ]);
    expect(texts(stream(reply, true))).toEqual([
      "Sure.",
      "It is sunny,",
      "and the high is 26 degrees,",
      "with wind from the east.",
    ]);
  });

  it("does not end units at numbers or abbreviations", () => {
    for (const text of [
      "There are 1,000 units ready.",
      "The temperature is 25.3 degrees Celsius.",
      "Dr. Smith arrived at 7:30 a.m. today.",
    ]) {
      expect(texts(stream(text, true))).toEqual([text]);
    }
  });

  it("waits at the end of the buffer for what follows a mark", () => {
    let s = new ReplySplitter();
    expect(s.push("The temperature is 25.", true)).toEqual([]);
    s = new ReplySplitter();
    expect(s.push("I would be glad to help with that,", true)).toEqual([]);
    s = new ReplySplitter();
    expect(s.push("First one.")).toEqual([]);
    expect(s.push("\n\n")).toEqual([]);
    expect(s.push("Second")).toEqual([{ text: "First one.", boundary: "paragraph" }]);
    expect(s.finish()).toEqual([{ text: "Second", boundary: "end" }]);
  });

  it("says whole sentences on a pause and keeps an unfinished one", () => {
    const s = new ReplySplitter();
    expect(s.push("Let me check the weather.")).toEqual([]);
    expect(s.pause()).toEqual([{ text: "Let me check the weather.", boundary: "sentence" }]);
    expect(s.push(" It is")).toEqual([]);
    expect(s.pause()).toEqual([]);
    expect(texts(s.finish())).toEqual(["It is"]);
  });

  it("keeps the first unit whole when no clause mark precedes the sentence end", () => {
    const units = stream("The weather in Nairobi today is expected to be warm and mostly sunny all afternoon. Then");
    expect(texts(units)).toEqual([
      "The weather in Nairobi today is expected to be warm and mostly sunny all afternoon.",
      "Then",
    ]);
    expect(units[0].boundary).toBe("sentence");
    expect(texts(stream("Here is a simple chapati recipe for about eight pieces. More."))).toEqual([
      "Here is a simple chapati recipe for about eight pieces.",
      "More.",
    ]);
  });

  it("reads a number starting a line as a list marker", () => {
    const units = splitText("Here is how.\n1. Mix the flour, salt and sugar.\n2. Add the oil.");
    expect(texts(units)).toEqual(["Here is how.", "1. Mix the flour, salt and sugar.", "2. Add the oil."]);
    expect(bounds(units)).toEqual(["list", "list", "end"]);
  });

  it("ends units at list lines and adds a comma when unpunctuated", () => {
    const units = splitText("You will need:\n- 3 cups of flour\n- 1 teaspoon of salt\n\nMix them well.");
    expect(texts(units)).toEqual(["You will need:", "- 3 cups of flour,", "- 1 teaspoon of salt", "Mix them well."]);
    expect(bounds(units)).toEqual(["list", "list", "paragraph", "end"]);
  });

  it("does not break at a wrapped line of prose", () => {
    const text = "This is a long sentence that the model\nwrapped onto a second line.";
    expect(texts(splitText(`Okay. ${text}`))).toEqual(["Okay.", text]);
    const header = "Weather Updates\nThe temperature is 25.3 degrees Celsius.";
    expect(texts(splitText(header))).toEqual([header]);
  });

  it("ends a unit at a blank line however short", () => {
    const units = splitText("Ingredients\n\nTwo cups.\t\t\n\n   Then knead.");
    expect(texts(units)).toEqual(["Ingredients", "Two cups.", "Then knead."]);
    expect(bounds(units)).toEqual(["paragraph", "paragraph", "end"]);
  });

  it("cuts a sentence past the budget at its last clause", () => {
    const clause = "and then the dough is folded over again";
    const units = stream(`Okay. ${Array(8).fill(clause).join(", ")}.`);
    expect(units.length).toBeGreaterThanOrEqual(3);
    for (const u of units.slice(1, -1)) {
      expect(estimateTokens(u.text)).toBeLessThanOrEqual(UNIT_TOKEN_BUDGET);
      expect(u.boundary).toBe("clause");
    }
  });

  it("cuts a streamed reply where it cuts the whole of it", () => {
    const reply =
      "Herons hunt by standing still for a long time, then striking fast. " +
      "They eat fish, frogs and insects, and some even bait the water with bread.\n\n" +
      "Dr. Smith counted 1,000 of them at 7:30 a.m. near the lake.";
    const streamed = stream(reply);
    expect(streamed).toEqual(splitText(reply));
    expect(texts(streamed)).toEqual([
      "Herons hunt by standing still for a long time,",
      "then striking fast.",
      "They eat fish, frogs and insects, and some even bait the water with bread.",
      "Dr. Smith counted 1,000 of them at 7:30 a.m. near the lake.",
    ]);
    expect(streamed[2].boundary).toBe("paragraph");
  });
});

describe("stripMarkdown list lines", () => {
  // Single newlines survive stripMarkdown; the voice hears them as spaces.
  const spoken = (md: string) => stripMarkdown(md).replace(/\s+/g, " ");

  it("joins unpunctuated list lines with a comma", () => {
    expect(spoken("- 3 cups of flour\n- 1 teaspoon of salt\n- water")).toBe(
      "3 cups of flour, 1 teaspoon of salt, water",
    );
    expect(spoken("You will need\n- flour")).toBe("You will need, flour");
    expect(spoken("A long line the model\nwrapped here.")).toBe("A long line the model wrapped here.");
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

  it("drops an invented snake_case block with its body", () => {
    expect(stripMarkdown("<search_web>What is the story of the Rift Valley</search_web>")).toBe("");
    expect(stripMarkdown("<turn_plan>Search first.</turn_plan> It formed slowly.")).toBe("It formed slowly.");
  });

  it("leaves comparisons and dunder names alone", () => {
    expect(stripMarkupForSpeech("3 < 5 and 7 > 2")).toBe("3 < 5 and 7 > 2");
    expect(stripMarkupForSpeech("The __init__ method runs first.")).toBe("The __init__ method runs first.");
    expect(stripMarkupForSpeech("I <3 tea.")).toBe("I <3 tea.");
  });
});

describe("spokenClip", () => {
  it("carries the recording as raw base64 with the words heard in it", () => {
    const wav = encodeWav(new Float32Array(48_000).fill(0.25), 16_000);
    const clip = spokenClip("what time is it", wav);
    expect(clip.mime_type).toBe("audio/wav");
    expect(clip.heard_as).toBe("what time is it");
    expect(clip.data.startsWith("data:")).toBe(false);
    expect(Buffer.from(clip.data, "base64").equals(Buffer.from(wav))).toBe(true);
  });
});
