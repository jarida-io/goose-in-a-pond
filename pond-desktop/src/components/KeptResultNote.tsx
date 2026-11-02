import { Clock } from "lucide-react";
import { keptLine, type KeptResult } from "../state/keptResults";

export function KeptResultNote({ result }: { result: KeptResult }) {
  return (
    <p className="kept-note">
      <Clock size={11} aria-hidden />
      <span>{keptLine(result)}</span>
    </p>
  );
}
