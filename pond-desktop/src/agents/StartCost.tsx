import type { ReactNode } from "react";
import type { RecipeValidation } from "../api/types";
import { ORIN_PREFILL_TOKENS_PER_SECOND, formatCount, formatSeconds, startCost } from "./agentWords";

interface Props {
  report: RecipeValidation | null;
  checking: boolean;
  /** The pond has no validate route, so it cannot measure anything. */
  unchecked: boolean;
}

/** The size of the agent's own text, and what compiling it costs once. Both numbers are estimates. */
export function StartCost({ report, checking, unchecked }: Props) {
  const role = report?.role ?? null;

  let body: ReactNode;
  if (unchecked) {
    body = <p className="ag-hint">This pond cannot measure an agent yet, so there is no size to show.</p>;
  } else if (role) {
    const cost = startCost(role.preamble_chars);
    body = (
      <>
        <p className="ag-cost__figures">
          About <span className="ag-mono">{formatCount(cost.tokens)} tokens</span> to read, and about{" "}
          <span className="ag-mono">{formatSeconds(cost.seconds)}</span> to compile the first time.
        </p>
        <p className="ag-hint">
          Both are estimates. The size is the agent's instructions and examples, {formatCount(cost.chars)} characters at about 4 to a
          token; its tools' descriptions are not counted. The time assumes about {ORIN_PREFILL_TOKENS_PER_SECOND} tokens a second, as
          measured on an Orin Nano.
        </p>
      </>
    );
  } else if (checking) {
    body = <p className="ag-hint">Measuring...</p>;
  } else {
    body = <p className="ag-hint">Fix the problems on this page to see how big the agent is.</p>;
  }

  return (
    <div className="ag-cost" aria-live="polite" data-stale={checking && role ? "true" : undefined}>
      <h3 className="ag-sub-title">What it costs to start</h3>
      {body}
      <p className="ag-hint">
        You pay this once, when the pond compiles the agent. After that every run starts from the saved copy, so it costs nothing again
        until the agent's text changes.
      </p>
    </div>
  );
}
