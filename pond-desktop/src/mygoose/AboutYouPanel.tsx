// What Goose keeps about the member: the fact sheet by category, each line with its confidence,
// source and tool hints, to pin, reword or remove; and below it the don't-list.

import { useId, useState, type FormEvent } from "react";
import { Pencil, Pin, PinOff, Plus, RefreshCw, Trash2, UserRound } from "lucide-react";
import { InkBadge, InkButton, InkEmptyState, InkField, InkFormField, InkInput, InkSegmented, InkSelect } from "@jarida/ink/react";
import { api } from "../api/PondApiClient";
import type { DontRule, FactCategory, FactLine, FactSheet } from "../api/types";
import { useConfirm } from "../components/shared";
import {
  ABOUT_HEADING,
  ABOUT_INTRO,
  ADD_LINE,
  ADD_LINE_HEADING,
  ADD_RULE,
  CANCEL,
  CATEGORY_LABEL,
  DELETE,
  DONT_HEADING,
  DONT_INTRO,
  EDIT,
  EDIT_LINE_LABEL,
  FACT_CATEGORIES,
  LINE_ADDED,
  LINE_LABEL,
  LINE_MISSING,
  LINE_PLACEHOLDER,
  LOADING_FACTS,
  NO_FACTS_BODY,
  NO_FACTS_TITLE,
  NO_RULES,
  PIN,
  REBUILD,
  REBUILD_ASKED,
  REMOVE,
  REMOVED,
  REMOVE_QUESTION,
  REMOVE_TITLE,
  RULE_ADDED,
  RULE_DELETED,
  RULE_LABEL,
  RULE_MISSING,
  RULE_PLACEHOLDER,
  SAVE,
  SAVED,
  STRENGTHS,
  STRENGTH_LABEL,
  UNPIN,
  buttonSize,
  countWords,
  factGroups,
  factNote,
  ruleNote,
  strengthWords,
  type Variant,
} from "./myGooseWords";
import { problemWords } from "./problems";
import { useLoaded } from "./useLoaded";
import { ActionProblem, LoadProblem, Loading } from "./MyGooseParts";

interface Props {
  variant: Variant;
  say: (text: string) => void;
}

export function AboutYouPanel({ variant, say }: Props) {
  return (
    <section className="mg-panel" aria-label={ABOUT_HEADING}>
      <FactSheetSection variant={variant} say={say} />
      <DontListSection variant={variant} say={say} />
    </section>
  );
}

function FactSheetSection({ variant, say }: Props) {
  const sheet = useLoaded(() => api.listFacts());
  const headingId = useId();
  const [rebuilding, setRebuilding] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const size = buttonSize(variant);

  function update(change: (current: FactSheet) => FactSheet) {
    if (sheet.state.status === "ready") sheet.replace(change(sheet.state.value));
  }

  async function rebuild() {
    setRebuilding(true);
    setProblem(null);
    try {
      await api.rebuildFacts();
      say(REBUILD_ASKED);
    } catch (e) {
      setProblem(problemWords(e));
    } finally {
      setRebuilding(false);
    }
  }

  const facts = sheet.state.status === "ready" ? sheet.state.value.facts : [];
  const limit = sheet.state.status === "ready" ? sheet.state.value.limit : 100;

  return (
    <section className="mg-sec" aria-labelledby={headingId}>
      <div className="mg__row">
        <h2 id={headingId} className="mg__h2">
          {ABOUT_HEADING}
          {sheet.state.status === "ready" ? <span className="mg__count">{countWords(facts.length, limit)}</span> : null}
        </h2>
        <InkButton size={size} icon={<RefreshCw size={16} aria-hidden="true" />} loading={rebuilding} onPress={() => void rebuild()}>
          {REBUILD}
        </InkButton>
      </div>
      <p className="mg__intro">{ABOUT_INTRO}</p>
      <ActionProblem text={problem} />
      {sheet.state.status === "loading" ? <Loading>{LOADING_FACTS}</Loading> : null}
      {sheet.state.status === "failed" ? <LoadProblem problem={sheet.state.problem} onRetry={sheet.retry} variant={variant} /> : null}
      {sheet.state.status === "ready" && facts.length === 0 ? (
        <InkEmptyState icon={<UserRound size={32} aria-hidden="true" />} title={NO_FACTS_TITLE}>
          {NO_FACTS_BODY}
        </InkEmptyState>
      ) : null}
      {factGroups(facts).map((group) => (
        <div key={group.category} className="mg-facts-group">
          <h3 className="mg__h3">{group.label}</h3>
          <ul className="mg-lines" aria-label={group.label}>
            {group.lines.map((line) => (
              <li key={line.id}>
                <FactLineRow
                  line={line}
                  variant={variant}
                  say={say}
                  onChanged={(next) => update((s) => ({ ...s, facts: s.facts.map((f) => (f.id === next.id ? next : f)) }))}
                  onRemoved={() => update((s) => ({ ...s, facts: s.facts.filter((f) => f.id !== line.id) }))}
                />
              </li>
            ))}
          </ul>
        </div>
      ))}
      {sheet.state.status === "ready" ? <AddLineForm variant={variant} say={say} onAdded={sheet.replace} /> : null}
    </section>
  );
}

interface LineProps {
  line: FactLine;
  variant: Variant;
  say: (text: string) => void;
  onChanged: (line: FactLine) => void;
  onRemoved: () => void;
}

function FactLineRow({ line, variant, say, onChanged, onRemoved }: LineProps) {
  const confirm = useConfirm();
  const ids = useId();
  const [editing, setEditing] = useState(false);
  const [words, setWords] = useState(line.fact);
  const [busy, setBusy] = useState<"pin" | "save" | "remove" | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const size = buttonSize(variant);

  async function run(which: "pin" | "save" | "remove", work: () => Promise<void>) {
    setBusy(which);
    setProblem(null);
    try {
      await work();
    } catch (e) {
      setProblem(problemWords(e));
    } finally {
      setBusy(null);
    }
  }

  async function save(e: FormEvent) {
    e.preventDefault();
    if (words.trim() === "" || words.trim() === line.fact) {
      setEditing(false);
      setWords(line.fact);
      return;
    }
    await run("save", async () => {
      onChanged(await api.changeFact(line.id, { fact: words.trim() }));
      setEditing(false);
      say(SAVED);
    });
  }

  async function remove() {
    const ok = await confirm(REMOVE_QUESTION(line.fact), { title: REMOVE_TITLE, confirmLabel: REMOVE, destructive: true });
    if (!ok) return;
    await run("remove", async () => {
      await api.removeFact(line.id);
      say(REMOVED);
      onRemoved();
    });
  }

  if (editing) {
    return (
      <form className="mg-line mg-line--editing" aria-label={EDIT_LINE_LABEL} onSubmit={(e) => void save(e)}>
        <InkFormField id={`${ids}-fact`} label={EDIT_LINE_LABEL}>
          <InkInput label={EDIT_LINE_LABEL} value={words} inputProps={{ id: `${ids}-fact-control` }} onChange={setWords} />
        </InkFormField>
        <div className="mg__actions">
          <InkButton type="submit" variant="primary" size={size} loading={busy === "save"}>
            {SAVE}
          </InkButton>
          <InkButton
            size={size}
            disabled={busy !== null}
            onPress={() => {
              setEditing(false);
              setWords(line.fact);
            }}
          >
            {CANCEL}
          </InkButton>
        </div>
        <ActionProblem text={problem} />
      </form>
    );
  }

  return (
    <div className="mg-line" data-pinned={line.pinned || undefined}>
      <div className="mg-line__text">
        <span className="mg-line__fact">
          {line.pinned ? <Pin size={14} aria-hidden="true" className="mg-line__pin" /> : null}
          {line.fact}
        </span>
        <span className="mg-line__note">{factNote(line)}</span>
      </div>
      <div className="mg-line__actions">
        <InkButton
          size={size}
          icon={line.pinned ? <PinOff size={16} aria-hidden="true" /> : <Pin size={16} aria-hidden="true" />}
          loading={busy === "pin"}
          disabled={busy !== null}
          onPress={() => void run("pin", async () => onChanged(await api.changeFact(line.id, { pinned: !line.pinned })))}
        >
          {line.pinned ? UNPIN : PIN}
        </InkButton>
        <InkButton size={size} icon={<Pencil size={16} aria-hidden="true" />} disabled={busy !== null} onPress={() => setEditing(true)}>
          {EDIT}
        </InkButton>
        <InkButton size={size} icon={<Trash2 size={16} aria-hidden="true" />} loading={busy === "remove"} disabled={busy !== null} onPress={() => void remove()}>
          {REMOVE}
        </InkButton>
      </div>
      <ActionProblem text={problem} />
    </div>
  );
}

function AddLineForm({ variant, say, onAdded }: { variant: Variant; say: (text: string) => void; onAdded: (sheet: FactSheet) => void }) {
  const ids = useId();
  const [category, setCategory] = useState<FactCategory>("other");
  const [fact, setFact] = useState("");
  const [missing, setMissing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  async function add(e: FormEvent) {
    e.preventDefault();
    if (fact.trim() === "") {
      setMissing(true);
      return;
    }
    setBusy(true);
    setProblem(null);
    try {
      onAdded(await api.addFact(category, fact.trim()));
      setFact("");
      say(LINE_ADDED);
    } catch (err) {
      setProblem(problemWords(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="mg-form mg-form--inline" aria-label={ADD_LINE_HEADING} onSubmit={(e) => void add(e)}>
      <h3 className="mg__h3">{ADD_LINE_HEADING}</h3>
      <div className="mg-form__grid">
        <InkFormField id={`${ids}-line`} label={LINE_LABEL}>
          <InkInput
            label={LINE_LABEL}
            value={fact}
            placeholder={LINE_PLACEHOLDER}
            invalid={missing ? LINE_MISSING : undefined}
            inputProps={{ id: `${ids}-line-control` }}
            onChange={(v) => {
              setFact(v);
              setMissing(false);
            }}
          />
        </InkFormField>
        <InkField label={CATEGORY_LABEL}>
          <InkSelect label={CATEGORY_LABEL} value={category} options={FACT_CATEGORIES} onChange={setCategory} />
        </InkField>
      </div>
      <div className="mg__actions">
        <InkButton type="submit" size={buttonSize(variant)} icon={<Plus size={16} aria-hidden="true" />} loading={busy}>
          {ADD_LINE}
        </InkButton>
      </div>
      <ActionProblem text={problem} />
    </form>
  );
}

function DontListSection({ variant, say }: Props) {
  const rules = useLoaded(() => api.listDontRules());
  const headingId = useId();
  const ids = useId();
  const [rule, setRule] = useState("");
  const [strength, setStrength] = useState<DontRule["strength"]>("never");
  const [missing, setMissing] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const size = buttonSize(variant);

  async function add(e: FormEvent) {
    e.preventDefault();
    if (rule.trim() === "") {
      setMissing(true);
      return;
    }
    setBusy("add");
    setProblem(null);
    try {
      rules.replace(await api.addDontRule(rule.trim(), strength));
      setRule("");
      say(RULE_ADDED);
    } catch (err) {
      setProblem(problemWords(err));
    } finally {
      setBusy(null);
    }
  }

  async function remove(id: string) {
    setBusy(id);
    setProblem(null);
    try {
      rules.replace(await api.removeDontRule(id));
      say(RULE_DELETED);
    } catch (err) {
      setProblem(problemWords(err));
    } finally {
      setBusy(null);
    }
  }

  const list = rules.state.status === "ready" ? rules.state.value : [];

  return (
    <section className="mg-sec" aria-labelledby={headingId}>
      <h2 id={headingId} className="mg__h2">
        {DONT_HEADING}
      </h2>
      <p className="mg__intro">{DONT_INTRO}</p>
      {rules.state.status === "loading" ? <Loading>{LOADING_FACTS}</Loading> : null}
      {rules.state.status === "failed" ? <LoadProblem problem={rules.state.problem} onRetry={rules.retry} variant={variant} /> : null}
      {rules.state.status === "ready" && list.length === 0 ? <p className="mg__note">{NO_RULES}</p> : null}
      {list.length > 0 ? (
        <ul className="mg-lines" aria-label={DONT_HEADING}>
          {list.map((r) => (
            <li key={r.id} className="mg-line">
              <div className="mg-line__text">
                <span className="mg-line__fact">
                  <InkBadge>{strengthWords(r.strength)}</InkBadge>
                  {r.rule}
                </span>
                <span className="mg-line__note">{ruleNote(r)}</span>
              </div>
              <div className="mg-line__actions">
                <InkButton
                  size={size}
                  icon={<Trash2 size={16} aria-hidden="true" />}
                  loading={busy === r.id}
                  disabled={busy !== null}
                  onPress={() => void remove(r.id)}
                >
                  {DELETE}
                </InkButton>
              </div>
            </li>
          ))}
        </ul>
      ) : null}
      <form className="mg-form mg-form--inline" aria-label={ADD_RULE} onSubmit={(e) => void add(e)}>
        <div className="mg-form__grid">
          <InkFormField id={`${ids}-rule`} label={RULE_LABEL}>
            <InkInput
              label={RULE_LABEL}
              value={rule}
              placeholder={RULE_PLACEHOLDER}
              invalid={missing ? RULE_MISSING : undefined}
              inputProps={{ id: `${ids}-rule-control` }}
              onChange={(v) => {
                setRule(v);
                setMissing(false);
              }}
            />
          </InkFormField>
          <InkField label={STRENGTH_LABEL}>
            <InkSegmented label={STRENGTH_LABEL} value={strength} options={STRENGTHS} onChange={setStrength} />
          </InkField>
        </div>
        <div className="mg__actions">
          <InkButton type="submit" size={size} icon={<Plus size={16} aria-hidden="true" />} loading={busy === "add"} disabled={busy !== null && busy !== "add"}>
            {ADD_RULE}
          </InkButton>
        </div>
        <ActionProblem text={problem} />
      </form>
    </section>
  );
}
