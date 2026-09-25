// ────────────────────────────────────────────────────────────
// ImageSupportStatus — one hairline strip while picture support sets itself
// up for the active chat model.
//
// The WarmupBanner idiom: hairline chrome, slate meta text, no ink offset
// (the edge marks content — DESIGN.md rule 3), a lucide icon rather than a
// bare spinner because "Getting picture support ready: 412 MB of 941 MB"
// says what is happening. Shown ABOVE the AttachmentTray, never in place of
// it — attaching still works while this reads.
//
// Visible in two different ways, on purpose. The transitional states
// (absent/downloading/verifying/failed/blocked) are things happening TO the
// household without their asking, so they stay on screen the whole time,
// same as the WarmupBanner. `not_declared` and `not_on_this_device` are
// permanent facts about the model in use — a persistent line for those would
// be chrome nobody asked for on a screen that otherwise says nothing, so it
// renders only the moment the household reaches for the paperclip or pastes
// an image, via `revealed`.
// ────────────────────────────────────────────────────────────

import { Image, ImageOff, Loader2 } from "lucide-react";
import type { VisionStatus } from "../api/types";
import "../styles/vision-status.css";

const PERSISTENT_KINDS = new Set(["absent", "downloading", "verifying", "failed", "blocked"]);
const SPINNING_KINDS = new Set(["downloading", "verifying"]);
const MUTED_KINDS = new Set(["failed", "blocked"]);

/** Shown once picture support is set up and a message comes back for a state
 *  the server does not send prose for (`message` is null for `not_declared`
 *  — it is a static fact about the model, not something happening). Pinned
 *  to design_v2.md section B1's household copy exactly, and exported so a
 *  composer's tooltip on the paperclip itself can say the same thing. */
export const NOT_DECLARED_COPY =
  "This model cannot look at pictures. To send one, choose a model marked Reads pictures on the Models page.";

/** The line under the composer when a send is gated on picture support —
 *  shared by both composers so the wording cannot drift between shells. */
export const COMPOSER_GATE_LINE =
  "Pictures can be sent once picture support is ready. Remove them to send just the text.";

/**
 * The clause a composer appends to a restored 409's message, so "your draft
 * came back" and "why" read as one sentence. Pinned per design_v2.md section
 * H — `not_ready` and `unsupported` are the only two codes the refusal path
 * carries a specific line for; anything else gets the shared fallback.
 */
export function refusalClientClause(code: string | undefined): string {
  if (code === "vision_not_ready") {
    return " Your message and pictures are back in the box; send them when it is ready.";
  }
  if (code === "vision_unsupported") {
    return " Your message and pictures are back in the box. Remove the pictures to send just the text.";
  }
  return " Your message and pictures are back in the box.";
}

/** "HH:MM", 24-hour, from a retry timestamp — what `failed`'s copy appends. */
function formatRetryClock(unixMs: number): string {
  const d = new Date(unixMs);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

interface ImageSupportStatusProps {
  status: VisionStatus | null;
  /** Whether the household has, this moment, reached for the paperclip or
   *  tried to paste an image — the only time a permanent reason earns a
   *  line. Ignored for the transitional states, which show regardless. */
  revealed: boolean;
}

export function ImageSupportStatus({ status, revealed }: ImageSupportStatusProps) {
  const kind = status?.state.kind;
  if (!status || !kind || kind === "ready" || kind === "unknown") return null;
  if (!PERSISTENT_KINDS.has(kind) && !revealed) return null;

  let line = status.message ?? (kind === "not_declared" ? NOT_DECLARED_COPY : "");
  if (kind === "failed" && status.state.kind === "failed") {
    line = `${line} It tries again at ${formatRetryClock(status.state.retry_at_unix_ms)}.`;
  }
  if (!line) return null;

  const Icon = SPINNING_KINDS.has(kind) ? Loader2 : MUTED_KINDS.has(kind) ? ImageOff : Image;

  return (
    <div className="vision-status" role="status" aria-live="polite">
      <Icon
        size={14}
        className={SPINNING_KINDS.has(kind) ? "vision-status__spin" : undefined}
        aria-hidden
      />
      <span>{line}</span>
    </div>
  );
}
