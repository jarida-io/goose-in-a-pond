// What the chat shells say about picture support, shared so both shells and the strip agree.

import { RESEARCH_REFUSED_CLAUSE } from "./deepResearchCopy";

/** Copy for `not_declared`, which has no server prose; the pond's own refusal reads the same. */
export const NOT_DECLARED_COPY =
  "This model cannot look at pictures. To send one, choose a model marked Pictures included on the Models page.";

/** The paperclip's reason while the active model cannot take pictures and the pond says nothing more. */
export const ATTACH_BLOCKED_COPY =
  "The active model cannot read images. Switch to a model marked Pictures included on the Models page.";

/** Under-composer line when a send waits on picture support; shared so both shells agree. */
export const COMPOSER_GATE_LINE =
  "Pictures can be sent once picture support is ready. Remove them to send just the text.";

/** Clause for a restored 409's message (design_v2.md §H); unknown codes get the fallback. */
export function refusalClientClause(code: string | undefined): string {
  if (code === "no_model") {
    return " Your message is back in the box; send it once a model is chosen.";
  }
  if (code === "vision_not_ready") {
    return " Your message and pictures are back in the box; send them when it is ready.";
  }
  if (code === "deep_research_off" || code === "deep_research_unidentified") {
    return RESEARCH_REFUSED_CLAUSE;
  }
  if (code === "vision_unsupported") {
    return " Your message and pictures are back in the box. Remove the pictures to send just the text.";
  }
  return " Your message and pictures are back in the box.";
}
