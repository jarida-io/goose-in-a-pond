import type { ContextCard } from "../../../state/reducer";
import { findCardByHint } from "../../../mcp-ui";

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
