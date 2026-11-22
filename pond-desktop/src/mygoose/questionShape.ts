// How a structured question is laid out: Goose's pick first, then the rest in the order asked.

import type { QuestionDraft, QuestionOption } from "../api/types";

export interface ShownOption extends QuestionOption {
  recommended: boolean;
}

export function shownOptions(question: QuestionDraft): ShownOption[] {
  const all = question.options.map((option, at) => ({ ...option, recommended: at === question.recommended }));
  return [...all.filter((o) => o.recommended), ...all.filter((o) => !o.recommended)];
}

/** The tool_call input of `giap-questions__ask`, read leniently: options may come as plain strings. */
export function askedQuestions(input: unknown): QuestionDraft[] {
  const raw = (input as { questions?: unknown } | null)?.questions;
  if (!Array.isArray(raw)) return [];
  return raw.flatMap((item): QuestionDraft[] => {
    if (!item || typeof item !== "object") return [];
    const q = item as Record<string, unknown>;
    const text = typeof q.text === "string" ? q.text : typeof q.question === "string" ? q.question : "";
    const options = (Array.isArray(q.options) ? q.options : []).flatMap((o): QuestionOption[] => {
      if (typeof o === "string") return [{ label: o, description: "" }];
      if (o && typeof o === "object" && typeof (o as { label?: unknown }).label === "string") {
        const option = o as { label: string; description?: unknown };
        return [{ label: option.label, description: typeof option.description === "string" ? option.description : "" }];
      }
      return [];
    });
    if (!text.trim() || options.length === 0) return [];
    return [
      {
        header: typeof q.header === "string" ? q.header : "",
        text,
        options,
        recommended: typeof q.recommended === "number" ? q.recommended : undefined,
        multi: q.multi === true,
      },
    ];
  });
}

/** Whether a stored batch is the one a tool call asked: each stored question is one it asked. Code
 * may have refused some, so the stored batch can be shorter. */
export function isTheBatchAsked(asked: QuestionDraft[], stored: { text: string }[]): boolean {
  const key = (t: string) => t.trim().toLowerCase().replace(/\s+/g, " ");
  const texts = new Set(asked.map((q) => key(q.text)));
  return stored.length > 0 && stored.every((q) => texts.has(key(q.text)));
}

export const ASK_TOOL = "giap-questions__ask";
