// ────────────────────────────────────────────────────────────
// Memory-fit guard — pure decision logic (Phase 6)
//
// Given a model's on-disk residency size and the device's LLM memory budget
// (from GET /api/v1/models/memory-status), decide whether the model will fit
// fully in the GPU/RAM budget or spill to CPU and run slowly.
//
// WHY this matters: on-device decode is memory-bandwidth-bound. When a model
// fully resides in the GPU budget, throughput ≈ bandwidth / model_size. When it
// exceeds the budget, part of the weights spill to CPU and per-token latency
// collapses to single-digit tok/s (the user's "hella slow" complaint on the
// 8 GB Jetson with the ~5.6 GB gemma3n:e2b).
//
// This module is framework-free and pure so it can be unit-tested on Mac/dev
// where the live memory-status endpoint may be absent.
// ────────────────────────────────────────────────────────────

import type { ModelEntry, ModelMemoryStatus } from "./types";

/** Fit verdict for a model against a device's LLM memory budget. */
export type FitVerdict = "fits" | "spills" | "unknown";

/**
 * Default headroom (MB) reserved on top of the model's own weights.
 *
 * The KV cache, activation buffers, and system slack all consume RAM beyond the
 * static weight residency. ~1 GB is a conservative reserve that keeps a fitting
 * model comfortably below the budget without false "spills" verdicts.
 */
export const DEFAULT_HEADROOM_MB = 1024;

/**
 * Decide whether a model of `modelSizeMb` fits within `availableForLlmMb`,
 * reserving `headroomMb` for KV cache + system slack.
 *
 * Returns `"unknown"` when the budget is unavailable (`<= 0`) — e.g. on Mac/dev
 * where the scheduler is a NoopScheduler and memory-status reports zeros, or
 * when the model size is unknown. Callers must treat `"unknown"` as "no verdict"
 * and render nothing (graceful degradation).
 */
export function modelFit(
  modelSizeMb: number | null | undefined,
  availableForLlmMb: number | null | undefined,
  headroomMb: number = DEFAULT_HEADROOM_MB,
): FitVerdict {
  // Budget unavailable → we cannot make a claim.
  if (availableForLlmMb == null || availableForLlmMb <= 0) return "unknown";
  // Model size unknown → we cannot make a claim.
  if (modelSizeMb == null || modelSizeMb <= 0) return "unknown";

  const effectiveBudget = availableForLlmMb - Math.max(0, headroomMb);
  return modelSizeMb <= effectiveBudget ? "fits" : "spills";
}

/**
 * Best residency-size estimate (MB) for a model.
 *
 * Prefers `size_mb` (on-disk weight residency — what actually has to fit in the
 * GPU budget) and falls back to `ram_estimate_mb`. Returns `null` when neither
 * is known.
 *
 * A declared-vision model keeps its encoder resident too (it loads eagerly,
 * on the GPU, at every model load — see `models/domain/vision_encoder.rs`),
 * so `image_support_bytes` is added on top whenever `reads_images` is true.
 */
export function modelResidencyMb(
  m: Pick<ModelEntry, "size_mb" | "ram_estimate_mb" | "reads_images" | "image_support_bytes">,
): number | null {
  const base =
    m.size_mb != null && m.size_mb > 0
      ? m.size_mb
      : m.ram_estimate_mb != null && m.ram_estimate_mb > 0
        ? m.ram_estimate_mb
        : null;
  if (base == null) return null;
  const encoderMb =
    m.reads_images === true && m.image_support_bytes ? m.image_support_bytes / 1_048_576 : 0;
  return base + encoderMb;
}

/**
 * Convenience: verdict for a `ModelEntry` against a `ModelMemoryStatus`.
 *
 * Degrades to `"unknown"` when `status` is null/missing or reports no budget
 * (`total_mb <= 0` — NoopScheduler / memory-status unavailable on Mac/dev).
 */
export function modelFitFor(
  m: Pick<ModelEntry, "size_mb" | "ram_estimate_mb" | "reads_images" | "image_support_bytes">,
  status: ModelMemoryStatus | null | undefined,
  headroomMb: number = DEFAULT_HEADROOM_MB,
): FitVerdict {
  if (!status || status.total_mb <= 0) return "unknown";
  return modelFit(modelResidencyMb(m), status.available_for_llm_mb, headroomMb);
}
