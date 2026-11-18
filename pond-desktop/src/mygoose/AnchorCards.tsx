// An anchor as a card: a candidate Goose asks about, or one it is working on.

import { useState } from "react";
import { ChevronRight, Loader2 } from "lucide-react";
import { InkBadge, InkButton, InkCard } from "@jarida/ink/react";
import type { Anchor } from "../api/types";
import {
  CONFIRM,
  NOT_NOW,
  WHY_PREFIX,
  anchorMeta,
  badgeWords,
  buttonSize,
  deliverableKindWords,
  deliverableStateWords,
  jobLine,
  sharedByWords,
  type Variant,
} from "./myGooseWords";
import { problemWords } from "./problems";
import { ActionProblem } from "./MyGooseParts";

interface CandidateProps {
  anchor: Anchor;
  variant: Variant;
  onConfirm: (anchor: Anchor) => Promise<void>;
  onDismiss: (anchor: Anchor) => Promise<void>;
}

export function CandidateCard({ anchor, variant, onConfirm, onDismiss }: CandidateProps) {
  const [busy, setBusy] = useState<"confirm" | "dismiss" | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  async function act(which: "confirm" | "dismiss") {
    setBusy(which);
    setProblem(null);
    try {
      await (which === "confirm" ? onConfirm(anchor) : onDismiss(anchor));
    } catch (e) {
      setProblem(problemWords(e));
      setBusy(null);
    }
  }

  return (
    <InkCard className="mg-anchor mg-anchor--candidate">
      <h3 className="mg-anchor__title">{anchor.title}</h3>
      <p className="mg-anchor__meta">{anchorMeta(anchor)}</p>
      {anchor.why.trim() !== "" ? (
        <p className="mg-anchor__why">
          <span className="mg-anchor__why-label">{WHY_PREFIX}</span> {anchor.why}
        </p>
      ) : null}
      <div className="mg__actions">
        <InkButton
          variant="primary"
          size={buttonSize(variant)}
          loading={busy === "confirm"}
          disabled={busy !== null}
          onPress={() => void act("confirm")}
        >
          {CONFIRM}
        </InkButton>
        <InkButton size={buttonSize(variant)} loading={busy === "dismiss"} disabled={busy !== null} onPress={() => void act("dismiss")}>
          {NOT_NOW}
        </InkButton>
      </div>
      <ActionProblem text={problem} />
    </InkCard>
  );
}

export function AnchorCard({ anchor, onOpen }: { anchor: Anchor; onOpen: (anchor: Anchor) => void }) {
  const badges = badgeWords(anchor);
  return (
    <InkCard pressable className="mg-anchor" onPress={() => onOpen(anchor)}>
      <span className="mg-anchor__head">
        <span className="mg-anchor__title">{anchor.title}</span>
        <ChevronRight size={18} aria-hidden="true" className="mg-anchor__go" />
      </span>
      <span className="mg-anchor__meta">{anchorMeta(anchor)}</span>
      {anchor.mine ? null : <span className="mg-anchor__shared">{sharedByWords(anchor.shared_by)}</span>}
      {anchor.deliverables.length > 0 ? (
        <span className="mg-anchor__made">
          {anchor.deliverables.map((d) => (
            <span key={d.id} className="mg-anchor__made-row">
              <span className="mg-anchor__made-name">{deliverableKindWords(d.kind)}</span>
              <span className="mg-anchor__made-state">{deliverableStateWords(d)}</span>
            </span>
          ))}
        </span>
      ) : null}
      {anchor.job ? (
        <span className="mg-anchor__job">
          {anchor.job.state === "running" ? <Loader2 size={14} aria-hidden="true" className="animate-spin" /> : null}
          {jobLine(anchor.job)}
        </span>
      ) : null}
      {badges.length > 0 ? (
        <span className="mg-anchor__badges">
          {badges.map((badge) => (
            <InkBadge key={badge}>
              {badge}
            </InkBadge>
          ))}
        </span>
      ) : null}
    </InkCard>
  );
}
