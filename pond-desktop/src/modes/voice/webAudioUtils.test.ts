import { describe, it, expect } from "vitest";
import { ReplySplitter, splitText, holdSecs, estimateTokens, UNIT_TOKEN_BUDGET, normalizeForSpeech, stripMarkdown, stripMarkupForSpeech, encodeWav, spokenClip } from "./webAudioUtils";
import type { SpeechUnit } from "./webAudioUtils";

const texts = (units: SpeechUnit[]) => units.map((u) => u.text);

/** A reply as the model streams it: each word, then the whitespace after it. */
function stream(reply: string): SpeechUnit[] {
  const s = new ReplySplitter();
  const units: SpeechUnit[] = [];
  for (const token of reply.match(/\S+|\s+/g) ?? []) units.push(...s.push(token));
  return [...units, ...s.finish()];
}

const SHORT =
  "Sure, I can help with that. It is 23 degrees in Nairobi, with light rain after four; " +
  "take a jacket. Shall I remind you?";

const LONG =
  "Bread needs four things: flour, water, salt and yeast. Mix them, then leave the dough to " +
  "rest for an hour, or longer if the kitchen is cold. Shape it gently, so the air stays in. " +
  "Bake it hot, at 230 degrees, until the crust is dark and the loaf sounds hollow when you " +
  "tap it. Let it cool before you cut it, however good it smells.";

// Mirrors the splitter tests in crates/pond-voice/src/text.rs.
describe("ReplySplitter", () => {
  it("says a reply one pass holds whole", () => {
    const s = new ReplySplitter();
    for (const token of SHORT.match(/\S+|\s+/g) ?? []) expect(s.push(token)).toEqual([]);
    expect(s.finish()).toEqual([{ text: SHORT, last: true }]);
    expect(splitText(SHORT)).toEqual([{ text: SHORT, last: true }]);
  });

  it("gives up a full unit while a reply past one pass streams", () => {
    const s = new ReplySplitter();
    const early: SpeechUnit[] = [];
    for (const token of LONG.match(/\S+|\s+/g) ?? []) early.push(...s.push(token));
    expect(early).toEqual([{
      text:
        "Bread needs four things: flour, water, salt and yeast. Mix them, then leave the dough " +
        "to rest for an hour, or longer if the kitchen is cold. Shape it gently, so the air stays in.",
      last: false,
    }]);
    expect(s.finish()).toEqual([{
      text:
        "Bake it hot, at 230 degrees, until the crust is dark and the loaf sounds hollow when " +
        "you tap it. Let it cool before you cut it, however good it smells.",
      last: true,
    }]);
  });

  it("fits every unit in a pass and marks only the closing one last", () => {
    const text = [LONG, LONG, SHORT].join(" ");
    const units = splitText(text);
    expect(units.length).toBeGreaterThanOrEqual(4);
    units.forEach((u, k) => {
      expect(estimateTokens(u.text)).toBeLessThanOrEqual(UNIT_TOKEN_BUDGET);
      expect(u.last).toBe(k === units.length - 1);
      expect(u.text).toMatch(/[.?]$/);
    });
    expect(texts(units).join(" ")).toBe(text);
  });

  it("cuts a streamed reply where it cuts the whole of it", () => {
    const text = [LONG, SHORT, LONG].join(" ");
    expect(stream(text)).toEqual(splitText(text));
  });

  it("says everything written so far when the model stops for a tool", () => {
    const s = new ReplySplitter();
    expect(s.push("Let me check the forecast")).toEqual([]);
    expect(s.pause()).toEqual([{ text: "Let me check the forecast", last: false }]);
    expect(s.pending).toBe("");
    expect(s.push(" It is dry until six.")).toEqual([]);
    expect(s.finish()).toEqual([{ text: "It is dry until six.", last: true }]);
    expect(s.finish()).toEqual([]);
  });

  it("cuts a long list at a line end", () => {
    const lines = Array.from({ length: 14 }, (_, k) => `${k + 1}. item number ${k + 1} of the list`);
    const units = splitText(lines.join("\n"));
    expect(units.length).toBeGreaterThan(1);
    for (const u of units) {
      expect(estimateTokens(u.text)).toBeLessThanOrEqual(UNIT_TOKEN_BUDGET);
      for (const line of u.text.split("\n")) expect(lines).toContain(line);
    }
  });

  it("does not end a unit at a number", () => {
    const text = `${"word ".repeat(30)} It costs 3. 5 or so, ${"more ".repeat(20)}`;
    expect(splitText(text)[0].text.endsWith("3.")).toBe(false);
  });

  it("cuts a sentence longer than a pass at a space, and an unbroken run at the limit", () => {
    const units = splitText("word ".repeat(80));
    expect(units.length).toBeGreaterThan(1);
    for (const u of units) {
      expect(estimateTokens(u.text)).toBeLessThanOrEqual(UNIT_TOKEN_BUDGET);
      expect(u.text.split(" ").every((w) => w === "word")).toBe(true);
    }
    const run = "a".repeat(181 * 2 + 5);
    const cut = splitText(run);
    expect(cut.length).toBe(3);
    expect(texts(cut).join("")).toBe(run);
  });
});

// Mirrors `a_hold_covers_the_unit_that_would_be_latest` in the pond's speech pipeline.
describe("holdSecs", () => {
  it("covers the unit that would be latest", () => {
    expect(holdSecs(10, [])).toBe(0);
    expect(holdSecs(10, [[4, 10]])).toBe(0);
    expect(holdSecs(10, [[14, 10]])).toBe(4);
    expect(holdSecs(10, [[14, 10], [14, 10]])).toBe(8);
    expect(holdSecs(10, [[14, 10], [2, 1]])).toBe(4);
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
