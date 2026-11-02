import { Loader2, Play, Square } from "lucide-react";
import { Field, FormSection } from "./AgentField";
import { formatSeconds } from "./agentWords";
import type { useTestRun } from "./useTestRun";

interface Props {
  /** The recipe's name once saved; `null` until then, since the pond runs what it has stored. */
  name: string | null;
  dirty: boolean;
  prompt: string;
  onPrompt: (value: string) => void;
  /** Held by the builder, so a run keeps streaming while the person looks at the YAML tab. */
  test: ReturnType<typeof useTestRun>;
}

export function TestRunPanel({ name, dirty, prompt, onPrompt, test }: Props) {
  const { state, run, stop, removeConversation } = test;
  const running = state.status === "running";
  const blocked = name === null ? "Save the agent first, so the pond has it to run." : dirty ? "Save your changes first: a test runs the saved agent." : null;

  return (
    <FormSection
      id="ag-try"
      title="Try it"
      lead="Runs the saved recipe once, as a conversation with the pond's usual tools. It does not use this agent's own tool list, schedule or hooks, so it shows what the text produces, not how a real run behaves."
    >
      <Field
        label="Test prompt"
        hint="What to ask when you press Run test. It is saved with the agent as its prompt line. A real run is given a brief by its schedule, event or the assistant instead."
      >
        {(control) => (
          <textarea {...control} className="ag-textarea" rows={3} value={prompt} onChange={(e) => onPrompt(e.target.value)} />
        )}
      </Field>
      <div className="ag-actions">
        {running ? (
          <button type="button" className="ink-btn ink-btn--quiet ag-btn" onClick={stop}>
            <Square size={14} aria-hidden="true" /> Stop
          </button>
        ) : (
          <button type="button" className="ink-btn ink-btn--quiet ag-btn" disabled={blocked !== null} onClick={() => void run()}>
            <Play size={14} aria-hidden="true" /> Run test
          </button>
        )}
        {blocked ? <span className="ag-hint">{blocked}</span> : null}
      </div>

      {state.status !== "idle" ? (
        <div className="ag-run" role="log" aria-live="polite" aria-busy={running} aria-label="Test run output">
          <p className="ag-run__status">
            {running ? (
              <>
                <Loader2 size={14} className="animate-spin" aria-hidden="true" /> Running...
              </>
            ) : null}
            {state.status === "done" ? `Finished in ${formatSeconds(state.seconds ?? 0)}.` : null}
            {state.status === "stopped" ? "Stopped." : null}
            {state.status === "failed" ? "The test run ended with an error." : null}
          </p>
          {state.tools.length > 0 ? <p className="ag-run__tools">Used: {state.tools.join(", ")}</p> : null}
          {state.error ? <p className="ag-errors__item">{state.error}</p> : null}
          {state.text !== "" ? <p className="ag-run__text">{state.text}</p> : running ? <p className="ag-hint">Waiting for the first words.</p> : null}
        </div>
      ) : null}

      {state.sessionId ? (
        <div className="ag-actions">
          <button type="button" className="ink-btn ink-btn--quiet ag-btn" onClick={() => void removeConversation()}>
            Remove this test from chat history
          </button>
          <span className="ag-hint">The run left a conversation behind, and the pond can learn from conversations.</span>
        </div>
      ) : null}
      {state.removed ? <p className="ag-hint">Removed from chat history.</p> : null}
      {state.removeError ? <p className="ag-errors__item">Could not remove it. {state.removeError}</p> : null}
    </FormSection>
  );
}
