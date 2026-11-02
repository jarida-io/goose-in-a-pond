import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api/PondApiClient";
import { ApiError, type AgentRecipe, type RecipeValidation } from "../api/types";
import { ErrorIndex, sectionOf, type SectionId } from "./agentErrors";
import {
  agentNameProblem,
  draftFromYaml,
  newAgentDraft,
  slugifyAgentName,
  yamlFromDraft,
  type AgentDraft,
  type ReadResult,
} from "./agentModel";

/** Long enough that a burst of typing is one request, short enough that the answer feels live. */
export const VALIDATE_DEBOUNCE_MS = 350;

export type BuilderView = "form" | "yaml";

export interface EditOptions {
  /** The section edited; errors in a section a new agent has not touched stay quiet. */
  section?: SectionId;
  /** Adds, removes or reorders items, which moves the paths older errors name. */
  structural?: boolean;
}

/** What every section editor receives to change the draft. */
export type EditDraft = (change: (d: AgentDraft) => AgentDraft, options?: EditOptions) => void;

function startingYaml(recipe: AgentRecipe | null): string {
  return recipe ? recipe.yaml : yamlFromDraft(newAgentDraft());
}

function failureText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** A pond built before the validate route answers 404 or 405; its agents are saved unchecked. */
function isMissingRoute(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 404 || e.status === 405 || e.status === 501);
}

export interface Readiness {
  ok: boolean;
  /** Why Save would do nothing yet, in the words the screen shows. */
  reason: string | null;
}

/** The state of one agent being written: the form, the YAML behind it, and the pond's opinion. */
export function useAgentBuilder(recipe: AgentRecipe | null, recipes: readonly AgentRecipe[]) {
  const [start] = useState(() => {
    const text = startingYaml(recipe);
    return { text, read: draftFromYaml(text, recipe?.description ?? "") };
  });

  const [current, setCurrent] = useState<AgentRecipe | null>(recipe);
  const [yaml, setYaml] = useState(start.text);
  const [baseline, setBaseline] = useState(start.text);
  const [read, setRead] = useState<ReadResult>(start.read);
  const [view, setView] = useState<BuilderView>(start.read.ok ? "form" : "yaml");
  const [typedName, setTypedName] = useState<string | null>(null);

  const [report, setReport] = useState<RecipeValidation | null>(null);
  const [checking, setChecking] = useState(true);
  const [unavailable, setUnavailable] = useState<string | null>(null);
  const [unchecked, setUnchecked] = useState(false);

  const [touched, setTouched] = useState<ReadonlySet<SectionId>>(() => new Set());
  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [justSaved, setJustSaved] = useState(false);

  const draft = read.ok ? read.draft : null;
  const name = current ? current.name : (typedName ?? slugifyAgentName(draft?.title ?? ""));
  const taken = useMemo(() => recipes.filter((r) => r.id !== current?.id).map((r) => r.name), [recipes, current]);
  const nameProblem = current ? null : agentNameProblem(name, taken);

  const sequence = useRef(0);
  useEffect(() => {
    const mine = ++sequence.current;
    setChecking(true);
    const timer = setTimeout(() => {
      api
        .validateRecipe(yaml, name || undefined)
        .then((answer) => {
          if (sequence.current !== mine) return;
          setReport(answer);
          setUnavailable(null);
          setUnchecked(false);
          setChecking(false);
        })
        .catch((e: unknown) => {
          if (sequence.current !== mine) return;
          setChecking(false);
          if (isMissingRoute(e)) {
            setUnchecked(true);
            setUnavailable(null);
          } else {
            setUnavailable(failureText(e));
          }
        });
    }, VALIDATE_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [yaml, name]);

  const changed = useCallback(() => {
    setJustSaved(false);
    setSaveError(null);
  }, []);

  const edit = useCallback(
    (change: (d: AgentDraft) => AgentDraft, options: EditOptions = {}) => {
      if (!read.ok) return;
      const next = change(read.draft);
      setRead({ ok: true, draft: next, hasRole: true });
      setYaml(yamlFromDraft(next));
      const section = options.section;
      if (section) setTouched((prev) => (prev.has(section) ? prev : new Set(prev).add(section)));
      if (options.structural) setReport(null);
      changed();
    },
    [read, changed],
  );

  const changeYaml = useCallback(
    (text: string) => {
      setYaml(text);
      setRead(draftFromYaml(text, ""));
      changed();
    },
    [changed],
  );

  const setName = useCallback((value: string) => setTypedName(value), []);

  const errors = useMemo(() => report?.errors ?? [], [report]);
  const revealAll = current !== null || submitted || view === "yaml";
  const visible = useMemo(
    () =>
      revealAll
        ? errors
        : errors.filter((e) => {
            const section = sectionOf(e.path);
            return section === null || touched.has(section);
          }),
    [errors, revealAll, touched],
  );
  const index = useMemo(() => new ErrorIndex(visible), [visible]);

  const dirty = yaml !== baseline;

  const readiness: Readiness = useMemo(() => {
    if (nameProblem) return { ok: false, reason: nameProblem };
    if (checking) return { ok: false, reason: "Checking the agent with the pond." };
    if (unavailable) return { ok: false, reason: "The pond could not check this agent just now." };
    if (!unchecked && report === null) return { ok: false, reason: "Checking the agent with the pond." };
    if (!unchecked && report && !report.ok) {
      const n = report.errors.length;
      return { ok: false, reason: n === 1 ? "One thing to fix before it can be saved." : `${n} things to fix before it can be saved.` };
    }
    return { ok: true, reason: null };
  }, [nameProblem, checking, unavailable, unchecked, report]);

  const save = useCallback(async (): Promise<AgentRecipe | null> => {
    setSubmitted(true);
    if (!readiness.ok || saving) return null;
    setSaving(true);
    setSaveError(null);
    try {
      const description = (draft?.description ?? current?.description ?? "").trim();
      const saved = current?.id
        ? await api.updateRecipe(current.id, { description, yaml })
        : await api.createRecipe({ name, description, yaml });
      setCurrent(saved);
      setBaseline(yaml);
      setJustSaved(true);
      return saved;
    } catch (e) {
      setSaveError(failureText(e));
      return null;
    } finally {
      setSaving(false);
    }
  }, [readiness, saving, draft, current, yaml, name]);

  return {
    draft,
    read,
    yaml,
    view,
    setView,
    edit,
    changeYaml,
    name,
    setName,
    nameLocked: current !== null,
    nameProblem,
    current,
    report,
    checking,
    unavailable,
    unchecked,
    errors: index,
    problemCount: errors.length,
    readiness,
    dirty,
    saving,
    saveError,
    justSaved,
    save,
    submitted,
  };
}
