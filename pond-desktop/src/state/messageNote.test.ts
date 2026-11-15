import { describe, expect, it } from "vitest";
import { addReceipt, fromLine, mergeStoredNote, readNote, receiptLine } from "./messageNote";

describe("readNote", () => {
  it("reads a full note", () => {
    const note = readNote({
      from: { kind: "schedule", label: "Daily Briefing", ref_id: "s1" },
      receipts: [{ kind: "reminder", outcome: "not_set", what: "call Sam", reason: "No time given" }],
      suggestion: { noticed: "You ask at 8", action: "Make it a routine", prompt: "set it up", state: "open", suggestion_id: "g1" },
    });
    expect(note?.from?.label).toBe("Daily Briefing");
    expect(note?.receipts?.[0]).toMatchObject({ outcome: "not_set", reason: "No time given" });
    expect(note?.suggestion?.state).toBe("open");
  });

  it("drops what it does not know instead of throwing", () => {
    expect(readNote(null)).toBeUndefined();
    expect(readNote({ from: { kind: "bogus", label: "x" }, receipts: [{ kind: "reminder", outcome: "maybe", what: "x" }], suggestion: { noticed: "a" } })).toBeUndefined();
  });
});

describe("addReceipt", () => {
  const r = { kind: "timer", outcome: "set", what: "10 minutes" } as const;
  it("collects in order and ignores a replayed frame", () => {
    let note = addReceipt(undefined, r);
    note = addReceipt(note, { kind: "todo", outcome: "set", what: "milk", id: "t1" });
    note = addReceipt(note, r);
    expect(note.receipts).toHaveLength(2);
  });
  it("keeps two receipts with distinct ids that read alike", () => {
    let note = addReceipt(undefined, { kind: "todo", outcome: "set", what: "milk", id: "a" });
    note = addReceipt(note, { kind: "todo", outcome: "set", what: "milk", id: "b" });
    expect(note.receipts).toHaveLength(2);
  });
});

describe("mergeStoredNote", () => {
  it("prefers the stored receipts and keeps the live ones when none are stored", () => {
    const live = { receipts: [{ kind: "timer", outcome: "set", what: "a" } as const] };
    expect(mergeStoredNote(live, undefined)).toBe(live);
    expect(mergeStoredNote(live, { from: { kind: "task", label: "x" } })?.receipts).toEqual(live.receipts);
    expect(mergeStoredNote(live, { receipts: [{ kind: "timer", outcome: "not_set", what: "a" }] })?.receipts?.[0].outcome).toBe("not_set");
  });
});

describe("wording", () => {
  it("names where a message came from", () => {
    expect(fromLine({ kind: "schedule", label: "Daily Briefing" })).toBe("From your schedule: Daily Briefing");
    expect(fromLine({ kind: "reminder", label: "call Sam" })).toBe("Reminder");
    expect(fromLine({ kind: "task", label: "Report" })).toBe("Task finished: Report");
    expect(fromLine({ kind: "lookup", label: "Flights" })).toBe("Looked up: Flights");
    expect(fromLine({ kind: "watch", label: "Bike" })).toBe("Watch: Bike");
    expect(fromLine({ kind: "suggestion", label: "" })).toBe("Suggestion");
  });
  it("says set or not set", () => {
    expect(receiptLine({ kind: "timer", outcome: "set", what: "10 minutes" })).toBe("Set: 10 minutes");
    expect(receiptLine({ kind: "timer", outcome: "not_set", what: "10 minutes" })).toBe("Not set: 10 minutes");
  });
});
