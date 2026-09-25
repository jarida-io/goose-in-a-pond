import { useState } from "react";
import { ChevronDown, ChevronRight, Wrench } from "lucide-react";
import type { ContextCard as ContextCardType } from "../state/reducer";
import { findCardByHint, findCardRenderer } from "../mcp-ui";

interface Props {
  card: ContextCardType;
  /** Sends a card's follow-up message as the user; without it, cards show static labels. */
  onAction?: (prompt: string) => void;
}

/** Collapsible tool-result chip in a chat bubble: the matching MCP-UI card, else truncated text. */
export function ToolCallChip({ card, onAction }: Props) {
  const [expanded, setExpanded] = useState(false);
  const name = friendlyToolName(card.tool);
  const result = extractResultText(card.data);

  // The server's `renderHint` (from `extract_ui_hint`) wins over a tool-name guess.
  const registration =
    (card.renderHint ? findCardByHint(card.renderHint) : null) ?? findCardRenderer(card.tool);
  // No hint until `tool_result`; before that a card would render empty, so show the text.
  const Renderer = card.renderHint && registration ? registration.component : null;
  const hasBody = Boolean(Renderer) || Boolean(result);

  return (
    <div
      className={`tool-call tool-call--chip${expanded ? " tool-call--expanded" : ""}`}
      onClick={() => setExpanded((v) => !v)}
      role="button"
      aria-expanded={expanded}
      aria-label={`${name} — click to ${expanded ? "collapse" : "expand"}`}
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          setExpanded((v) => !v);
        }
      }}
    >
      <Wrench size={11} aria-hidden />
      <span className="tool-call__name">{name}</span>
      {hasBody
        ? (expanded
            ? <ChevronDown size={10} aria-hidden />
            : <ChevronRight size={10} aria-hidden />)
        : null}

      {expanded && Renderer && (
        <div
          className="tool-call-card"
          role="region"
          aria-label={`${name} result`}
          // Keep clicks off the chip's toggle, or any click inside the card would collapse it.
          onClick={(e) => e.stopPropagation()}
          onKeyDown={(e) => e.stopPropagation()}
        >
          <Renderer
            data={card.data ?? {}}
            toolName={card.tool}
            variant="compact"
            onAction={onAction}
          />
        </div>
      )}

      {expanded && !Renderer && result && (
        <div className="tool-call-result" role="region" aria-label={`${name} result`}>
          {result}
        </div>
      )}
    </div>
  );
}

// ── Friendly name mapping ────────────────────────────────────────────────────

const NAME_MAP: Record<string, string> = {
  // weather
  get_current_weather: "Weather",
  // wikipedia
  get_wikipedia_article: "Wikipedia",
  // computation
  compute_answer: "Computed",
  // schedule
  list_schedules: "Schedules",
  create_schedule: "Create Schedule",
  delete_schedule: "Delete Schedule",
  pause_schedule: "Pause Schedule",
  resume_schedule: "Resume Schedule",
  run_schedule_now: "Run Schedule",
  // memory
  save_memory: "Save Memory",
  recall_memories: "Recall Memory",
  forget_memory: "Forget Memory",
  // system
  get_system_info: "System Info",
  get_current_time: "Current Time",
  send_notification: "Notification",
  // devices
  list_registered_devices: "Devices",
  get_current_profile: "Profile",
  get_model_config: "Model Config",
  // music
  play: "Play Music",
  status: "Now Playing",
  control: "Playback Control",
};

export function friendlyToolName(raw: string): string {
  // Strip namespace prefix: "giap-knowledge__get_wikipedia_article" → "get_wikipedia_article"
  const bare = raw.includes("__") ? raw.split("__").pop()! : raw;
  if (NAME_MAP[bare]) return NAME_MAP[bare];
  // Fallback: title-case the snake_case function name
  return bare
    .replace(/_/g, " ")
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

// ── Result text extraction ───────────────────────────────────────────────────

/** A readable string from whatever shape `card.data` has, or null. */
function extractResultText(data: Record<string, unknown> | null): string | null {
  if (!data) return null;

  // tool_result shape: { result: "..." }
  const result = data.result;
  if (typeof result === "string" && result.trim()) {
    return truncate(result, 300);
  }

  // weather shape: { temperature, description, location }
  if (data.temperature !== undefined || data.temp !== undefined) {
    const temp = data.temperature ?? data.temp;
    const desc = data.description ?? data.condition ?? data.weather ?? "";
    const loc  = data.location ?? data.city ?? "";
    const parts = [`${temp}°`];
    if (desc) parts.push(String(desc));
    if (loc)  parts.push(String(loc));
    return parts.join(" · ");
  }

  // Wikipedia / long text
  if (typeof data.text === "string" && data.text.trim()) {
    return truncate(data.text, 300);
  }
  if (typeof data.content === "string" && data.content.trim()) {
    return truncate(data.content, 300);
  }
  if (typeof data.summary === "string" && data.summary.trim()) {
    return truncate(data.summary, 300);
  }

  // Generic: JSON fallback
  const json = JSON.stringify(data);
  if (json !== "{}") return truncate(json, 300);

  return null;
}

function truncate(s: string, max: number): string {
  const trimmed = s.trim();
  return trimmed.length <= max ? trimmed : trimmed.slice(0, max) + "…";
}
