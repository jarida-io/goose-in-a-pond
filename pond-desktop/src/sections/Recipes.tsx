import { useState, useEffect } from "react";
import { Card, CardContent, Button, Input, Switch } from "@heroui/react";
import { Plus, Trash2, BookOpen, Check, Pencil, Play, X } from "lucide-react";
import { api } from "../api/PondApiClient";
import { PageHeader, useConfirm } from "../components/shared";
import type { AgentRecipe, RecipeParameter, RecipeExtensionSpec } from "../api/types";

// ── Extension vocabulary ──────────────────────────────────────────────────
// Names `recipe_extension_to_tool_group` (crates/pond-api/src/routes.rs) knows; others round-trip via YAML.
const EXTENSION_OPTIONS: Array<{ name: string; label: string }> = [
  { name: "weather", label: "Weather" },
  { name: "schedule", label: "Schedule" },
  { name: "memory", label: "Memory" },
  { name: "device", label: "Devices" },
  { name: "matter", label: "Home / Matter" },
  { name: "vision", label: "Vision" },
];

const INPUT_TYPES: RecipeParameter["input_type"][] = ["string", "number", "boolean", "date", "select"];
const REQUIREMENTS: RecipeParameter["requirement"][] = ["required", "optional"];

interface RecipeFormState {
  name: string;
  title: string;
  description: string;
  prompt: string;
  parameters: RecipeParameter[];
  extensionNames: string[];
  activities: string[];
}

const EMPTY_FORM: RecipeFormState = {
  name: "",
  title: "",
  description: "",
  prompt: "",
  parameters: [],
  extensionNames: [],
  activities: [],
};

function yamlString(s: string): string {
  return JSON.stringify(s);
}

/** Minimal YAML writer using JSON-quoted scalars, like the Hub's `RecipeBuilderModal`; flat values only. */
function buildRecipeYaml(f: RecipeFormState): string {
  const lines: string[] = [];
  lines.push(`title: ${yamlString(f.title || f.name)}`);
  lines.push(`description: ${yamlString(f.description)}`);
  lines.push(`prompt: ${yamlString(f.prompt)}`);

  if (f.parameters.length > 0) {
    lines.push(`parameters:`);
    for (const p of f.parameters) {
      lines.push(`  - key: ${yamlString(p.key)}`);
      lines.push(`    input_type: ${yamlString(p.input_type || "string")}`);
      lines.push(`    requirement: ${yamlString(p.requirement || "optional")}`);
      if (p.description) lines.push(`    description: ${yamlString(p.description)}`);
      if (p.default) lines.push(`    default: ${yamlString(p.default)}`);
    }
  }

  if (f.extensionNames.length > 0) {
    lines.push(`extensions:`);
    for (const name of f.extensionNames) {
      lines.push(`  - type: "builtin"`);
      lines.push(`    name: ${yamlString(name)}`);
    }
  }

  if (f.activities.length > 0) {
    lines.push(`activities:`);
    for (const a of f.activities) lines.push(`  - ${yamlString(a)}`);
  }

  return lines.join("\n") + "\n";
}

/** Best-effort `prompt:` read: round-trips the one-line JSON-quoted form; a block scalar yields "". */
function extractPrompt(yaml: string): string {
  const match = yaml.match(/^prompt:\s*(.+)$/m);
  if (!match) return "";
  const raw = match[1].trim();
  if (raw === "|" || raw === ">") return "";
  try {
    return JSON.parse(raw);
  } catch {
    return raw.replace(/^["']|["']$/g, "");
  }
}

function slugify(s: string): string {
  return s.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "");
}

export function Recipes() {
  const confirm = useConfirm();
  const [recipes, setRecipes] = useState<AgentRecipe[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [editingRecipe, setEditingRecipe] = useState<AgentRecipe | null>(null);
  const [form, setForm] = useState<RecipeFormState>(EMPTY_FORM);
  const [running, setRunning] = useState<string | null>(null);
  const [runTarget, setRunTarget] = useState<AgentRecipe | null>(null);
  const [runValues, setRunValues] = useState<Record<string, string>>({});
  const [runError, setRunError] = useState<string | null>(null);

  function load() {
    setLoading(true);
    api
      .listRecipes()
      .then((res) => setRecipes(Array.isArray(res) ? res : []))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  function resetForm() {
    setShowForm(false);
    setEditingRecipe(null);
    setForm(EMPTY_FORM);
  }

  function startCreate() {
    setEditingRecipe(null);
    setForm(EMPTY_FORM);
    setShowForm(true);
  }

  function startEdit(r: AgentRecipe) {
    setEditingRecipe(r);
    setForm({
      name: r.name,
      title: r.title || r.name,
      description: r.description || "",
      prompt: extractPrompt(r.yaml),
      parameters: r.parameters ? r.parameters.map((p) => ({ ...p })) : [],
      extensionNames: r.extensions ? r.extensions.map((e) => e.name) : [],
      activities: r.activities ? [...r.activities] : [],
    });
    setShowForm(true);
  }

  function updateForm<K extends keyof RecipeFormState>(key: K, value: RecipeFormState[K]) {
    setForm((prev) => ({ ...prev, [key]: value }));
  }

  function addParameter() {
    updateForm("parameters", [
      ...form.parameters,
      { key: "", input_type: "string", requirement: "optional" },
    ]);
  }

  function updateParameter(index: number, patch: Partial<RecipeParameter>) {
    updateForm(
      "parameters",
      form.parameters.map((p, i) => (i === index ? { ...p, ...patch } : p)),
    );
  }

  function removeParameter(index: number) {
    updateForm("parameters", form.parameters.filter((_, i) => i !== index));
  }

  function toggleExtension(name: string) {
    updateForm(
      "extensionNames",
      form.extensionNames.includes(name)
        ? form.extensionNames.filter((n) => n !== name)
        : [...form.extensionNames, name],
    );
  }

  const [activityDraft, setActivityDraft] = useState("");
  function addActivity() {
    const v = activityDraft.trim();
    if (!v) return;
    updateForm("activities", [...form.activities, v]);
    setActivityDraft("");
  }
  function removeActivity(index: number) {
    updateForm("activities", form.activities.filter((_, i) => i !== index));
  }

  async function save() {
    if (!form.title.trim() || !form.prompt.trim()) return;
    const yaml = buildRecipeYaml(form);
    try {
      if (editingRecipe) {
        await api.updateRecipe(editingRecipe.id!, {
          description: form.description.trim(),
          yaml,
        });
      } else {
        const name = form.name.trim() || slugify(form.title);
        if (!name) return;
        await api.createRecipe({ name, description: form.description.trim(), yaml });
      }
      resetForm();
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  async function toggleActive(r: AgentRecipe) {
    if (!r.id) return;
    try {
      await api.updateRecipe(r.id, { active: !r.active });
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  async function remove(r: AgentRecipe) {
    if (!r.id) return;
    if (!await confirm(`Delete recipe "${r.title || r.name}"? This cannot be undone.`, { title: "Delete Recipe", confirmLabel: "Delete", destructive: true })) return;
    try {
      await api.removeRecipe(r.id);
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  function requestRun(r: AgentRecipe) {
    const required = (r.parameters || []).filter((p) => p.requirement === "required" && !p.default);
    if (required.length === 0) {
      void doRun(r, {});
      return;
    }
    setRunTarget(r);
    setRunValues(Object.fromEntries((r.parameters || []).map((p) => [p.key, p.default || ""])));
    setRunError(null);
  }

  async function doRun(r: AgentRecipe, parameters: Record<string, string>) {
    setRunning(r.name);
    setRunError(null);
    try {
      // Output is discarded: Chat/the Hub show a recipe's response; this only runs it.
      // eslint-disable-next-line @typescript-eslint/no-unused-vars
      for await (const _event of api.runRecipe(r.name, { parameters })) {
        // no-op: consuming the generator drives the run to completion
      }
      setRunTarget(null);
    } catch (e) {
      setRunError(String(e));
    } finally {
      setRunning(null);
    }
  }

  return (
    <div className="screen screen--recipes">
      <PageHeader
        title="Recipes"
        action={
          <Button
            variant="secondary"
            onPress={() => (showForm ? resetForm() : startCreate())}
          >
            {showForm ? null : <Plus size={14} />} {showForm ? "Cancel" : "Add Recipe"}
          </Button>
        }
      />

      {showForm && (
        <Card className="card">
          <CardContent>
            <div className="recipes-form">
              <h3 className="recipes-form__title">{editingRecipe ? "Edit recipe" : "New recipe"}</h3>

              <label htmlFor="recipe-title" className="ext-form-label">Title</label>
              <Input
                id="recipe-title"
                placeholder="e.g. Morning Brief"
                value={form.title}
                onChange={(e) => updateForm("title", e.target.value)}
              />

              {!editingRecipe && (
                <>
                  <label htmlFor="recipe-name" className="ext-form-label">Slug (optional — derived from title if left blank)</label>
                  <Input
                    id="recipe-name"
                    placeholder={slugify(form.title) || "morning_brief"}
                    value={form.name}
                    onChange={(e) => updateForm("name", e.target.value)}
                  />
                </>
              )}

              <label htmlFor="recipe-description" className="ext-form-label">Description</label>
              <Input
                id="recipe-description"
                placeholder="One sentence, shown in the recipe list"
                value={form.description}
                onChange={(e) => updateForm("description", e.target.value)}
              />

              <label htmlFor="recipe-prompt" className="ext-form-label">Prompt</label>
              <textarea
                id="recipe-prompt"
                className="skills-form__textarea"
                placeholder="What should Pond do when this recipe runs? Use {{param_key}} to reference a parameter."
                value={form.prompt}
                onChange={(e) => updateForm("prompt", e.target.value)}
                rows={4}
              />

              <label className="ext-form-label">Parameters</label>
              <div className="recipe-params-editor">
                {form.parameters.map((p, i) => (
                  <div key={i} className="recipe-param-row">
                    <Input
                      placeholder="key"
                      value={p.key}
                      onChange={(e) => updateParameter(i, { key: e.target.value })}
                      className="recipe-param-row__key"
                    />
                    <select
                      className="recipe-param-row__select"
                      value={p.input_type || "string"}
                      onChange={(e) => updateParameter(i, { input_type: e.target.value as RecipeParameter["input_type"] })}
                    >
                      {INPUT_TYPES.map((t) => <option key={t} value={t}>{t}</option>)}
                    </select>
                    <select
                      className="recipe-param-row__select"
                      value={p.requirement || "optional"}
                      onChange={(e) => updateParameter(i, { requirement: e.target.value as RecipeParameter["requirement"] })}
                    >
                      {REQUIREMENTS.map((r) => <option key={r} value={r}>{r}</option>)}
                    </select>
                    <Input
                      placeholder="default (optional)"
                      value={p.default || ""}
                      onChange={(e) => updateParameter(i, { default: e.target.value })}
                      className="recipe-param-row__default"
                    />
                    <button
                      type="button"
                      className="skill-card__action-btn skill-card__action-btn--danger"
                      onClick={() => removeParameter(i)}
                      aria-label="Remove parameter"
                    >
                      <Trash2 size={14} />
                    </button>
                  </div>
                ))}
                <Button size="sm" variant="ghost" onPress={addParameter}>
                  <Plus size={12} /> Add parameter
                </Button>
              </div>

              <label className="ext-form-label">Extensions (tools this recipe may use)</label>
              <div className="recipe-extensions-picker">
                {EXTENSION_OPTIONS.map((opt) => (
                  <label key={opt.name} className="recipe-extensions-picker__option">
                    <input
                      type="checkbox"
                      checked={form.extensionNames.includes(opt.name)}
                      onChange={() => toggleExtension(opt.name)}
                    />
                    {opt.label}
                  </label>
                ))}
              </div>
              <p className="muted-12">Leave all unchecked to allow the recipe's normal tool selection, unrestricted.</p>

              <label htmlFor="recipe-activity-draft" className="ext-form-label">Activities (suggestion pills)</label>
              <div className="recipe-activities-editor">
                <div className="recipe-activities-editor__pills">
                  {form.activities.map((a, i) => (
                    <span key={i} className="recipe-activity-pill">
                      {a}
                      <button type="button" onClick={() => removeActivity(i)} aria-label={`Remove ${a}`}>
                        <X size={12} />
                      </button>
                    </span>
                  ))}
                </div>
                <div className="recipe-activities-editor__add">
                  <Input
                    id="recipe-activity-draft"
                    placeholder="e.g. Give me my morning brief"
                    value={activityDraft}
                    onChange={(e) => setActivityDraft(e.target.value)}
                    onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addActivity(); } }}
                  />
                  <Button size="sm" variant="ghost" onPress={addActivity}>Add</Button>
                </div>
              </div>

              <div className="skills-form__actions">
                <Button variant="ghost" onPress={resetForm}>Cancel</Button>
                <Button
                  variant="secondary"
                  onPress={save}
                  isDisabled={!form.title.trim() || !form.prompt.trim()}
                >
                  <Check size={14} /> {editingRecipe ? "Update recipe" : "Save recipe"}
                </Button>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {error && <p className="text-error text-error--sm">{error}</p>}

      {loading ? (
        <p className="muted-12">Loading...</p>
      ) : recipes.length === 0 && !showForm ? (
        <Card className="card">
          <CardContent className="card-body--list">
            <div className="empty-state">
              <BookOpen size={20} />
              <span>No recipes yet. Add one to teach Pond a repeatable routine.</span>
            </div>
          </CardContent>
        </Card>
      ) : recipes.length > 0 ? (
        <div className="skills-grid">
          {recipes.map((r) => (
            <div key={r.id || r.name} className={`skill-card${r.active === false ? " skill-card--inactive" : ""}`}>
              <div className="skill-card__top">
                <span className="skill-card__icon">
                  <BookOpen size={16} />
                </span>
                <Switch
                  size="sm"
                  isSelected={r.active !== false}
                  onChange={() => toggleActive(r)}
                  aria-label={`Enable ${r.title || r.name}`}
                >
                  <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
                </Switch>
              </div>
              <h3 className="skill-card__title">{r.title || r.name}</h3>
              <p className="skill-card__desc">
                {r.description || <span className="muted">No description</span>}
              </p>
              {r.activities && r.activities.length > 0 && (
                <div className="recipe-activities-editor__pills">
                  {r.activities.slice(0, 3).map((a, i) => (
                    <span key={i} className="recipe-activity-pill recipe-activity-pill--static">{a}</span>
                  ))}
                </div>
              )}
              <div className="skill-card__actions">
                <button
                  type="button"
                  className="skill-card__action-btn"
                  onClick={() => requestRun(r)}
                  aria-label={`Run ${r.title || r.name}`}
                  disabled={running === r.name}
                >
                  <Play size={14} />
                </button>
                <button
                  type="button"
                  className="skill-card__action-btn"
                  onClick={() => startEdit(r)}
                  aria-label={`Edit ${r.title || r.name}`}
                >
                  <Pencil size={14} />
                </button>
                <button
                  type="button"
                  className="skill-card__action-btn skill-card__action-btn--danger"
                  onClick={() => remove(r)}
                  aria-label={`Delete ${r.title || r.name}`}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            </div>
          ))}
        </div>
      ) : null}

      {runTarget && (
        <div className="recipe-run-modal-backdrop" onClick={() => setRunTarget(null)}>
          <div
            className="recipe-run-modal"
            role="dialog"
            aria-modal="true"
            aria-label={`Run ${runTarget.title || runTarget.name}`}
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="recipes-form__title">Run "{runTarget.title || runTarget.name}"</h3>
            <p className="muted-12">This recipe needs a few values before it can run.</p>
            {(runTarget.parameters || []).map((p) => (
              <div key={p.key}>
                <label htmlFor={`run-param-${p.key}`} className="ext-form-label">
                  {p.key}{p.requirement === "required" && !p.default ? " *" : ""}
                </label>
                <Input
                  id={`run-param-${p.key}`}
                  placeholder={p.description || p.key}
                  value={runValues[p.key] || ""}
                  onChange={(e) => setRunValues((prev) => ({ ...prev, [p.key]: e.target.value }))}
                />
              </div>
            ))}
            {runError && <p className="text-error text-error--sm">{runError}</p>}
            <div className="skills-form__actions">
              <Button variant="ghost" onPress={() => setRunTarget(null)}>Cancel</Button>
              <Button
                variant="secondary"
                isDisabled={running === runTarget.name}
                onPress={() => doRun(runTarget, runValues)}
              >
                <Play size={14} /> Run
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
