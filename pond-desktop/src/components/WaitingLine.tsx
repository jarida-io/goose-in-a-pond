import { Clock } from "lucide-react";
import "./WaitingLine.css";

/** The pond's own "your turn is next" line: calm and in control, never styled as an error. */
export function WaitingLine({ message }: { message: string }) {
  return (
    <span className="waiting-line" role="status" aria-live="polite" data-testid="waiting-line">
      <Clock size={14} className="waiting-line__icon" aria-hidden="true" />
      <span>{message}</span>
    </span>
  );
}
