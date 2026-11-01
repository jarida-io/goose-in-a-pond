import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// A cheap tripwire, not coverage (the composer tests are that): both chat UIs are kept, so a
// change to the plus menu that lands in only one of them is caught by a deletion here.
const SRC_DIR = dirname(dirname(fileURLToPath(import.meta.url)));
const read = (rel: string) => readFileSync(join(SRC_DIR, rel), "utf8");

describe("both chat UIs carry the plus menu and research mode", () => {
  for (const surface of ["hub/views/ChatHub.tsx", "sections/Chat.tsx"]) {
    describe(surface, () => {
      const src = read(surface);

      it("renders the plus menu, and no longer a paperclip of its own", () => {
        expect(src.includes("<ComposerMenu"), `${surface} lost the plus menu`).toBe(true);
        expect(src.includes("Paperclip"), `${surface} draws its own paperclip beside the menu`).toBe(false);
      });

      it("shows the chip while the mode is on", () => {
        expect(src.includes("<ResearchModeChip"), `${surface} gives no sign that research mode is on`).toBe(true);
      });

      it("asks whether the pond offers research", () => {
        expect(src.includes("useDeepResearch("), `${surface} never asks whether deep research is offered`).toBe(true);
      });

      it("sends the flag with the box's text only, then leaves the mode", () => {
        expect(
          src.includes("sendTurn({ text: t, deepResearch: true, deepResearchAnswer: deepResearch.answer })") ||
            src.includes("sendTurn({ text, deepResearch: true, deepResearchAnswer: deepResearch.answer })"),
          `${surface} never marks a turn as research, or drops the length the person chose`,
        ).toBe(true);
        expect(src.includes("onAnswer={deepResearch.setAnswer}"), `${surface} gives no way to choose the length`).toBe(true);
        expect(src.includes("directText === undefined") || src.includes("raw === undefined"), `${surface} would send a chip's words as research`).toBe(true);
      });

      it("puts a refused question and its mode back through the shared refusal path", () => {
        expect(src.includes("deepResearch.refused("), `${surface} ignores the pond's refusal of research`).toBe(true);
        expect(src.includes("takeRefusedDraft"), `${surface} lost the refusal path`).toBe(true);
      });

      it("keeps the picture rules: the gate, the reason line and the file input", () => {
        expect(src.includes("gateBlocked"), `${surface} lost the picture-support gate`).toBe(true);
        expect(src.includes("setAttachReasonShown"), `${surface} lost the reason line`).toBe(true);
        expect(src.includes('type="file"'), `${surface} lost the file input Add pictures opens`).toBe(true);
      });
    });
  }

  it("the turn driver passes the flag to the stream, and never queues a research turn", () => {
    const src = read("state/chatRunStore.ts");
    expect(src.includes("turn.deepResearch ? true : undefined"), "the flag never reaches chatStream").toBe(true);
    expect(src.includes("turn.deepResearch ? turn.deepResearchAnswer : undefined"), "the length never reaches chatStream").toBe(true);
    expect(src.includes("turn.deepResearch) return"), "a research turn can be queued as an ordinary message").toBe(true);
  });

  it("the request type carries the field the pond reads", () => {
    const src = read("api/types.ts");
    const request = src.match(/export interface ChatStreamRequest \{[^}]+\}/)?.[0] ?? "";
    expect(request.includes("deep_research?: boolean"), "ChatStreamRequest has no deep_research").toBe(true);
  });
});
