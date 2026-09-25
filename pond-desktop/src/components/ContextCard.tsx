import { Card, Chip } from "@heroui/react";
import { findCardRenderer, findCardByHint } from "../mcp-ui";
import type { ContextCard as ContextCardType } from "../state/reducer";

interface Props {
  card: ContextCardType;
}

export function ContextCard({ card }: Props) {
  const { tool, data } = card;
  const toolName = tool.includes("__") ? tool.split("__")[1] : tool;

  return (
    <Card role="article" aria-label={`Tool result: ${toolName}`}>
      <div style={styles.header}>
        <Chip size="sm" variant="primary">{formatToolName(toolName)}</Chip>
      </div>
      <div style={styles.body}>
        <ToolContent toolName={toolName} data={data} renderHint={card.renderHint} />
      </div>
    </Card>
  );
}

function ToolContent({ toolName, data, renderHint }: { toolName: string; data: Record<string, unknown> | null; renderHint?: string }) {
  const safe = data ?? {};

  // 1. Try explicit MCP-APP hint (highest priority)
  if (renderHint) {
    const reg = findCardByHint(renderHint);
    if (reg) {
      const Renderer = reg.component;
      return <Renderer data={safe} toolName={toolName} variant="compact" />;
    }
  }

  // 2. Try MCP-UI registry pattern match
  const reg = findCardRenderer(toolName);
  if (reg) {
    const Renderer = reg.component;
    return <Renderer data={safe} toolName={toolName} variant="compact" />;
  }

  // 3. Fallbacks by tool name
  if (toolName === "list_registered_devices") return <DevicesContent data={safe} />;
  if (toolName === "recall_memories" || toolName === "save_memory") return <MemoryContent data={safe} />;
  if (toolName === "list_schedules") return <SchedulesContent data={safe} />;
  return <GenericContent data={safe} />;
}

function WeatherContent({ data }: { data: Record<string, unknown> }) {
  const temp = data?.temperature ?? data?.temp;
  const desc = data?.description ?? data?.condition ?? data?.weather;
  const location = data?.location ?? data?.city;
  return (
    <div style={styles.weatherRow}>
      <span style={styles.weatherTemp}>{temp !== undefined ? `${temp}°` : "—"}</span>
      <div>
        {desc != null && <p style={styles.value}>{String(desc)}</p>}
        {location != null && <p style={styles.hint}>{String(location)}</p>}
      </div>
    </div>
  );
}

function DevicesContent({ data }: { data: Record<string, unknown> }) {
  const devices = Array.isArray(data?.devices) ? data.devices : Array.isArray(data) ? data : [];
  if (devices.length === 0) return <p style={styles.hint}>No devices found.</p>;
  return (
    <ul style={styles.list}>
      {devices.slice(0, 5).map((d: unknown, i) => {
        const dev = d as Record<string, unknown>;
        return (
          <li key={i} style={styles.listItem}>
            <span style={{ ...styles.dot, background: dev.is_online ? "var(--color-success)" : "var(--color-neutral)" }} />
            <span style={styles.value}>{String(dev.name ?? "Device")}</span>
            {dev.room != null && <span style={styles.hint}>{String(dev.room)}</span>}
          </li>
        );
      })}
    </ul>
  );
}

function MemoryContent({ data }: { data: Record<string, unknown> }) {
  const content = data?.content ?? data?.text ?? data?.memory;
  return content ? (
    <p style={{ ...styles.value, userSelect: "text" as const }}>{String(content)}</p>
  ) : (
    <GenericContent data={data} />
  );
}

function SchedulesContent({ data }: { data: Record<string, unknown> }) {
  const schedules = Array.isArray(data?.schedules) ? data.schedules : [];
  if (schedules.length === 0) return <p style={styles.hint}>No schedules.</p>;
  return (
    <ul style={styles.list}>
      {schedules.slice(0, 4).map((s: unknown, i) => {
        const sched = s as Record<string, unknown>;
        return (
          <li key={i} style={styles.listItem}>
            <span style={styles.value}>{String(sched.name ?? "Schedule")}</span>
            <span style={styles.hint}>{String(sched.cron ?? "")}</span>
          </li>
        );
      })}
    </ul>
  );
}

function GenericContent({ data }: { data: Record<string, unknown> }) {
  const text = typeof data === "string"
    ? data
    : JSON.stringify(data ?? {}, null, 2);
  return (
    <pre style={styles.pre}>{text}</pre>
  );
}

function formatToolName(name: string): string {
  return name
    .replace(/_/g, " ")
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

const styles: Record<string, React.CSSProperties> = {
  card: {
    background: "var(--color-bg)",
    border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-lg)",
    overflow: "hidden",
    fontSize: "var(--text-sm)",
  },
  header: {
    padding: "6px 10px",
    background: "var(--color-accent-subtle)",
    borderBottom: "1px solid var(--color-border)",
  },
  toolName: {
    fontFamily: "var(--font-display)",
    fontWeight: 600,
    fontSize: "var(--text-xs)",
    color: "var(--color-accent)",
    textTransform: "uppercase",
    letterSpacing: "0.05em",
  },
  body: {
    padding: "8px 10px",
  },
  weatherRow: {
    display: "flex",
    alignItems: "center",
    gap: "10px",
  },
  weatherTemp: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-xl)",
    color: "var(--color-text)",
    lineHeight: "1",
  },
  list: {
    listStyle: "none",
    margin: 0,
    padding: 0,
    display: "flex",
    flexDirection: "column",
    gap: "4px",
  },
  listItem: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
  },
  dot: {
    width: "6px",
    height: "6px",
    borderRadius: "50%",
    flexShrink: 0,
  },
  value: {
    margin: 0,
    color: "var(--color-text)",
    fontSize: "var(--text-sm)",
  },
  hint: {
    margin: 0,
    color: "var(--color-text-tertiary)",
    fontSize: "var(--text-xs)",
  },
  pre: {
    margin: 0,
    fontSize: "var(--text-xs)",
    fontFamily: "var(--font-mono)",
    color: "var(--color-text-secondary)",
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
    userSelect: "text",
    maxHeight: "120px",
    overflowY: "auto",
  },
};
