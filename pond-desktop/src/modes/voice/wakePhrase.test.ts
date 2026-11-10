import { describe, expect, it } from "vitest";
import { asksForAnAnswer, buildCalibrationPrompts, containsTrigger, estimateSounds, normalizeTranscript, wakePhraseError, wakePhraseWarning } from "./wakePhrase";

describe("asksForAnAnswer", () => {
  it("waits for an answer only after a question", () => {
    expect(asksForAnAnswer("Which room do you mean?")).toBe(true);
    expect(asksForAnAnswer("Shall I set it for **seven**?** \n")).toBe(true);
    expect(asksForAnAnswer("Do you want the long version (it takes a minute)?)")).toBe(true);
    expect(asksForAnAnswer("It is sunny and 24 degrees.")).toBe(false);
    expect(asksForAnAnswer("Did you know? Geese mate for life.")).toBe(false);
    expect(asksForAnAnswer("")).toBe(false);
  });
});

const fires = (transcript: string, trigger: string) =>
  containsTrigger(normalizeTranscript(transcript), normalizeTranscript(trigger));

describe("wake word matching", () => {
  it("never fires inside a longer word", () => {
    expect(fires("A mongoose ran past", "goose")).toBe(false);
    expect(fires("gooseberry jam", "goose")).toBe(false);
  });

  it("fires on whole words whatever whisper's punctuation", () => {
    expect(fires("Goose, what's the weather?", "goose")).toBe(true);
    expect(fires("Hey, GOOSE!", "hey goose")).toBe(true);
  });

  it("needs a multi-word trigger's words adjacent and in order", () => {
    expect(fires("goose hey", "hey goose")).toBe(false);
    expect(fires("hey there goose", "hey goose")).toBe(false);
  });

  it("normalizes as the server does", () => {
    expect(normalizeTranscript("  Hey,  Goose! ")).toBe("hey goose");
    expect(normalizeTranscript("it's 5 o'clock")).toBe("it s o clock");
  });
});

describe("wake phrase rules", () => {
  it("accepts sayable words, names and Swahili", () => {
    for (const ok of ["goose", "Hey Goose", "habari jarida", "o'malley"]) expect(wakePhraseError(ok)).toBeNull();
  });

  it("refuses what the detector could never match", () => {
    for (const bad of ["goose!!", "hey 2", "one two three four five", "", "x"]) expect(wakePhraseError(bad)).not.toBeNull();
  });

  it("estimates speech sounds like espeak on the common phrases", () => {
    const sounds = (p: string) => p.split(" ").reduce((n, w) => n + estimateSounds(w), 0);
    expect(sounds("goose")).toBe(3);
    expect(sounds("hey goose")).toBe(5);
    expect(sounds("okay goose")).toBe(6);
    expect(sounds("alexa")).toBe(6);
  });

  it("warns, without blocking, about a short phrase and suggests an opener", () => {
    expect(wakePhraseWarning("goose")).toMatch(/hey goose/);
    expect(wakePhraseWarning("hey goose")).not.toMatch(/Try/);
    expect(wakePhraseWarning("okay goose")).toBeNull();
  });
});

describe("calibration prompts", () => {
  it("match the server's list, which locates the phrase in a take", () => {
    // The same literals are asserted in crates/pond-voice/src/text.rs.
    expect(buildCalibrationPrompts("goose")).toEqual([
      "goose", "Hey goose, good morning", "OK goose, what time is it", "goose, tell me a joke", "Thank you goose",
    ]);
  });
});
