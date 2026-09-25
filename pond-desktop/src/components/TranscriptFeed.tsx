import { useEffect, useRef } from "react";
import { Chip } from "@heroui/react";
import type { ContextCard, TranscriptMessage } from "../state/reducer";
import { ContextCard as ContextCardView } from "./ContextCard";
import { ThinkingPlaceholder } from "./ThinkingPlaceholder";

interface Props {
  messages: TranscriptMessage[];
  /** Context cards to render inline after the agent message they correspond to */
  contextCards?: ContextCard[];
  /** Legacy fixed max-height (ignored when fillHeight is true) */
  maxHeight?: string;
  /** Fill the parent flex container vertically (Voice Mode) */
  fillHeight?: boolean;
  compact?: boolean;
}

export function TranscriptFeed({
  messages,
  contextCards = [],
  maxHeight = "200px",
  fillHeight = false,
  compact = false,
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);

  // Scroll whichever of this feed or its ancestors scrolls (with `fillHeight`, the drawer),
  // stopping at `overflow-y: hidden` so a closed drawer never scrolls `.vm-root` instead.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;

    const scroller = ((): HTMLElement | null => {
      for (let node: HTMLElement | null = el; node; node = node.parentElement) {
        const overflowY = getComputedStyle(node).overflowY;
        if (overflowY === "hidden") return null;
        if (
          (overflowY === "auto" || overflowY === "scroll") &&
          node.scrollHeight > node.clientHeight
        ) {
          return node;
        }
      }
      return null;
    })();

    scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
  }, [messages, contextCards]);

  const rootStyle: React.CSSProperties = fillHeight
    ? { ...styles.root, flex: 1, maxHeight: "none" }
    : { ...styles.root, maxHeight };

  if (messages.length === 0) {
    return (
      <div ref={containerRef} style={rootStyle}>
        <p style={styles.empty}>
          {compact ? "Say something…" : "No messages yet. Start listening to begin."}
        </p>
      </div>
    );
  }

  return (
    <div ref={containerRef} style={rootStyle} role="log" aria-live="polite" aria-label="Conversation">
      {messages.map((msg, idx) => {
        // Cards whose timestamp falls after this message and before the next
        const nextMsg = messages[idx + 1];
        const inlineCards = msg.role === "agent"
          ? contextCards.filter((c) => {
              const afterThis  = c.timestamp_ms >= msg.timestamp;
              const beforeNext = !nextMsg || c.timestamp_ms < nextMsg.timestamp;
              return afterThis && beforeNext;
            })
          : [];

        return (
          <div key={msg.id}>
            <div
              style={{
                ...styles.message,
                ...(msg.role === "user" ? styles.userMsg : styles.agentMsg),
                ...(compact ? styles.compact : {}),
              }}
            >
              {!compact && (
                <Chip
                  size="sm"
                  variant={msg.role === "user" ? "soft" : "primary"}
                >
                  {msg.role === "user" ? "You" : "Pond"}
                </Chip>
              )}
              <p style={{ ...styles.text, fontSize: compact ? "11px" : "13px", padding: compact ? "4px 8px" : styles.text.padding }}>
                {msg.text || (msg.role === "agent" ? <ThinkingPlaceholder compact /> : "")}
              </p>
            </div>

            {/* Inline context cards after agent messages */}
            {inlineCards.length > 0 && (
              <div style={styles.cardsRow}>
                {inlineCards.map((card) => (
                  <div key={card.id} style={styles.cardWrapper}>
                    <ContextCardView card={card} />
                  </div>
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: {
    overflowY: "auto",
    display: "flex",
    flexDirection: "column",
    gap: "8px",
    width: "100%",
    padding: "4px 0",
  },
  empty: {
    textAlign: "center",
    fontSize: "var(--text-sm)",
    color: "var(--color-text-tertiary)",
    margin: "12px 0",
  },
  message: {
    display: "flex",
    flexDirection: "column",
    gap: "2px",
  },
  userMsg: {
    alignItems: "flex-end",
  },
  agentMsg: {
    alignItems: "flex-start",
  },
  compact: {
    gap: "1px",
  },
  text: {
    margin: 0,
    lineHeight: "1.5",
    color: "var(--color-text)",
    maxWidth: "88%",
    padding: "6px 10px",
    borderRadius: "var(--radius-lg)",
    background: "var(--color-border)",
    wordBreak: "break-word",
    userSelect: "text",
  },
  cardsRow: {
    display: "flex",
    flexDirection: "column",
    gap: "6px",
    paddingLeft: "4px",
    marginTop: "4px",
    marginBottom: "4px",
  },
  cardWrapper: {
    maxWidth: "min(340px, 90%)",
  },
};
