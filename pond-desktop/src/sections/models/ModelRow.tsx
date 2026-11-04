import { Download, Trash2 } from "lucide-react";
import i18n from "../../i18n";
import type { HelperModel, ModelEntry, ModelMemoryStatus, ModelUse } from "../../api/types";
import {
  AddOnLine, FitCell, PicturesChoice, SourceChip, TransferView, UseChips,
} from "../../components/models/ModelMarks";
import type { ModelTransfer, TransferAction } from "./modelDownloads";
import {
  type RoleKey, ROLES, addOnOf, downloadSummary, fitReading, fitsOnlyWithoutPictures, formatBytes,
  formatSize, modelFacts, modelFullName, modelLabel, offersPictures, picturesOf, sourceChip,
  unavailableNote, jobLabel,
} from "./modelsView";

export interface ModelRowProps {
  model: ModelEntry;
  memory: ModelMemoryStatus | null;
  inUse: boolean;
  /** The jobs this row can take: one "Use" each. */
  roles: RoleKey[];
  /** The jobs this row is doing now, each shown as a chip by its name. */
  uses: ModelUse[];
  transfer: ModelTransfer | null;
  /** The household's choice on the tick box, for a row not yet on the device. */
  withPictures: boolean;
  busy: boolean;
  onWithPictures: (next: boolean) => void;
  onUse: (role: RoleKey) => void;
  onDelete: () => void;
  onDownload: () => void;
  onAddPictures: () => void;
  onControl: (action: TransferAction) => void;
}

/** One model, on the device or still to fetch. It says what it is, what it costs and what it can do. */
export function ModelRow({
  model, memory, inUse, roles, uses, transfer, withPictures, busy, onWithPictures, onUse, onDelete,
  onDownload, onAddPictures, onControl,
}: ModelRowProps) {
  const title = modelLabel(model);
  const full = modelFullName(model);
  const chip = sourceChip(model);
  const facts = modelFacts(model);
  const addOn = addOnOf(model);
  const onDevice = model.downloaded === true;
  const choice = offersPictures(model);
  const fit = fitReading(model, memory, { inUse, withPictures: choice && withPictures });
  const pictures = picturesOf(model);
  const unavailable = unavailableNote(model);
  const canDownload =
    !onDevice && model.acquire !== "external" && model.acquire !== "unavailable" && !unavailable;
  const managedElsewhere = model.acquire === "external";

  return (
    <div className="mdl-row" data-fit={fit.state} data-inuse={inUse ? "true" : undefined}>
      <div className="mdl-row__id">
        <div className="mdl-row__idline">
          <span className="mdl-row__name" title={title}>{title}</span>
          <UseChips uses={uses} />
          {chip && <SourceChip chip={chip} />}
        </div>
        {facts.length > 0 && <span className="mdl-row__facts">{facts.join(" · ")}</span>}
        {addOn.kind !== "none" && (
          <div className="mdl-row__addon">
            {choice && pictures ? (
              <PicturesChoice
                checked={withPictures}
                size={formatBytes(pictures.size_bytes)}
                onChange={onWithPictures}
                disabled={busy || transfer !== null}
                leftOut={fitsOnlyWithoutPictures(model, memory)}
              />
            ) : (
              <AddOnLine
                addOn={addOn}
                busy={busy}
                onAdd={onDevice && addOn.kind === "add" ? onAddPictures : undefined}
              />
            )}
          </div>
        )}
      </div>

      <span className="mdl-row__size">
        {onDevice
          ? formatSize(model.size_mb ?? model.ram_estimate_mb) || "—"
          : downloadSummary(model, choice && withPictures)}
      </span>

      <div className="mdl-row__fit">
        <FitCell fit={fit} />
      </div>

      {transfer ? (
        <div className="mdl-row__xfer">
          <TransferView transfer={transfer} title={full} busy={busy} onControl={onControl} />
        </div>
      ) : (
        <div className="mdl-row__acts">
          {onDevice &&
            !inUse &&
            !unavailable &&
            roles.map((role) => (
              <button
                key={role}
                type="button"
                className="mm-btn"
                onClick={() => onUse(role)}
                disabled={busy}
                aria-label={`Use ${full} for ${ROLES.find((r) => r.key === role)?.label.toLowerCase()}`}
              >
                Use
              </button>
            ))}
          {onDevice && !managedElsewhere && (
            <button
              type="button"
              className="mm-btn mm-btn--danger"
              onClick={onDelete}
              disabled={busy}
              aria-label={`Delete ${full}`}
            >
              <Trash2 size={15} aria-hidden="true" />
              <span>Delete</span>
            </button>
          )}
          {canDownload && (
            <button
              type="button"
              className="mm-btn"
              onClick={onDownload}
              disabled={busy}
              aria-label={`Download ${full}`}
            >
              <Download size={15} aria-hidden="true" />
              <span>Download</span>
            </button>
          )}
          {unavailable ? (
            <span className="mdl-row__note">{unavailable}</span>
          ) : (
            !onDevice &&
            !canDownload &&
            !managedElsewhere && <span className="mdl-row__note">No source to download from</span>
          )}
        </div>
      )}
    </div>
  );
}

/** A model the pond has in use that is no catalogue row of its own, such as picture support or the
 *  camera's detector: named by the pond where it can be, else by its job. It has nothing to do. */
export function HelperRow({ helper, serves }: { helper: HelperModel; serves: ModelEntry | null }) {
  const job = helper.uses[0] ? jobLabel(helper.uses[0]) : null;
  const title = helper.title?.trim() || job || helper.id;
  return (
    <div className="mdl-row" data-inuse="true">
      <div className="mdl-row__id">
        <div className="mdl-row__idline">
          <span className="mdl-row__name" title={title}>{title}</span>
          <UseChips uses={helper.uses} />
        </div>
        {serves && (
          <span className="mdl-row__facts">
            {i18n.t("models.serves", { name: modelLabel(serves) })}
          </span>
        )}
      </div>
    </div>
  );
}
