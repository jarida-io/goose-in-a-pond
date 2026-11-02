import { Plus, Trash2 } from "lucide-react";
import { ErrorIndex, ROLE, paths } from "./agentErrors";
import { MAX_EXAMPLES, type AgentDraft, type ExampleDraft } from "./agentModel";
import { Field, FormSection, Messages } from "./AgentField";
import type { EditDraft } from "./useAgentBuilder";

interface Props {
  draft: AgentDraft;
  edit: EditDraft;
  errors: ErrorIndex;
}

export function ExamplesEditor({ draft, edit, errors }: Props) {
  const examples = draft.examples;

  function update(index: number, patch: Partial<ExampleDraft>) {
    edit(
      (d) => ({ ...d, examples: d.examples.map((e, i) => (i === index ? { ...e, ...patch } : e)) }),
      { section: "examples" },
    );
  }

  function remove(index: number) {
    edit((d) => ({ ...d, examples: d.examples.filter((_, i) => i !== index) }), { section: "examples", structural: true });
  }

  function add() {
    edit((d) => ({ ...d, examples: [...d.examples, { brief: "", answer: "" }] }), { section: "examples" });
  }

  return (
    <FormSection
      id="ag-examples"
      title="Examples"
      problems={errors.inSection("examples").length}
      lead="Pairs of a job and the answer you would want. The agent reads them with its instructions, so they teach it your style. Each one makes the first compile a little longer, and costs nothing after that."
    >
      <Messages messages={[...errors.at(paths.examples), ...errors.at(`${ROLE}.context`)]} />
      {examples.length === 0 ? (
        <p className="ag-hint">No examples. Instructions alone are often enough.</p>
      ) : (
        <ol className="ag-items">
          {examples.map((example, i) => (
            <li key={i} className="ag-item">
              <div className="ag-item__head">
                <h3 className="ag-item__title">Example {i + 1}</h3>
                <button
                  type="button"
                  className="ag-iconbtn"
                  aria-label={`Remove example ${i + 1}`}
                  onClick={() => remove(i)}
                >
                  <Trash2 size={15} aria-hidden="true" />
                </button>
              </div>
              <Messages messages={errors.leftovers(paths.example(i), [paths.exampleField(i, "brief"), paths.exampleField(i, "answer")])} />
              <Field label="A job it might be given" errors={errors.at(paths.exampleField(i, "brief"))}>
                {(control) => (
                  <textarea
                    {...control}
                    className="ag-textarea"
                    rows={2}
                    value={example.brief}
                    onChange={(e) => update(i, { brief: e.target.value })}
                  />
                )}
              </Field>
              <Field label="The answer you would want" errors={errors.at(paths.exampleField(i, "answer"))}>
                {(control) => (
                  <textarea
                    {...control}
                    className="ag-textarea"
                    rows={3}
                    value={example.answer}
                    onChange={(e) => update(i, { answer: e.target.value })}
                  />
                )}
              </Field>
            </li>
          ))}
        </ol>
      )}
      <div className="ag-actions">
        <button type="button" className="ink-btn ink-btn--quiet ag-btn" onClick={add} disabled={examples.length >= MAX_EXAMPLES}>
          <Plus size={14} aria-hidden="true" /> Add an example
        </button>
        <span className="ag-hint">
          {examples.length} of {MAX_EXAMPLES}
        </span>
      </div>
    </FormSection>
  );
}
