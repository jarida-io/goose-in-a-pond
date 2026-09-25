import { useEffect, useRef, useState } from "react";
import { HubModal } from "../primitives/HubModal";
import { HubIco } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import { api } from "../../api/PondApiClient";
import { refreshHomeData } from "../state/hubDataStore";

interface RecipeBuilderModalProps {
  onClose: () => void;
  onCreated?: () => void;
}

// Goose recipe names are unique slugs: lowercase snake_case, [a-z0-9_] only.
function slugify(input: string): string {
  return input
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
}

// JSON strings are valid YAML scalars, so JSON.stringify does the escaping.
function buildYaml(title: string, description: string, prompt: string): string {
  return [
    `title: ${JSON.stringify(title)}`,
    `description: ${JSON.stringify(description)}`,
    `prompt: ${JSON.stringify(prompt)}`,
  ].join("\n") + "\n";
}

export function RecipeBuilderModal({ onClose, onCreated }: RecipeBuilderModalProps) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [prompt, setPrompt] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const titleRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    titleRef.current?.focus();
  }, []);

  const slug = slugify(title);
  const canSubmit = slug.length > 0 && prompt.trim().length > 0 && !submitting;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    try {
      await api.createRecipe({
        name: slug,
        description: description.trim(),
        yaml: buildYaml(title.trim(), description.trim(), prompt.trim()),
      });
      await refreshHomeData();
      onCreated?.();
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not create routine");
      setSubmitting(false);
    }
  };

  return (
    <HubModal label="Create a routine" onClose={onClose}>
      <form className="rb-form" onSubmit={handleSubmit}>
        <header className="rb-form__head">
          <span className="rb-form__icon">
            <HubIco d={HP_PATHS.plus} size={22} color="#fff" sw={2.4} />
          </span>
          <div>
            <h2 className="rb-form__title">Create a routine</h2>
            <p className="rb-form__sub">
              Give Goose a one-tap action — name it, then describe what it
              should do.
            </p>
          </div>
        </header>

        <label className="rb-field">
          <span className="rb-field__label">Name</span>
          <input
            ref={titleRef}
            className="rb-field__input"
            type="text"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder="Morning Wake Up"
            maxLength={64}
            required
          />
          {slug && (
            <span className="rb-field__hint">
              Saved as <code>{slug}</code>
            </span>
          )}
        </label>

        <label className="rb-field">
          <span className="rb-field__label">What it does</span>
          <input
            className="rb-field__input"
            type="text"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="Lights, music, news brief"
            maxLength={120}
          />
          <span className="rb-field__hint">
            Shown as chips on the card. Separate with commas.
          </span>
        </label>

        <label className="rb-field">
          <span className="rb-field__label">Instruction</span>
          <textarea
            className="rb-field__input rb-field__input--area"
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            placeholder="Turn on the bedroom lights, play morning playlist, and read the news headlines."
            rows={4}
            required
          />
          <span className="rb-field__hint">
            Goose runs this when you tap the routine.
          </span>
        </label>

        {error && <div className="rb-form__error">{error}</div>}

        <div className="rb-form__actions">
          <button
            type="button"
            className="rb-form__btn rb-form__btn--ghost"
            onClick={onClose}
            disabled={submitting}
          >
            Cancel
          </button>
          <button
            type="submit"
            className="rb-form__btn rb-form__btn--primary"
            disabled={!canSubmit}
          >
            {submitting ? "Creating…" : "Create routine"}
          </button>
        </div>
      </form>
    </HubModal>
  );
}
