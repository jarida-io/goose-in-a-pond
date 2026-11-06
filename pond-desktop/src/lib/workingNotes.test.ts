import { describe, it, expect } from "vitest";
import { foldToolCall, type StepsCarrier } from "./workingNotes";

/** Pinned to the carrier type, so the optional fields can be read off a literal. */
const fold = (reply: StepsCarrier, notes?: string) => foldToolCall(reply, notes);

describe("foldToolCall", () => {
  it("moves the notes off the end of the answer into the steps", () => {
    const folded = fold({ text: "Let me check the weather. " }, "Let me check the weather. ");
    expect(folded.text).toBe("");
    expect(folded.steps).toEqual(["Let me check the weather."]);
  });

  it("lets the text after the last tool call stand as the answer", () => {
    let reply: StepsCarrier = { text: "Checking the weather." };
    reply = fold(reply, "Checking the weather.");
    reply = { ...reply, text: reply.text + "Now the news." };
    reply = fold(reply, "Now the news.");
    reply = { ...reply, text: reply.text + "It is sunny, and the match was a draw." };

    expect(reply.text).toBe("It is sunny, and the match was a draw.");
    expect(reply.steps).toEqual(["Checking the weather.", "Now the news."]);
  });

  it("takes the text since the last tool call when the notes are not its suffix", () => {
    // The client filtered something the server counted: the notes no longer end the answer.
    const folded = fold({ text: "Kept. Looking it up", stepsFrom: 6 }, "<b>Looking it up</b>");
    expect(folded.text).toBe("Kept. ");
    expect(folded.steps).toEqual(["Looking it up"]);
    expect(folded.stepsFrom).toBe(6);
  });

  it("never reaches back past the last tool call, even for a matching suffix", () => {
    const folded = fold({ text: "one two", stepsFrom: 4 }, "one two");
    expect(folded.text).toBe("one ");
    expect(folded.steps).toEqual(["two"]);
  });

  it("leaves the answer alone when the frame carries no notes", () => {
    const folded = fold({ text: "Let me check." });
    expect(folded.text).toBe("Let me check.");
    expect(folded.steps).toBeUndefined();
    expect(folded.stepsFrom).toBe("Let me check.".length);
  });

  it("drops a step that is only whitespace but still moves it out of the answer", () => {
    const folded = fold({ text: "\n\n" }, "\n\n");
    expect(folded.text).toBe("");
    expect(folded.steps).toBeUndefined();
  });
});
