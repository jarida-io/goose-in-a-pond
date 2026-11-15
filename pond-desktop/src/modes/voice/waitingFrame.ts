import { readWaitReason } from "../../api/types";

/** What a chat frame does to the voice label: a message to show, null to clear it, undefined to leave it. */
export function waitingLabel(ev: Record<string, unknown>): string | null | undefined {
  if (ev.type === "status") {
    return readWaitReason(ev.waiting) && typeof ev.content === "string" && ev.content ? ev.content : null;
  }
  if (ev.type === "text" || ev.type === "thinking" || ev.type === "tool_call" || ev.type === "error" || ev.error || ev.done === true) {
    return null;
  }
  return undefined;
}
