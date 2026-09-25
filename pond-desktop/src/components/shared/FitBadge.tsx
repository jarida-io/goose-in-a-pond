// FitBadge: warns when a model would spill to CPU on this device; silent for "fits" and "unknown".

import { AlertTriangle } from "lucide-react";
import { modelFitFor } from "../../api/modelFit";
import type { ModelEntry, ModelMemoryStatus } from "../../api/types";

interface FitBadgeProps {
  model: Pick<ModelEntry, "size_mb" | "ram_estimate_mb">;
  status: ModelMemoryStatus | null | undefined;
  /** Compact variant (icon + short label) for dense rows. */
  compact?: boolean;
}

export function FitBadge({ model, status, compact = false }: FitBadgeProps) {
  const verdict = modelFitFor(model, status);
  if (verdict !== "spills") return null;

  const availGb = status ? (status.available_for_llm_mb / 1024).toFixed(1) : "?";

  return (
    <span
      className="fit-badge fit-badge--spills"
      role="status"
      title={`This model is larger than the device GPU budget (${availGb} GB available). It will spill to CPU and run slowly. Recommended: pick a model that fits your GPU budget.`}
    >
      <AlertTriangle size={13} strokeWidth={2} />
      {compact ? "Too large" : "Too large — will spill to CPU and run slowly"}
    </span>
  );
}
