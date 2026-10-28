import type { ContextCard } from "../../../state/reducer";
import { findCardByHint } from "../../../mcp-ui";

// Cards whose whole content is the web's answer for the person: the model never saw a picture, a
// video or a page link, so the screen is the only place they exist.
const WEB_CARD_HINTS = new Set(["search", "pictures"]);

/** The turn's web results card, when its last tool is one; null otherwise. */
export function webCardOf(card: ContextCard | undefined): ContextCard | null {
  return card?.renderHint && WEB_CARD_HINTS.has(card.renderHint) ? card : null;
}

export function WebResultCard({ card }: { card: ContextCard }) {
  const registration = card.renderHint ? findCardByHint(card.renderHint) : null;
  if (!registration) return null;
  const Renderer = registration.component;
  return (
    <div className="ch-web-result" role="region" aria-label={`${registration.label} result`}>
      <Renderer data={card.data ?? {}} toolName={card.tool} variant="normal" />
    </div>
  );
}
