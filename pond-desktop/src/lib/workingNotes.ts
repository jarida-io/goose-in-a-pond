/** A streamed reply whose working notes can be moved out of its answer. */
export interface StepsCarrier {
  text: string;
  /** The model at work ("let me check the weather"), not the answer. Live turns only: history keeps none. */
  steps?: string[];
  /** Where `text` stood at the last tool call: what a mismatched `working_notes` takes instead. */
  stepsFrom?: number;
}

/**
 * Folds a `tool_call` frame into a reply. Its `working_notes` are the text streamed since the
 * previous tool call, so they normally end the answer; when they don't (the client filtered or
 * rewrote that text), everything since the last tool call is taken instead. Without notes (an
 * older pond) only the boundary moves, and the answer is left as it was.
 */
export function foldToolCall<T extends StepsCarrier>(reply: T, notes: string | undefined): T {
  const from = Math.min(reply.stepsFrom ?? 0, reply.text.length);
  if (!notes) return { ...reply, stepsFrom: reply.text.length };
  const suffixAt = reply.text.length - notes.length;
  const cut = reply.text.endsWith(notes) && suffixAt >= from ? suffixAt : from;
  const text = reply.text.slice(0, cut);
  const step = reply.text.slice(cut).trim();
  return {
    ...reply,
    text,
    steps: step ? [...(reply.steps ?? []), step] : reply.steps,
    stepsFrom: text.length,
  };
}
