import { describe, it, expect } from "vitest";
import {
  modelFit,
  modelFitFor,
  modelResidencyMb,
  DEFAULT_HEADROOM_MB,
} from "./modelFit";
import type { ModelMemoryStatus } from "./types";

describe("modelFit", () => {
  // Budget mirrors the Jetson-class E2E mock: 4096 MB available for the LLM.
  const AVAIL = 4096;
  // With the default 1024 MB headroom, the effective budget is 3072 MB.
  const EFFECTIVE = AVAIL - DEFAULT_HEADROOM_MB;

  it("fits when the model is well under the effective budget", () => {
    // gemma-2-2b (~1600 MB) — the safe fitting alternative.
    expect(modelFit(1600, AVAIL)).toBe("fits");
  });

  it("fits a ~2 GB 3B-Q4 model (the roofline-friendly choice)", () => {
    expect(modelFit(2000, AVAIL)).toBe("fits");
  });

  it("spills when the model exceeds the budget", () => {
    // gemma3n:e2b real download (~5600 MB) — the root-cause slow model.
    expect(modelFit(5600, AVAIL)).toBe("spills");
  });

  it("fits exactly at the effective budget boundary", () => {
    expect(modelFit(EFFECTIVE, AVAIL)).toBe("fits");
  });

  it("spills just one MB over the effective budget boundary", () => {
    expect(modelFit(EFFECTIVE + 1, AVAIL)).toBe("spills");
  });

  it("returns unknown when the budget is zero (memory-status unavailable)", () => {
    // NoopScheduler (llamafile/ollama) reports zeros on Mac/dev.
    expect(modelFit(2000, 0)).toBe("unknown");
  });

  it("returns unknown when the budget is null or negative", () => {
    expect(modelFit(2000, null)).toBe("unknown");
    expect(modelFit(2000, -1)).toBe("unknown");
  });

  it("returns unknown when the model size is unknown", () => {
    expect(modelFit(null, AVAIL)).toBe("unknown");
    expect(modelFit(0, AVAIL)).toBe("unknown");
  });

  it("respects a custom headroom margin", () => {
    // A 3500 MB model fits with no headroom but spills with 1 GB reserved.
    expect(modelFit(3500, AVAIL, 0)).toBe("fits");
    expect(modelFit(3500, AVAIL, 1024)).toBe("spills");
  });

  it("clamps a negative headroom to zero", () => {
    // Negative headroom must not inflate the budget above availableForLlmMb.
    expect(modelFit(AVAIL, AVAIL, -500)).toBe("fits");
    expect(modelFit(AVAIL + 1, AVAIL, -500)).toBe("spills");
  });
});

describe("modelResidencyMb", () => {
  it("prefers size_mb (on-disk residency) over ram_estimate_mb", () => {
    expect(modelResidencyMb({ size_mb: 5600, ram_estimate_mb: 5600 })).toBe(5600);
    expect(modelResidencyMb({ size_mb: 3100, ram_estimate_mb: 4000 })).toBe(3100);
  });

  it("falls back to ram_estimate_mb when size_mb is missing", () => {
    expect(modelResidencyMb({ ram_estimate_mb: 3200 })).toBe(3200);
    expect(modelResidencyMb({ size_mb: 0, ram_estimate_mb: 3200 })).toBe(3200);
  });

  it("returns null when neither is known", () => {
    expect(modelResidencyMb({})).toBeNull();
    expect(modelResidencyMb({ size_mb: 0, ram_estimate_mb: 0 })).toBeNull();
  });

  it("adds the encoder's resident bytes when the model reads pictures", () => {
    // A declared-vision model keeps the encoder resident too (eager load, on
    // the GPU, at every model load) — the fit meter must count it or a model
    // that "fits" can still spill once its encoder lands.
    expect(
      modelResidencyMb({ size_mb: 2600, reads_images: true, image_support_bytes: 986_833_728 }),
    ).toBeCloseTo(2600 + 986_833_728 / 1_048_576, 6);
  });

  it("never adds encoder bytes when reads_images is not exactly true", () => {
    expect(
      modelResidencyMb({ size_mb: 2600, reads_images: false, image_support_bytes: 986_833_728 }),
    ).toBe(2600);
    expect(modelResidencyMb({ size_mb: 2600, image_support_bytes: 986_833_728 })).toBe(2600);
  });
});

describe("modelFitFor", () => {
  const status: ModelMemoryStatus = {
    total_mb: 8192,
    available_for_llm_mb: 4096,
    loaded_model: null,
  };

  it("spills the corrected 5.6 GB gemma3n:e2b on an 8 GB device", () => {
    expect(modelFitFor({ size_mb: 5600, ram_estimate_mb: 5600 }, status)).toBe("spills");
  });

  it("fits the 1.6 GB llamafile alternative on an 8 GB device", () => {
    expect(modelFitFor({ size_mb: 1600, ram_estimate_mb: 1800 }, status)).toBe("fits");
  });

  it("returns unknown when memory-status is null (Mac/dev)", () => {
    expect(modelFitFor({ size_mb: 5600 }, null)).toBe("unknown");
  });

  it("returns unknown when the scheduler reports no budget (total_mb 0)", () => {
    const noop: ModelMemoryStatus = { total_mb: 0, available_for_llm_mb: 0, loaded_model: null };
    expect(modelFitFor({ size_mb: 5600 }, noop)).toBe("unknown");
  });
});
