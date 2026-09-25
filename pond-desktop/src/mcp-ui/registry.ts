import type { ComponentType } from "react";

export interface McpCardProps {
  data: Record<string, unknown>;
  toolName: string;
  onClose?: () => void;
  variant?: "compact" | "normal" | "large";
  /**
   * A message, not a tool call, so follow-ups stay audited and policy-checked; a direct call would need
   * `DIRECT_DISPATCH_ALLOWLIST`, opening the tool to every paired client and MCP App iframe. Unset on Canvas.
   */
  onAction?: (prompt: string) => void;
}

export interface McpCardRegistration {
  /** Unique key for this card type */
  key: string;
  label: string;
  /** Lucide icon name for chrome bar */
  icon: string;
  /** Matches tool names; a string is a substring match. */
  toolPattern: string | RegExp;
  component: ComponentType<McpCardProps>;
  parseResult?: (raw: string) => Record<string, unknown>;
  /** Mock data for demo/suggest mode */
  mockData?: Record<string, unknown>;
  /** Mock tool name for demo triggers */
  mockTool?: string;
}

const _registry: McpCardRegistration[] = [];

export function registerMcpCard(entry: McpCardRegistration): void {
  const idx = _registry.findIndex((r) => r.key === entry.key);
  if (idx >= 0) _registry[idx] = entry;
  else _registry.push(entry);
}

export function findCardRenderer(toolName: string): McpCardRegistration | null {
  const bare = toolName.includes("__") ? toolName.split("__")[1]! : toolName;
  for (const reg of _registry) {
    if (typeof reg.toolPattern === "string") {
      if (bare.includes(reg.toolPattern) || toolName.includes(reg.toolPattern)) return reg;
    } else {
      if (reg.toolPattern.test(bare) || reg.toolPattern.test(toolName)) return reg;
    }
  }
  return null;
}

/** Find a card renderer by explicit hint key (exact match on registration key). */
export function findCardByHint(hint: string): McpCardRegistration | null {
  return _registry.find((r) => r.key === hint) ?? null;
}

export function getAllRegistrations(): readonly McpCardRegistration[] {
  return _registry;
}
