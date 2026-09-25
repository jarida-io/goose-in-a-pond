import { describe, it, expect } from "vitest";
import {
  describeVoice,
  groupVoices,
  paceLabel,
  clampPace,
  describeQuality,
  voiceTitle,
  gradeFor,
  gradeRank,
  statementAt,
  PREVIEW_STATEMENTS,
  qualityAdvice,
  recommendedQuality,
  ONBOARDING_QUALITY,
  VOICE_QUALITY_TIERS,
  DEFAULT_QUALITY,
  MIN_PACE,
  MAX_PACE,
  DEFAULT_PACE,
} from "./voiceCatalogue";

describe("describeVoice", () => {
  it("reads accent and gender out of the id", () => {
    expect(describeVoice("af_heart")).toMatchObject({
      id: "af_heart",
      name: "Heart",
      language: "American English",
      gender: "Female",
      group: "American English · Female",
    });
    expect(describeVoice("bm_george")).toMatchObject({
      name: "George",
      language: "British English",
      gender: "Male",
    });
  });

  it("handles the non-English voices the repo also ships", () => {
    expect(describeVoice("jf_alpha").language).toBe("Japanese");
    expect(describeVoice("zf_xiaoni").language).toBe("Mandarin");
    expect(describeVoice("ef_dora").language).toBe("Spanish");
  });

  it("title-cases multi-word names", () => {
    expect(describeVoice("jf_gongitsune").name).toBe("Gongitsune");
    expect(describeVoice("am_van_dyke").name).toBe("Van Dyke");
  });

  it("keeps an unrecognised id usable instead of dropping it", () => {
    const v = describeVoice("weird-new-voice");
    expect(v.id).toBe("weird-new-voice");
    expect(v.group).toBe("Other");
    expect(v.name.length).toBeGreaterThan(0);

    const unknownLang = describeVoice("qf_thing");
    expect(unknownLang.id).toBe("qf_thing");
    expect(unknownLang.group).toBe("Other");
  });
});

describe("groupVoices", () => {
  it("puts English first and Other last", () => {
    const groups = groupVoices(["zf_xiaoni", "mystery", "bm_george", "af_heart"]);
    expect(groups[0].group).toBe("American English · Female");
    expect(groups[1].group).toBe("British English · Male");
    expect(groups[groups.length - 1].group).toBe("Other");
  });

  it("sorts voices by name inside a group and loses none", () => {
    const groups = groupVoices(["af_sky", "af_bella", "af_heart"]);
    expect(groups).toHaveLength(1);
    expect(groups[0].voices.map((v) => v.name)).toEqual(["Bella", "Heart", "Sky"]);
  });

  it("is empty for no voices rather than throwing", () => {
    expect(groupVoices([])).toEqual([]);
  });
});

describe("pace", () => {
  it("clamps to what the engine accepts", () => {
    expect(clampPace(0.1)).toBe(MIN_PACE);
    expect(clampPace(9)).toBe(MAX_PACE);
    expect(clampPace(1.25)).toBe(1.25);
  });

  /// A NaN reaching the slider would render an empty handle and persist junk.
  it("falls back to the default for a non-number", () => {
    expect(clampPace(Number.NaN)).toBe(DEFAULT_PACE);
    expect(clampPace(Number.POSITIVE_INFINITY)).toBe(DEFAULT_PACE);
  });

  it("labels the default as natural", () => {
    expect(paceLabel(DEFAULT_PACE)).toBe("Natural");
  });

  it("labels every point in range without a gap", () => {
    for (let p = MIN_PACE; p <= MAX_PACE + 0.001; p += 0.05) {
      expect(paceLabel(p)).toBeTruthy();
    }
  });
});

describe("quality tiers", () => {
  it("defaults to q8 when unset or unknown", () => {
    expect(describeQuality(undefined).value).toBe(DEFAULT_QUALITY);
    expect(describeQuality("nonsense").value).toBe(DEFAULT_QUALITY);
    expect(describeQuality("").value).toBe(DEFAULT_QUALITY);
  });

  it("resolves a real tier", () => {
    expect(describeQuality("fp32").label).toBe("Reference");
  });

  /// The size is shown before a download starts, so every tier needs one.
  it("gives every tier a label, detail and size", () => {
    for (const t of VOICE_QUALITY_TIERS) {
      expect(t.label).toBeTruthy();
      expect(t.detail).toBeTruthy();
      expect(t.sizeMb).toBeGreaterThan(0);
    }
  });

  it("offers the shipping default first", () => {
    expect(VOICE_QUALITY_TIERS[0].value).toBe(DEFAULT_QUALITY);
  });
});

describe("voiceTitle", () => {
  it("titles a voice id for the models list", () => {
    expect(voiceTitle("af_heart")).toBe("Af_Heart");
    expect(voiceTitle("bm_george")).toBe("Bm_George");
    expect(voiceTitle("am_michael")).toBe("Am_Michael");
  });

  // The engine resolves `<name>.bin` from the lowercase id, so titling must not touch it.
  it("does not alter the id it was given", () => {
    const id = "af_heart";
    voiceTitle(id);
    expect(id).toBe("af_heart");
  });

  it("handles ids with more or fewer underscores", () => {
    expect(voiceTitle("jf_gongitsune")).toBe("Jf_Gongitsune");
    expect(voiceTitle("solo")).toBe("Solo");
    expect(voiceTitle("a_b_c")).toBe("A_B_C");
  });
});

describe("grades", () => {
  // A wrong grade is worse than none: it changes which voice a household picks.
  it("matches the published table", () => {
    expect(gradeFor("af_heart")).toBe("A");
    expect(gradeFor("af_bella")).toBe("A-");
    expect(gradeFor("am_adam")).toBe("F+");
    expect(gradeFor("bf_emma")).toBe("B-");
  });

  it("has no grade for a voice the table does not list", () => {
    expect(gradeFor("zf_xiaoni")).toBeNull();
    expect(gradeFor("nonsense")).toBeNull();
  });

  it("ranks better grades first and sorts ungraded voices last", () => {
    expect(gradeRank("af_heart")).toBeLessThan(gradeRank("af_bella"));
    expect(gradeRank("af_bella")).toBeLessThan(gradeRank("bf_emma"));
    expect(gradeRank("bf_emma")).toBeLessThan(gradeRank("am_adam"));
    expect(gradeRank("zf_xiaoni")).toBeGreaterThan(gradeRank("am_adam"));
  });

  it("orders modifiers within a letter", () => {
    // C+ is better than C, which is better than C-.
    expect(gradeRank("af_aoede")).toBeLessThan(gradeRank("af_alloy"));
    expect(gradeRank("af_alloy")).toBeLessThan(gradeRank("af_sky"));
  });
});

describe("preview statements", () => {
  it("cycles rather than repeating or randomising", () => {
    const n = PREVIEW_STATEMENTS.length;
    expect(statementAt(0)).not.toBe(statementAt(1));
    // Deterministic and wrapping, so two voices can be compared on the same line.
    expect(statementAt(n)).toBe(statementAt(0));
    expect(statementAt(n + 3)).toBe(statementAt(3));
  });

  it("reads as things this assistant would actually say", () => {
    expect(PREVIEW_STATEMENTS.length).toBeGreaterThan(3);
    for (const line of PREVIEW_STATEMENTS) {
      expect(line.trim()).toBe(line);
      expect(line.length).toBeGreaterThan(15);
      expect(/[.!?]$/.test(line)).toBe(true);
    }
    // Varied length is the point — one short line and one long one at least.
    const lengths = PREVIEW_STATEMENTS.map((l) => l.length);
    expect(Math.max(...lengths) - Math.min(...lengths)).toBeGreaterThan(30);
  });
});

describe("quality advice", () => {
  it("recommends the best tier that actually fits", () => {
    expect(recommendedQuality(200)).toBe("q8");        // nothing fits; fall back
    expect(recommendedQuality(6000)).toBe("fp32");     // plenty of room
  });

  it("falls back to the default when the device is unknown", () => {
    expect(recommendedQuality(null)).toBe(DEFAULT_QUALITY);
    expect(recommendedQuality(0)).toBe(DEFAULT_QUALITY);
  });

  it("says a bigger tier would fit when one would", () => {
    const advice = qualityAdvice("q8", 6000);
    expect(advice).toContain("Reference");
    expect(advice).toContain("would also fit");
  });

  it("warns when the chosen tier does not fit this device", () => {
    const advice = qualityAdvice("fp32", 300);
    expect(advice).toContain("Expect slower replies");
  });

  // Never "higher is better" — it costs memory the language model wants.
  it("names the trade rather than urging an upgrade", () => {
    const advice = qualityAdvice("q8", null);
    expect(advice.toLowerCase()).toContain("memory");
    expect(advice.toLowerCase()).not.toContain("best quality");
  });

  it("says nothing to change when the current tier is already the fit", () => {
    const advice = qualityAdvice("fp32", 6000);
    expect(advice).toContain("best fit");
  });
});

describe("onboarding tier", () => {
  it("is the smallest tier, and smaller than the everyday default", () => {
    expect(ONBOARDING_QUALITY).toBe("q8f16");
    const setup = describeQuality(ONBOARDING_QUALITY);
    const everyday = describeQuality(DEFAULT_QUALITY);
    expect(setup.sizeMb).toBeLessThan(everyday.sizeMb);
    expect(Math.min(...VOICE_QUALITY_TIERS.map((t) => t.sizeMb))).toBe(setup.sizeMb);
  });
});
