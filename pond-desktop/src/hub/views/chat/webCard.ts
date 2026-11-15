import type { ContextCard } from "../../../state/reducer";

// Cards whose whole content is the web's answer for the person: the model never saw a picture, a
// video or a page link, so the screen is the only place they exist.
const WEB_CARD_HINTS = new Set(["search", "pictures"]);

/** The turn's web results card, when its last tool is one; null otherwise. */
export function webCardOf(card: ContextCard | undefined): ContextCard | null {
  return card?.renderHint && WEB_CARD_HINTS.has(card.renderHint) ? card : null;
}
