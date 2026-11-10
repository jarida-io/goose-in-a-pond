import { useId } from "react";
import {
  AlertTriangle, Check, Cpu, FileBox, Image, ImageOff, ImagePlus, Loader2, Microchip, Pause,
  Play, RotateCw, Server, X, Zap, type LucideIcon,
} from "lucide-react";
import type { ModelEngine, ModelUse } from "../../api/types";
import { PICTURES_LEFT_OUT, includedLabel, jobLabel } from "../../sections/models/modelsView";
import type { AddOn, FitReading, SourceChip as Chip } from "../../sections/models/modelsView";
import {
  pausedText, reconnectingText, type ModelTransfer, type TransferAction,
} from "../../sections/models/modelDownloads";
import "../../styles/model-marks.css";

// The small, repeated words of a Models screen. Both surfaces compose them, so an engine, a
// source, a picture add-on, a fit and a download read the same wherever a model is chosen.

const ENGINE_ICON: Record<string, LucideIcon> = {
  llama_cpp: Cpu,
  litert_lm: Microchip,
  ollama: Server,
  llamafile: FileBox,
  other: Server,
};

/** The engine in words, with the file it loads in mono. The icon only aids the label. */
export function EngineMark({
  engine,
  label,
  format,
}: {
  engine: Pick<ModelEngine, "id"> | { id: "other" };
  label: string;
  format?: string | null;
}) {
  const Icon = ENGINE_ICON[engine.id] ?? Server;
  return (
    <span className="mm-engine">
      <Icon size={14} className="mm-engine__icon" aria-hidden="true" />
      <span className="mm-engine__label">{label}</span>
      {format ? <span className="mm-engine__format">{format}</span> : null}
    </span>
  );
}

/** Where a row came from. Only where that informs: the catalogue earns none. */
export function SourceChip({ chip }: { chip: Chip }) {
  return (
    <span className={chip === "Recommended" ? "mm-chip mm-chip--pick" : "mm-chip"}>{chip}</span>
  );
}

/** A chip per job a model is doing, in the pond's order; nothing for a model doing none. */
export function UseChips({ uses }: { uses: ModelUse[] }) {
  if (uses.length === 0) return null;
  return (
    <span className="mm-uses">
      {uses.map((use) => (
        <span key={use} className="mm-chip mm-chip--use">{jobLabel(use)}</span>
      ))}
    </span>
  );
}

/** What the models serving a row add to it, one mark each ("Faster answers"), or nothing. */
export function ServedMarks({ uses }: { uses: ModelUse[] }) {
  if (uses.length === 0) return null;
  return (
    <>
      {uses.map((use) => {
        const Icon = use === "pictures" ? Image : use === "drafter" ? Zap : Check;
        return (
          <span key={use} className="mm-addon mm-addon--on">
            <Icon size={14} aria-hidden="true" />
            {includedLabel(use)}
          </span>
        );
      })}
    </>
  );
}

/** What a row can do with pictures. An add-on still to fetch is a button: nothing is fetched until pressed. */
export function AddOnLine({
  addOn,
  busy = false,
  onAdd,
}: {
  addOn: AddOn;
  busy?: boolean;
  onAdd?: () => void;
}) {
  switch (addOn.kind) {
    case "none":
      return null;
    case "included":
      return (
        <span className="mm-addon mm-addon--on">
          <Image size={14} aria-hidden="true" />
          {addOn.text}
        </span>
      );
    case "add":
      return onAdd ? (
        <button type="button" className="mm-addon mm-addon--add reach" onClick={onAdd} disabled={busy}>
          <ImagePlus size={14} aria-hidden="true" />
          {addOn.text}
        </button>
      ) : (
        <span className="mm-addon">
          <ImagePlus size={14} aria-hidden="true" />
          {addOn.text}
        </span>
      );
    case "adding":
    case "checking":
      return (
        <span className="mm-addon" role="status">
          <Loader2 size={14} className="mm-spin" aria-hidden="true" />
          {addOn.text}
        </span>
      );
    default:
      return (
        <span className="mm-addon">
          <ImageOff size={14} aria-hidden="true" />
          {addOn.text}
        </span>
      );
  }
}

/** Whether a model fits. In use and over budget are words; only a model that fits gets a bar. */
export function FitCell({ fit, quiet = false }: { fit: FitReading; quiet?: boolean }) {
  switch (fit.state) {
    case "in_use":
      return (
        <span className="mm-fit mm-fit--use">
          <Check size={14} aria-hidden="true" />
          In use
        </span>
      );
    case "too_big":
      return (
        <span className="mm-fit mm-fit--big" title={fit.label}>
          <AlertTriangle size={14} aria-hidden="true" />
          <span>
            {fit.text}
            {fit.hint && <span className="mm-fit__hint">{fit.hint}</span>}
          </span>
        </span>
      );
    case "not_applicable":
      return null;
    case "fits":
      return quiet ? null : (
        <span className="mm-fit mm-fit--ok" role="img" aria-label={fit.label} title={fit.label}>
          <span className="mm-fit__track">
            <span className="mm-fit__fill" style={{ width: `${fit.percent ?? 0}%` }} />
          </span>
          <span className="mm-fit__pct">{fit.text}</span>
        </span>
      );
    default:
      return quiet ? null : (
        <span className="mm-fit mm-fit--none" title={fit.label}>
          {fit.text}
        </span>
      );
  }
}

/** The tick box that keeps picture support in the download. Ticked until the household says otherwise,
 *  except where only the model fits this pond: then it starts unticked and says why beside it. */
export function PicturesChoice({
  checked,
  size,
  onChange,
  disabled = false,
  leftOut = false,
}: {
  checked: boolean;
  /** "945 MB" */
  size: string;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  /** Only the model fits this pond, so the box is unticked and the reason is shown. */
  leftOut?: boolean;
}) {
  const whyId = useId();
  const why = leftOut && !checked;
  return (
    <>
      <label className="mm-choice">
        <input
          type="checkbox"
          checked={checked}
          disabled={disabled}
          aria-describedby={why ? whyId : undefined}
          onChange={(e) => onChange(e.target.checked)}
        />
        <ImagePlus size={14} aria-hidden="true" />
        <span>Include picture support</span>
        <span className="mm-choice__size">{size}</span>
      </label>
      {why && (
        <p className="mm-choice__why" id={whyId}>
          {PICTURES_LEFT_OUT}
        </p>
      )}
    </>
  );
}

function ControlButton({
  action,
  busy,
  onControl,
}: {
  action: TransferAction;
  busy: boolean;
  onControl: (action: TransferAction) => void;
}) {
  const view = {
    pause: { Icon: Pause, label: "Pause", tone: "" },
    resume: { Icon: Play, label: "Resume", tone: "" },
    cancel: { Icon: X, label: "Stop", tone: " mm-btn--danger" },
  }[action];
  return (
    <button
      type="button"
      className={`mm-btn${view.tone}`}
      onClick={() => onControl(action)}
      disabled={busy}
    >
      <view.Icon size={15} aria-hidden="true" />
      <span>{view.label}</span>
    </button>
  );
}

/** What is coming down for one model: a bar per file, one set of controls for the model. */
export function TransferView({
  transfer,
  title,
  busy = false,
  onControl,
}: {
  transfer: ModelTransfer;
  title: string;
  busy?: boolean;
  onControl: (action: TransferAction) => void;
}) {
  const finishing = transfer.state === "finishing";
  const failed = transfer.parts.some((p) => p.status === "error");
  return (
    <div className="mm-xfer" role="group" aria-label={`Downloading ${title}`}>
      {transfer.parts.map((part) => (
        <div className="mm-xfer__part" key={part.filename} data-status={part.status}>
          <span className="mm-xfer__label">{part.label}</span>
          <div
            className="mm-xfer__track"
            role="progressbar"
            aria-label={`${title}, ${part.label.toLowerCase()}`}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={part.percent ?? undefined}
          >
            <span
              className={part.percent == null ? "mm-xfer__fill mm-xfer__fill--unknown" : "mm-xfer__fill"}
              style={part.percent == null ? undefined : { width: `${part.percent}%` }}
            />
          </div>
          <span className="mm-xfer__bytes">{part.status === "done" ? "Arrived" : part.bytes}</span>
        </div>
      ))}

      {transfer.state === "paused" && (
        <p className="mm-xfer__note">{pausedText(transfer.resumable)}</p>
      )}
      {transfer.reconnecting && (
        <p className="mm-xfer__note" role="status">
          <Loader2 size={14} className="mm-spin" aria-hidden="true" />
          {reconnectingText(transfer.reconnecting)}
        </p>
      )}
      {finishing && (
        <p className="mm-xfer__note" role="status">
          <Loader2 size={14} className="mm-spin" aria-hidden="true" />
          Getting it ready
        </p>
      )}
      {transfer.error && (
        <p className="mm-xfer__error" role="alert">
          <AlertTriangle size={14} aria-hidden="true" />
          <span>{transfer.error}</span>
        </p>
      )}

      {!finishing && (
        <div className="mm-xfer__acts">
          {failed && transfer.state !== "paused" && (
            <button type="button" className="mm-btn" onClick={() => onControl("resume")} disabled={busy}>
              <RotateCw size={15} aria-hidden="true" />
              <span>Try again</span>
            </button>
          )}
          {/* A failure that kept a partial file waits here until resumed or stopped. */}
          {transfer.state === "error" && <ControlButton action="cancel" busy={busy} onControl={onControl} />}
          {transfer.state === "downloading" && (
            <>
              <ControlButton action="pause" busy={busy} onControl={onControl} />
              <ControlButton action="cancel" busy={busy} onControl={onControl} />
            </>
          )}
          {transfer.state === "paused" && (
            <>
              <ControlButton action="resume" busy={busy} onControl={onControl} />
              <ControlButton action="cancel" busy={busy} onControl={onControl} />
            </>
          )}
        </div>
      )}
    </div>
  );
}
