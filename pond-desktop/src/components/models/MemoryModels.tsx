import { useId, useState } from "react";
import { AlertTriangle, Check, ChevronDown, ChevronRight, Clock, Loader2, RefreshCw, RotateCw, SearchX } from "lucide-react";
import type { ModelActiveRoles, ModelEntry } from "../../api/types";
import type { ModelTransfer, TransferAction } from "../../sections/models/modelDownloads";
import {
  type EmbedderKind, type EmbedderLine, holdsMemory, memoryCost, memoryModels,
} from "../../sections/models/memoryEmbedder";
import { measuredOf, modelFullName, modelLabel, sourceChip } from "../../sections/models/modelsView";
import { SourceChip, TransferView } from "./ModelMarks";
import "../../styles/memory-models.css";

// The Memory job on a Models screen: one flat row per model it can run on, the pond's recommendation
// first, and one sentence about where the switch has got to. Both the classic page and the hub compose it.

const STATUS_ICON: Record<EmbedderKind, typeof Check> = {
  off: SearchX,
  waiting: Clock,
  loading: Loader2,
  downloading: Loader2,
  missing: Clock,
  failed: AlertTriangle,
  reindexing: RefreshCw,
  ready: Check,
};

const SPINS: EmbedderKind[] = ["loading", "downloading", "reindexing"];

/** Where the Memory embedder is, in words. Silent when the pond cannot say. */
export function EmbedderStatus({
  line,
  onRetry,
  retrying = false,
  className,
}: {
  line: EmbedderLine | null;
  onRetry?: () => void;
  retrying?: boolean;
  className?: string;
}) {
  if (!line) return null;
  const Icon = STATUS_ICON[line.kind];
  const pct = line.progress && line.progress.total > 0
    ? Math.round((line.progress.done / line.progress.total) * 100)
    : null;
  return (
    <div
      className={className ? `mm-embedder ${className}` : "mm-embedder"}
      data-kind={line.kind}
      role={line.kind === "failed" ? "alert" : "status"}
    >
      <p className="mm-embedder__line">
        <Icon
          size={14}
          className={SPINS.includes(line.kind) ? "mm-embedder__icon mm-spin" : "mm-embedder__icon"}
          aria-hidden="true"
        />
        <span>{line.text}</span>
      </p>
      {pct !== null && (
        <div
          className="mm-embedder__track"
          role="progressbar"
          aria-label="Memories re-indexed"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={pct}
        >
          <span className="mm-embedder__fill" style={{ width: `${pct}%` }} />
        </div>
      )}
      {line.detail && <p className="mm-embedder__why">{line.detail}</p>}
      {line.retry && onRetry && (
        <button type="button" className="mm-btn mm-embedder__retry" onClick={onRetry} disabled={retrying}>
          <RotateCw size={15} aria-hidden="true" />
          <span>{retrying ? line.retryingLabel : line.retryLabel}</span>
        </button>
      )}
    </div>
  );
}

function MemoryRow({
  model, inUse, transfer, busy, onUse, onControl,
}: {
  model: ModelEntry;
  inUse: boolean;
  transfer: ModelTransfer | null;
  busy: boolean;
  onUse: () => void;
  onControl: (action: TransferAction) => void;
}) {
  const title = modelLabel(model);
  const full = modelFullName(model);
  const chip = sourceChip(model);
  const measured = measuredOf(model.recommended);
  return (
    <li className="mm-mem" data-inuse={inUse ? "true" : undefined}>
      <div className="mm-mem__text">
        <span className="mm-mem__name">
          <span className="mm-mem__title">{title}</span>
          {chip && <SourceChip chip={chip} />}
        </span>
        {model.recommended && <span className="mm-mem__why">{model.recommended.reason}</span>}
        {measured && (
          <span className="mm-mem__measured">
            <span className="mm-mem__numbers">{measured.text}</span>
            <span className="mm-mem__caption">{measured.caption}</span>
          </span>
        )}
        <span className="mm-mem__cost">{memoryCost(model)}</span>
      </div>
      {/* While it comes down the bar and its controls are the row's whole state: "In use" would be early. */}
      {!transfer && (
        <div className="mm-mem__end">
          {inUse ? (
            <span className="mm-fit mm-fit--use">
              <Check size={14} aria-hidden="true" />
              In use
            </span>
          ) : (
            <button type="button" className="mm-btn" onClick={onUse} disabled={busy} aria-label={`Use ${full} for memory`}>
              Use
            </button>
          )}
        </div>
      )}
      {transfer && (
        <div className="mm-mem__xfer">
          <TransferView transfer={transfer} title={full} busy={busy} onControl={onControl} />
        </div>
      )}
    </li>
  );
}

export interface MemoryModelsProps {
  models: ModelEntry[];
  roles: ModelActiveRoles | null;
  transferFor: (m: Pick<ModelEntry, "id" | "downloaded">) => ModelTransfer | null;
  line: EmbedderLine | null;
  retrying?: boolean;
  busy: boolean;
  /** Memory is switched off: the status says so and offers to turn it on. */
  off?: boolean;
  onTurnOn?: () => void;
  onUse: (m: ModelEntry) => void;
  onControl: (m: ModelEntry, transfer: ModelTransfer, action: TransferAction) => void;
  onRetry?: () => void;
  /** A small screen shows the recommendation and the model in use, and the rest on request. */
  fold?: boolean;
}

export function MemoryModels({
  models, roles, transferFor, line, retrying, busy, off = false, onTurnOn, onUse, onControl, onRetry,
  fold = false,
}: MemoryModelsProps) {
  const [open, setOpen] = useState(!fold);
  const listId = useId();
  const rows = memoryModels(models);
  if (rows.length === 0) return null;

  const front = (m: ModelEntry, i: number) => i === 0 || holdsMemory(m, roles);
  const shown = open ? rows : rows.filter(front);
  const folded = rows.length - rows.filter(front).length;

  return (
    <div className="mm-memory">
      <p className="mm-memory__intro">
        Memory finds what the pond knows by meaning, so a question in English can find a fact said in
        Swahili. A new model reads your memories again once, at a quiet moment.
      </p>
      {off ? (
        <div className="mm-embedder" data-kind="off" role="status">
          <p className="mm-embedder__line">
            <SearchX size={14} className="mm-embedder__icon" aria-hidden="true" />
            <span>Search by meaning is off. Memory searches by keyword.</span>
          </p>
          {onTurnOn && (
            <button type="button" className="mm-btn mm-embedder__retry" onClick={onTurnOn} disabled={busy}>
              Turn on
            </button>
          )}
        </div>
      ) : (
        <EmbedderStatus line={line} onRetry={onRetry} retrying={retrying} />
      )}
      <ul className="mm-memory__rows" id={listId} aria-label="Memory models">
        {shown.map((m) => (
          <MemoryRow
            key={m.id}
            model={m}
            inUse={holdsMemory(m, roles)}
            transfer={transferFor(m)}
            busy={busy}
            onUse={() => onUse(m)}
            onControl={(action) => {
              const transfer = transferFor(m);
              if (transfer) onControl(m, transfer, action);
            }}
          />
        ))}
      </ul>
      {fold && folded > 0 && (
        <button
          type="button"
          className="mm-btn mm-memory__more"
          aria-expanded={open}
          aria-controls={listId}
          onClick={() => setOpen((v) => !v)}
        >
          {open ? <ChevronDown size={15} aria-hidden="true" /> : <ChevronRight size={15} aria-hidden="true" />}
          <span>{open ? "Show fewer Memory models" : `Other Memory models (${folded})`}</span>
        </button>
      )}
    </div>
  );
}
