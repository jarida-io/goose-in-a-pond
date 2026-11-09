// The choices a task compared, as cards a member can keep ("Save") or have the pond watch ("Watch price").

import { useState } from "react";
import { Bookmark, BookmarkCheck, ExternalLink, Eye } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { ReportOption } from "../api/types";
import { listingOf } from "./optionListing";
import {
  OPEN_OPTION,
  OPTIONS_LABEL,
  PRICE_WATCHED,
  SAVED_OPTION,
  SAVE_OPTION,
  SAVING_OPTION,
  WATCHING_PRICE,
  WATCH_PRICE,
  couldntSave,
  couldntWatch,
} from "./reportWords";

function reasonOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

type Step = "idle" | "saving" | "watching";

function OptionCard({ option, variant }: { option: ReportOption; variant: "classic" | "hub" }) {
  const [savedId, setSavedId] = useState<string | null>(null);
  const [watched, setWatched] = useState(false);
  const [step, setStep] = useState<Step>("idle");
  const [problem, setProblem] = useState<string | null>(null);
  const webLink = option.link && /^https?:\/\//i.test(option.link) ? option.link : null;

  async function save(): Promise<string | null> {
    if (savedId) return savedId;
    setStep("saving");
    setProblem(null);
    try {
      const card = await api.createSavedItem(listingOf(option));
      setSavedId(card.id);
      return card.id;
    } catch (e) {
      setProblem(couldntSave(reasonOf(e)));
      return null;
    } finally {
      setStep("idle");
    }
  }

  async function watchPrice() {
    const id = await save();
    if (!id) return;
    setStep("watching");
    try {
      await api.watchSavedItem(id);
      setWatched(true);
    } catch (e) {
      setProblem(couldntWatch(reasonOf(e)));
    } finally {
      setStep("idle");
    }
  }

  return (
    <article className={`ropt ropt--${variant}`} aria-label={option.title}>
      <div className="ropt__head">
        <h4 className="ropt__title">{option.title}</h4>
        {option.price ? <p className="ropt__price">{option.price}</p> : null}
      </div>
      {option.fields.length > 0 ? (
        <dl className="ropt__facts">
          {option.fields.map(([name, value]) => (
            <div key={name} className="ropt__fact">
              <dt>{name}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
      ) : null}
      {option.why ? <p className="ropt__why">{option.why}</p> : null}
      <div className="ropt__actions">
        <button
          type="button"
          className="ink-btn ink-btn--quiet ropt__btn"
          onClick={() => void save()}
          disabled={savedId !== null || step !== "idle"}
        >
          {savedId ? <BookmarkCheck size={16} aria-hidden="true" /> : <Bookmark size={16} aria-hidden="true" />}
          {savedId ? SAVED_OPTION : step === "saving" ? SAVING_OPTION : SAVE_OPTION}
        </button>
        {webLink ? (
          <button
            type="button"
            className="ink-btn ink-btn--quiet ropt__btn"
            onClick={() => void watchPrice()}
            disabled={watched || step !== "idle"}
          >
            <Eye size={16} aria-hidden="true" />
            {watched ? PRICE_WATCHED : step === "watching" ? WATCHING_PRICE : WATCH_PRICE}
          </button>
        ) : null}
        {webLink ? (
          <a className="ropt__link" href={webLink} target="_blank" rel="noopener noreferrer">
            <ExternalLink size={14} aria-hidden="true" />
            {OPEN_OPTION}
          </a>
        ) : null}
      </div>
      {problem ? (
        <p className="ropt__problem" role="alert">
          {problem}
        </p>
      ) : null}
    </article>
  );
}

export function ReportOptions({ options, variant }: { options: ReportOption[]; variant: "classic" | "hub" }) {
  if (options.length === 0) return null;
  return (
    <section className="ropts" aria-label={OPTIONS_LABEL}>
      <h3 className="md__h md__h2">{OPTIONS_LABEL}</h3>
      <div className="ropts__grid">
        {options.map((option, i) => (
          <OptionCard key={`${i}-${option.title}`} option={option} variant={variant} />
        ))}
      </div>
    </section>
  );
}
