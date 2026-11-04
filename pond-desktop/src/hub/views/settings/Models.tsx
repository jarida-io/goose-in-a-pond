import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Brain, ChevronRight, Ear, MessageSquare, RefreshCw, Volume2, type LucideIcon } from "lucide-react";
import { DetailShell } from "./DetailShell";
import { Card } from "./controls";
import { useModels } from "../../../hooks/useModels";
import { useModelActions } from "../../../hooks/useModelActions";
import {
  AddOnLine, EngineMark, FitCell, ServedMarks, SourceChip, TransferView, UseChips,
} from "../../../components/models/ModelMarks";
import { PickCard } from "../../../sections/models/PickCard";
import { HolderBadge, MemoryModels } from "../../../components/models/MemoryModels";
import { jobState } from "../../../sections/models/memoryEmbedder";
import { useMemoryJob } from "../../../hooks/useMemoryJob";
import { engineOf, groupByEngine } from "../../../lib/modelProvider";
import {
  ROLES, type RoleKey, addOnOf, askingPick, budgetReading, emptyJobText, fitReading,
  groupBySection, helpersOf, holderEntry, isInUse, measuredOf, modelFacts, modelFullName, modelLabel,
  raisedPick, recommendedPicks, roleHolder, rolesFor, servedUses, sortModels, sourceChip,
  unavailableNote, usesOf,
} from "../../../sections/models/modelsView";
import type { HelperModel, ModelEntry, ModelUse } from "../../../api/types";

const ROLE_ICON: Record<RoleKey, LucideIcon> = {
  chat: MessageSquare,
  asr: Ear,
  tts: Volume2,
  embedding: Brain,
};

// ─── One installed model ─────────────────────────────────────

function HubRow({
  model, inUse, uses, helpers, raised, actions, memory, transfer,
}: {
  model: ModelEntry;
  inUse: boolean;
  /** The jobs this row is doing now, each shown as a chip. */
  uses: ModelUse[];
  /** The models in use that serve this row, such as its drafter. */
  helpers: HelperModel[];
  raised: boolean;
  actions: ReturnType<typeof useModelActions>;
  memory: ReturnType<typeof useModels>["memory"];
  transfer: ReturnType<ReturnType<typeof useModels>["transferFor"]>;
}) {
  const title = modelLabel(model);
  const full = modelFullName(model);
  const chip = sourceChip(model);
  const facts = modelFacts(model);
  const addOn = addOnOf(model);
  const served = servedUses(helpers, addOn);
  const measured = measuredOf(model.recommended);
  const fit = fitReading(model, memory, { inUse });
  const role = rolesFor(model)[0];
  const unavailable = unavailableNote(model);

  return (
    <div className="hm-row" data-inuse={inUse ? "true" : undefined} data-raised={raised ? "true" : undefined}>
      <div className="hm-row__text">
        <span className="hm-row__name">
          <span className="hm-row__title">{title}</span>
          <UseChips uses={uses} />
          {chip && <SourceChip chip={chip} />}
        </span>
        {facts.length > 0 && <span className="hm-row__facts">{facts.join(" · ")}</span>}
        {model.recommended && <span className="hm-row__why">{model.recommended.reason}</span>}
        {measured && <span className="hm-row__facts">{measured.text}</span>}
        {addOn.kind !== "none" && (
          <AddOnLine
            addOn={addOn}
            busy={actions.busy}
            onAdd={addOn.kind === "add" ? () => void actions.addPictures(model) : undefined}
          />
        )}
        <ServedMarks uses={served} />
      </div>

      {transfer ? (
        <div className="hm-row__xfer">
          <TransferView
            transfer={transfer}
            title={full}
            busy={actions.busy}
            onControl={(action) => void actions.control(model, transfer, action)}
          />
        </div>
      ) : (
        <div className="hm-row__end">
          {inUse ? (
            <FitCell fit={fit} />
          ) : (
            <>
              <FitCell fit={fit} quiet />
              {unavailable && <span className="hm-row__note">{unavailable}</span>}
              {role && !unavailable && (
                <button
                  type="button"
                  className="mm-btn"
                  disabled={actions.busy}
                  onClick={() => void actions.useFor(model, role)}
                  aria-label={`Use ${full} for ${ROLES.find((r) => r.key === role)?.label.toLowerCase()}`}
                >
                  Use
                </button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

/** The card that changes each job; listening and the voice share the Speech card. */
const JOB_CARD: Record<RoleKey, string> = {
  chat: "hm-card-conversation",
  asr: "hm-card-speech",
  tts: "hm-card-speech",
  embedding: "hm-card-memory",
};

/** Takes the person to a card, gently unless they asked for no motion, and puts focus on it. */
function openCard(id: string) {
  const el = document.getElementById(id);
  if (!el) return;
  const calm = typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  el.scrollIntoView?.({ behavior: calm ? "auto" : "smooth", block: "start" });
  el.focus({ preventScroll: true });
}

function SkeletonRow() {
  return (
    <div className="hm-row hm-row--skeleton" aria-hidden="true">
      <div className="hm-row__text">
        <span className="hm-skel hm-skel--wide" />
        <span className="hm-skel" />
      </div>
    </div>
  );
}

// ─── The screen ──────────────────────────────────────────────

interface ModelsDetailProps {
  go: (route: string) => void;
}

export function ModelsDetail({ go }: ModelsDetailProps) {
  const data = useModels();
  const { models, roles, memory, loading, error, transferFor } = data;
  const [flash, setFlash] = useState<{ text: string; ok: boolean } | null>(null);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const say = useCallback((text: string, ok = true) => {
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlash({ text, ok });
    flashTimer.current = setTimeout(() => setFlash(null), ok ? 4500 : 7000);
  }, []);
  useEffect(() => () => { if (flashTimer.current) clearTimeout(flashTimer.current); }, []);

  const actions = useModelActions(data, say);
  const job = useMemoryJob(data, actions, say);

  const groups = useMemo(
    () => groupBySection(models, roles?.helpers ?? [], roles),
    [models, roles],
  );
  const conversation = useMemo(
    () => groups.find((g) => g.key === "conversation")?.models ?? [],
    [groups],
  );
  const listening = useMemo(
    () => groups.find((g) => g.key === "listening")?.models ?? [],
    [groups],
  );
  const picks = useMemo(() => recommendedPicks(models), [models]);
  // Picks already here are listed with the models on the device, which say why they are picks.
  const toGet = useMemo(() => picks.filter((m) => !m.downloaded), [picks]);
  const comingDown = useCallback((m: ModelEntry) => transferFor(m) !== null, [transferFor]);

  const asking = askingPick(picks.filter((m) => !m.downloaded), roles, comingDown);
  const inUseNow = conversation.find((m) => isInUse(m, roles)) ?? null;
  const raisedInstalled = inUseNow;
  const raisedCard = inUseNow ? null : raisedPick(toGet, roles);

  const budget = budgetReading(memory);
  const subtitle =
    budget.text === "—"
      ? "Everything runs on this pond."
      : `Everything runs on this pond. Room for one model: ${budget.text}.`;
  const voice = holderEntry(models, roles, "tts");

  const rowFor = (m: ModelEntry) => (
    <HubRow
      key={m.id}
      model={m}
      inUse={isInUse(m, roles)}
      uses={usesOf(m, roles)}
      helpers={helpersOf(m, roles?.helpers ?? [])}
      raised={raisedInstalled?.id === m.id}
      actions={actions}
      memory={memory}
      transfer={transferFor(m)}
    />
  );

  return (
    <DetailShell
      title="Models"
      subtitle={subtitle}
      onBack={() => go("settings")}
      headRight={
        <button
          className="hm-iconbtn"
          type="button"
          onClick={() => void data.reload()}
          aria-label="Refresh models"
        >
          <RefreshCw size={18} strokeWidth={2} className={loading ? "hm-dim" : undefined} aria-hidden="true" />
        </button>
      }
    >
      {flash && (
        <div className={flash.ok ? "hm-flash" : "hm-flash hm-flash--bad"} role={flash.ok ? "status" : "alert"}>
          {flash.text}
        </div>
      )}

      {error && (
        <div className="hm-flash hm-flash--bad" role="alert">
          Could not reach the server: {error}
        </div>
      )}

      {/* GIAP's picks lead, as on the full Models page; the jobs follow. */}
      {toGet.length > 0 && (
        <Card title="Recommended for this pond">
          <div className="hm-picks">
            {toGet.map((m) => {
              const transfer = transferFor(m);
              return (
                <PickCard
                  key={m.id}
                  model={m}
                  memory={memory}
                  inUse={false}
                  raised={raisedCard?.id === m.id}
                  asking={asking?.id === m.id}
                  transfer={transfer}
                  withPictures={actions.withPictures(m)}
                  busy={actions.busy}
                  onWithPictures={(next) => actions.setWithPictures(m, next)}
                  onUse={() => void actions.useFor(m, "chat")}
                  onDownload={() => void actions.download(m)}
                  onControl={(action) => transfer && void actions.control(m, transfer, action)}
                />
              );
            })}
          </div>
        </Card>
      )}

      <div className="hm-tiles" role="list" aria-label="Which model does each job">
        {ROLES.map((role) => {
          const Icon = ROLE_ICON[role.key];
          const isMemory = role.key === "embedding";
          const entry = isMemory ? job.holder?.entry ?? null : holderEntry(models, roles, role.key);
          const name = isMemory ? job.holder?.name ?? null : roleHolder(roles, role.key);
          const engine = entry ? engineOf(entry) : null;
          const empty = emptyJobText(roles, role.key);
          const state = isMemory && job.holder ? jobState(job.line, { checking: job.checking }) : null;
          const shown = name ? (entry ? modelLabel(entry) : name) : empty ?? "Nothing chosen yet";
          // Memory has a card only while the pond lists a model for it.
          const card = isMemory && job.rows.length === 0 ? null : JOB_CARD[role.key];
          const body = (
            <>
              <span className="hm-tile__icon"><Icon size={18} aria-hidden="true" /></span>
              {card && <ChevronRight size={16} className="hm-tile__go" aria-hidden="true" />}
              <span className="hm-tile__job">{role.label}</span>
              <span className="hm-tile__holder">{loading ? <span className="hm-skel" /> : shown}</span>
              {!loading && engine && (
                <EngineMark engine={engine} label={engine.label} format={engine.file_format} />
              )}
              {!loading && state && (
                <span className="hm-tile__state"><HolderBadge badge={state} /></span>
              )}
              {!loading && isMemory && job.holder?.builtIn && (
                <span className="hm-tile__note">Built-in default</span>
              )}
            </>
          );
          const vacant = name || empty ? undefined : "true";
          return (
            <div key={role.key} className="hm-tiles__cell" role="listitem">
              {card ? (
                <button
                  type="button"
                  className="hm-tile"
                  data-empty={vacant}
                  onClick={() => openCard(card)}
                  aria-label={[
                    `${role.label}: ${loading ? "loading" : shown}`,
                    !loading && state ? state.label : null,
                    !loading && isMemory && job.holder?.builtIn ? "built-in default" : null,
                  ].filter(Boolean).join(", ") + ". Change"}
                >
                  {body}
                </button>
              ) : (
                <div className="hm-tile" data-empty={vacant}>{body}</div>
              )}
            </div>
          );
        })}
      </div>

      <Card title="Conversation" id={JOB_CARD.chat}>
        {loading ? (
          <>
            <SkeletonRow />
            <SkeletonRow />
          </>
        ) : conversation.length === 0 ? (
          <p className="hm-empty">
            Nothing is on this device to talk with yet.
            {toGet.length > 0 ? " Download one of the picks above." : ""}
          </p>
        ) : (
          groupByEngine(sortModels(conversation)).map((section) => (
            <div key={section.key} className="hm-engine">
              <div className="hm-engine__head">
                <EngineMark engine={{ id: section.key }} label={section.label} format={section.format} />
                <p className="hm-engine__blurb">{section.blurb}</p>
              </div>
              {section.models.map(rowFor)}
            </div>
          ))
        )}
      </Card>

      <Card title="Speech" id={JOB_CARD.asr}>
        {loading ? (
          <SkeletonRow />
        ) : (
          <>
            {listening.length === 0 ? (
              <p className="hm-empty">
                No listening model is on this device yet. Add one from the Models page on a computer.
              </p>
            ) : (
              listening.map(rowFor)
            )}
            <div className="hm-row hm-voice">
              <div className="hm-row__text">
                <span className="hm-row__name">
                  <span className="hm-row__title">Voice</span>
                </span>
                <span className="hm-row__facts">
                  {voice ? modelLabel(voice) : "Not chosen yet"}
                </span>
              </div>
              <div className="hm-row__end">
                <button type="button" className="mm-btn" onClick={() => go("voice")}>
                  Choose a voice
                </button>
              </div>
            </div>
          </>
        )}
      </Card>

      {job.rows.length > 0 && (
        <Card title="Memory" id={JOB_CARD.embedding}>
          <MemoryModels
            models={models}
            roles={roles}
            transferFor={transferFor}
            line={job.line}
            checking={job.checking}
            retrying={job.retrying}
            busy={actions.busy}
            off={job.off}
            offline={job.offline}
            onTurnOn={() => void job.turnOn()}
            onUse={(m) => void job.useModel(m)}
            onControl={(m, transfer, action) => void actions.control(m, transfer, action)}
            onRetry={() => void job.retry()}
            fold
          />
        </Card>
      )}
    </DetailShell>
  );
}

