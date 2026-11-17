import { describe, expect, it } from "vitest";
import { ApiError } from "../api/types";
import { anchor, factLine } from "./myGooseFixtures";
import { anchorMeta, badgeWords, canOpen, deliverableStateWords, factGroups, roundsWords, toolWords, whenWords } from "./myGooseWords";
import { isNotServed, isUnknownCaller, problemWords } from "./problems";

describe("what the screens say", () => {
  it("puts kind, dates and place on one line, leaving out what is missing", () => {
    expect(anchorMeta(anchor({ place: "Diani" }))).toBe("Trip · Diani");
    expect(whenWords(null, null)).toBeNull();
    const one = whenWords("2026-12-03T09:00:00Z", "2026-12-03T17:00:00Z");
    expect(one).not.toContain(" to ");
    expect(whenWords("2026-12-03T09:00:00Z", "2026-12-06T09:00:00Z")).toContain(" to ");
  });

  it("names a refresh of something already made apart from its first making", () => {
    expect(deliverableStateWords({ state: "making", version: 0 })).toBe("Being made");
    expect(deliverableStateWords({ state: "making", version: 2 })).toBe("Ready, a fresh version is being made");
    expect(canOpen({ state: "making", version: 0 })).toBe(false);
    expect(canOpen({ state: "queued", version: 1 })).toBe(true);
    expect(canOpen({ state: "rejected", version: 1 })).toBe(false);
  });

  it("counts what is new and what is asked, and says nothing when there is neither", () => {
    expect(badgeWords(anchor())).toEqual([]);
    expect(roundsWords(0)).toBeNull();
    expect(roundsWords(3)).toBe("Checked 3 times");
  });

  it("never shows a tool by its id", () => {
    expect(toolWords("giap-weather__get_forecast")).toBe("get forecast");
    expect(toolWords("search_web")).toBe("search web");
  });

  it("groups the sheet in category order with pinned lines first", () => {
    const groups = factGroups([
      factLine({ id: "1", category: "food", fact: "Fish" }),
      factLine({ id: "2", category: "home", fact: "Kilimani" }),
      factLine({ id: "3", category: "food", fact: "Vegetarian", pinned: true }),
    ]);
    expect(groups.map((g) => g.label)).toEqual(["Home", "Food"]);
    expect(groups[1].lines.map((l) => l.fact)).toEqual(["Vegetarian", "Fish"]);
  });
});

describe("refusals", () => {
  it("tells an unknown caller and an older pond from an ordinary refusal", () => {
    expect(isUnknownCaller(new ApiError(409, "who_are_you: this phone is not linked to a member yet"))).toBe(true);
    expect(isUnknownCaller(new ApiError(409, "this is over: add it again if you still want it"))).toBe(false);
    expect(isNotServed(new ApiError(404, "Not Found"))).toBe(true);
    expect(isNotServed(new ApiError(404, "no such anchor", undefined, { error: "no such anchor" }))).toBe(false);
  });

  it("gives the pond's own reason as a sentence, and says what to do when it has none", () => {
    expect(problemWords(new ApiError(422, "it needs a title: say what it is in a few words"))).toBe(
      "It needs a title: say what it is in a few words.",
    );
    expect(problemWords(new ApiError(500, "database is locked"))).toBe("The pond couldn't do that just now. Try again in a moment.");
    expect(problemWords(new ApiError(404, "no such anchor", undefined, { error: "no such anchor" }))).not.toMatch(/anchor/);
    expect(problemWords(new TypeError("Failed to fetch"))).toBe("Goose couldn't reach the pond. Check that it's on, then try again.");
  });
});
