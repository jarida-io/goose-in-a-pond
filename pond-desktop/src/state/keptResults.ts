import type { SessionMessage } from "../api/types";
import { friendlyToolName } from "../components/toolNames";

/** A tool result the pond did not keep: history holds only a stub that names the sources. */
export interface KeptResult {
  callId: string;
  tool: string;
  stub: string;
}

const SOURCES = /\bSources:\s*(.+?)\.?\s*$/s;

/** A page read's stub ends with the address to read again. */
const PAGE = /read_web_page:\s*(https?:\/\/\S+)\s*$/;

function hostOf(address: string): string | null {
  try {
    return new URL(address).host.replace(/^www\./, "") || null;
  } catch {
    return null;
  }
}

// The row is stored as JSON inside `content`, so the marker is read from there first.
function readRow(content: string): Record<string, unknown> | null {
  try {
    const parsed: unknown = JSON.parse(content);
    return typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

/** The stub a tool row carries, or null when the row holds a whole result or is not a tool row. */
export function keptResultOf(m: SessionMessage): KeptResult | null {
  if (m.role !== "tool") return null;
  const row = readRow(m.content);
  if (row?.transient !== true && m.transient !== true) return null;
  const stub = (typeof row?.content === "string" ? row.content : m.content).trim();
  if (!stub) return null;
  return {
    callId: m.tool_call_id ?? (typeof row?.tool_call_id === "string" ? row.tool_call_id : ""),
    tool: typeof row?.tool === "string" ? row.tool : "",
    stub,
  };
}

/** One quiet sentence for a reloaded conversation, in place of a card there is nothing to fill. */
export function keptLine(kept: KeptResult): string {
  const label = kept.tool ? friendlyToolName(kept.tool) : "Tool";
  const page = PAGE.exec(kept.stub)?.[1];
  if (page) {
    const host = hostOf(page);
    const base = `${label}: its text isn't kept after the turn.`;
    return host ? `${base} Page: ${host}.` : base;
  }
  const sources = SOURCES.exec(kept.stub)?.[1]?.trim().slice(0, 200);
  const base = `${label}: results aren't kept after the turn.`;
  return sources ? `${base} Sources: ${sources}.` : base;
}
