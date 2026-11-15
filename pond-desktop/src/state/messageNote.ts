import type { MessageNote, NoteFromKind, NoteReceipt, SuggestionState } from "../api/types";

const FROM_KINDS: readonly NoteFromKind[] = ["schedule", "reminder", "task", "lookup", "watch", "heartbeat", "suggestion"];
const RECEIPT_KINDS = ["reminder", "todo", "timer", "schedule", "watch", "task", "standing"];
const SUGGESTION_STATES: readonly SuggestionState[] = ["open", "done", "dismissed"];

function text(v: unknown): string | undefined {
  return typeof v === "string" && v.trim() ? v : undefined;
}

/** A receipt as the wire carries it, or null for a shape this app does not know. */
export function readReceipt(raw: unknown): NoteReceipt | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  const what = text(r.what);
  if (!what || !RECEIPT_KINDS.includes(r.kind as string)) return null;
  if (r.outcome !== "set" && r.outcome !== "not_set") return null;
  return {
    kind: r.kind as NoteReceipt["kind"],
    outcome: r.outcome,
    what,
    ...(text(r.id) ? { id: text(r.id) } : {}),
    ...(text(r.reason) ? { reason: text(r.reason) } : {}),
  };
}

/** Defensive: a row's note is stored data, and a malformed one must not break the thread. */
export function readNote(raw: unknown): MessageNote | undefined {
  if (typeof raw !== "object" || raw === null) return undefined;
  const n = raw as Record<string, unknown>;
  const note: MessageNote = {};

  const from = n.from as Record<string, unknown> | undefined;
  if (from && FROM_KINDS.includes(from.kind as NoteFromKind)) {
    note.from = {
      kind: from.kind as NoteFromKind,
      label: typeof from.label === "string" ? from.label : "",
      ...(text(from.ref_id) ? { ref_id: text(from.ref_id) } : {}),
    };
  }

  if (Array.isArray(n.receipts)) {
    const receipts = n.receipts.map(readReceipt).filter((r): r is NoteReceipt => r !== null);
    if (receipts.length > 0) note.receipts = receipts;
  }

  const s = n.suggestion as Record<string, unknown> | undefined;
  if (s && text(s.noticed) && text(s.action) && text(s.prompt) && SUGGESTION_STATES.includes(s.state as SuggestionState)) {
    note.suggestion = {
      noticed: s.noticed as string,
      action: s.action as string,
      prompt: s.prompt as string,
      state: s.state as SuggestionState,
      ...(text(s.suggestion_id) ? { suggestion_id: text(s.suggestion_id) } : {}),
    };
  }

  return note.from || note.receipts || note.suggestion ? note : undefined;
}

/** One chip per call: the same receipt arriving twice, as a replayed frame would, shows once. */
export function addReceipt(note: MessageNote | undefined, receipt: NoteReceipt): MessageNote {
  const have = note?.receipts ?? [];
  const same = (r: NoteReceipt) =>
    receipt.id ? r.id === receipt.id : r.kind === receipt.kind && r.outcome === receipt.outcome && r.what === receipt.what;
  if (have.some(same)) return note ?? { receipts: have };
  return { ...note, receipts: [...have, receipt] };
}

/** The stored note is the record; the live one only fills in until it is read back. */
export function mergeStoredNote(live: MessageNote | undefined, stored: MessageNote | undefined): MessageNote | undefined {
  if (!stored) return live;
  if (!live) return stored;
  return { ...live, ...stored, receipts: stored.receipts ?? live.receipts };
}

export function fromLine(from: NonNullable<MessageNote["from"]>): string {
  const label = from.label.trim();
  switch (from.kind) {
    case "schedule": return label ? `From your schedule: ${label}` : "From your schedule";
    case "reminder": return "Reminder";
    case "task": return label ? `Task finished: ${label}` : "Task finished";
    case "lookup": return label ? `Looked up: ${label}` : "Looked up";
    case "watch": return label ? `Watch: ${label}` : "Watch";
    case "heartbeat": return label ? `From the pond: ${label}` : "From the pond";
    case "suggestion": return "Suggestion";
  }
}

export function receiptLine(r: NoteReceipt): string {
  return `${r.outcome === "set" ? "Set" : "Not set"}: ${r.what}`;
}

export const OFFER_CLOSED_LABEL: Record<Exclude<SuggestionState, "open">, string> = {
  done: "Done",
  dismissed: "Passed over",
};
