// Turning something down with a reason: stopping an anchor, or rejecting one deliverable. What
// the member says goes on their don't-list, which every maker and critic reads.

import { useId, useState } from "react";
import { InkButton, InkFormField, InkSheet, InkSwitch, InkTextArea } from "@jarida/ink/react";
import type { ReasonChip, RejectDeliverable, StopAnchor } from "../api/types";
import {
  CANCEL,
  IN_YOUR_WORDS,
  NEVER_AGAIN,
  NEVER_THIS,
  NEVER_THIS_DESCRIPTION,
  NOT_THIS_DESCRIPTION,
  NOT_THIS_ONE,
  REASONS_LABEL,
  STOP_DESCRIPTION,
  STOP_IT,
  WHAT_WAS_WRONG,
  buttonSize,
  type Variant,
} from "./myGooseWords";
import { problemWords } from "./problems";
import { ActionProblem, ReasonChips } from "./MyGooseParts";

function wordsOf(text: string): string | undefined {
  const trimmed = text.trim();
  return trimmed === "" ? undefined : trimmed;
}

function useSending(): { busy: boolean; problem: string | null; send: (work: () => Promise<void>) => Promise<void> } {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  async function send(work: () => Promise<void>) {
    setBusy(true);
    setProblem(null);
    try {
      await work();
    } catch (e) {
      setProblem(problemWords(e));
      setBusy(false);
    }
  }
  return { busy, problem, send };
}

interface StopProps {
  anchorTitle: string;
  variant: Variant;
  onClose: () => void;
  onStop: (stop: StopAnchor) => Promise<void>;
}

export function StopSheet({ anchorTitle, variant, onClose, onStop }: StopProps) {
  const ids = useId();
  const [reasons, setReasons] = useState<ReasonChip[]>([]);
  const [text, setText] = useState("");
  const [never, setNever] = useState(false);
  const { busy, problem, send } = useSending();

  return (
    <InkSheet open onClose={busy ? () => {} : onClose} title={`Stop ${anchorTitle}`} description={STOP_DESCRIPTION}>
      <ReasonChips label={REASONS_LABEL} value={reasons} onChange={setReasons} />
      <InkFormField id={`${ids}-words`} label={WHAT_WAS_WRONG}>
        <InkTextArea
          label={WHAT_WAS_WRONG}
          value={text}
          placeholder={IN_YOUR_WORDS}
          rows={2}
          inputProps={{ id: `${ids}-words-control` }}
          onChange={setText}
        />
      </InkFormField>
      <InkSwitch checked={never} onChange={setNever}>
        {NEVER_AGAIN}
      </InkSwitch>
      <div className="mg__actions">
        <InkButton
          variant="danger"
          size={buttonSize(variant)}
          loading={busy}
          onPress={() => void send(() => onStop({ reasons, text: wordsOf(text), never }))}
        >
          {STOP_IT}
        </InkButton>
        <InkButton size={buttonSize(variant)} disabled={busy} onPress={onClose}>
          {CANCEL}
        </InkButton>
      </div>
      <ActionProblem text={problem} />
    </InkSheet>
  );
}

interface RejectProps {
  verb: RejectDeliverable["verb"];
  variant: Variant;
  onClose: () => void;
  onReject: (rejection: RejectDeliverable) => Promise<void>;
}

/** "Not this one" sets one deliverable aside; "Never this" asks for a general rule from the member's words. */
export function RejectSheet({ verb, variant, onClose, onReject }: RejectProps) {
  const ids = useId();
  const [reasons, setReasons] = useState<ReasonChip[]>([]);
  const [text, setText] = useState("");
  const { busy, problem, send } = useSending();
  const label = verb === "never" ? NEVER_THIS : NOT_THIS_ONE;

  return (
    <InkSheet
      open
      onClose={busy ? () => {} : onClose}
      title={label}
      description={verb === "never" ? NEVER_THIS_DESCRIPTION : NOT_THIS_DESCRIPTION}
     
    >
      <ReasonChips label={REASONS_LABEL} value={reasons} onChange={setReasons} />
      <InkFormField id={`${ids}-words`} label={WHAT_WAS_WRONG}>
        <InkTextArea
          label={WHAT_WAS_WRONG}
          value={text}
          placeholder={IN_YOUR_WORDS}
          rows={2}
          inputProps={{ id: `${ids}-words-control` }}
          onChange={setText}
        />
      </InkFormField>
      <div className="mg__actions">
        <InkButton
          variant="danger"
          size={buttonSize(variant)}
          loading={busy}
          onPress={() => void send(() => onReject({ verb, reasons, text: wordsOf(text) }))}
        >
          {label}
        </InkButton>
        <InkButton size={buttonSize(variant)} disabled={busy} onPress={onClose}>
          {CANCEL}
        </InkButton>
      </div>
      <ActionProblem text={problem} />
    </InkSheet>
  );
}
