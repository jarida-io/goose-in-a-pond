import type { SessionMessage } from "../api/types";

export const SEARCH_TOOL = "giap-knowledge__search_web";
export const SEARCH_STUB =
  "Web search results are not kept after the turn. Sources: kenyapower.co.ke, nation.africa.";

/** A tool row as history returns it: the stored row is JSON inside `content`, `transient` among its keys. */
export function keptToolRow(over: Partial<SessionMessage> = {}, row: Record<string, unknown> = {}): SessionMessage {
  return {
    id: "tool-1",
    session_id: "sess-1",
    role: "tool",
    created_at: "",
    tool_call_id: "call-1",
    content: JSON.stringify({
      tool_call_id: "call-1",
      tool: SEARCH_TOOL,
      content: SEARCH_STUB,
      arguments: '{"question":"when does the power come back"}',
      transient: true,
      ...row,
    }),
    ...over,
  };
}

export function assistantRow(over: Partial<SessionMessage> = {}): SessionMessage {
  return {
    id: "asst-1",
    session_id: "sess-1",
    role: "assistant",
    content: "Power should be back by 6 pm.",
    created_at: "",
    tool_calls: [{ id: "call-1", name: SEARCH_TOOL, arguments: '{"question":"q"}' }],
    ...over,
  };
}

export function userRow(over: Partial<SessionMessage> = {}): SessionMessage {
  return {
    id: "user-1",
    session_id: "sess-1",
    role: "user",
    content: "when does the power come back?",
    created_at: "",
    ...over,
  };
}
