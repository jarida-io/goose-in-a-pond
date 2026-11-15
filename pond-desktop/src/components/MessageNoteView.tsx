import { useState } from "react";
import { AlertTriangle, Check, Loader2 } from "lucide-react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import type { MessageNote, NoteReceipt } from "../api/types";
import { openSession, patchMessage, sendTurn } from "../state/chatRunStore";
import { OFFER_CLOSED_LABEL, fromLine, receiptLine } from "../state/messageNote";

/** The one quiet line above a message that the pond wrote on its own. */
export function NoteFromLine({ note }: { note: MessageNote | undefined }) {
  if (!note?.from) return null;
  return <p className="msg-note__from">{fromLine(note.from)}</p>;
}

function Chip({ receipt }: { receipt: NoteReceipt }) {
  const notSet = receipt.outcome === "not_set";
  return (
    <li className={`msg-note__chip${notSet ? " msg-note__chip--problem" : ""}`}>
      {notSet ? <AlertTriangle size={12} aria-hidden /> : <Check size={12} aria-hidden />}
      <span>
        {receiptLine(receipt)}
        {notSet && receipt.reason && <span className="msg-note__reason">{receipt.reason}</span>}
      </span>
    </li>
  );
}

/** What the turn set up, from stored or streamed receipts only. */
export function ReceiptChips({ note }: { note: MessageNote | undefined }) {
  if (!note?.receipts || note.receipts.length === 0) return null;
  return (
    <ul className="msg-note__chips" aria-label="What was set up">
      {note.receipts.map((r, i) => (
        <Chip key={`${r.id ?? r.what}-${i}`} receipt={r} />
      ))}
    </ul>
  );
}

interface OfferProps {
  /** The bubble's local id, so the answer can be written back to it. */
  localId: number;
  /** session_messages.id; the offer cannot be answered until the turn has been read back. */
  messageId: string | undefined;
  sessionId: string | undefined;
  note: MessageNote | undefined;
}

/** A suggestion the pond left in the thread: what it noticed, and one press to take it up or pass. */
export function SuggestionOffer({ localId, messageId, sessionId, note }: OfferProps) {
  const [pending, setPending] = useState(false);
  const [failed, setFailed] = useState(false);
  const offer = note?.suggestion;
  if (!offer) return null;

  if (offer.state !== "open") {
    return (
      <div className="msg-note__offer">
        <p className="msg-note__noticed">{offer.noticed}</p>
        <span className="msg-note__closed">{OFFER_CLOSED_LABEL[offer.state]}</span>
      </div>
    );
  }

  async function answer(take: boolean) {
    if (!sessionId || !messageId || !offer) return;
    setPending(true);
    setFailed(false);
    try {
      const result = await api.answerOffer(sessionId, messageId, take);
      patchMessage(localId, { note: { ...note, suggestion: { ...offer, state: result.state } } });
      if (take) sendTurn({ text: result.prompt ?? offer.prompt });
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) void openSession(sessionId);
      else setFailed(true);
    } finally {
      setPending(false);
    }
  }

  const ready = Boolean(sessionId && messageId) && !pending;
  return (
    <div className="msg-note__offer">
      <p className="msg-note__noticed">{offer.noticed}</p>
      <div className="msg-note__offerActions">
        <button type="button" className="msg-note__btn msg-note__btn--primary" disabled={!ready} onClick={() => void answer(true)}>
          {pending && <Loader2 size={12} className="msg-note__spin" aria-hidden />}
          {offer.action}
        </button>
        <button type="button" className="msg-note__btn" disabled={!ready} onClick={() => void answer(false)}>
          Not now
        </button>
      </div>
      {failed && <p className="msg-note__noticed" role="alert">The pond did not answer. Try again in a moment.</p>}
    </div>
  );
}
