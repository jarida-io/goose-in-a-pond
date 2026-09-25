// Voice transcript as a three-column grid (pond, orb channel, you); the side is the only attribution.
// Not TranscriptFeed: its own flex column and scrolling can't place a message in a grid column.

import { useEffect, useRef } from "react";
import type { ContextCard, TranscriptMessage } from "../../state/reducer";
import { ContextCard as ContextCardView } from "../../components/ContextCard";
import { ThinkingPlaceholder } from "../../components/ThinkingPlaceholder";

interface Props {
  messages: TranscriptMessage[];
  contextCards?: ContextCard[];
}

export function VoiceStream({ messages, contextCards = [] }: Props) {
  const scrollerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [messages, contextCards]);

  if (messages.length === 0) {
    return (
      <div className="vm-stream vm-stream--empty" ref={scrollerRef}>
        {/* An empty screen is an invitation, so it names the next move rather
            than reporting that a list is empty. */}
        <p className="vm-stream__empty">Say the wake word to begin.</p>
      </div>
    );
  }

  return (
    <div
      className="vm-stream"
      ref={scrollerRef}
      role="log"
      aria-live="polite"
      aria-label="Conversation"
    >
      {messages.map((msg, idx) => {
        // Same card-to-turn rule as TranscriptFeed.
        const nextMsg = messages[idx + 1];
        const inlineCards = msg.role === "agent"
          ? contextCards.filter((c) => {
              const afterThis = c.timestamp_ms >= msg.timestamp;
              const beforeNext = !nextMsg || c.timestamp_ms < nextMsg.timestamp;
              return afterThis && beforeNext;
            })
          : [];

        return (
          // Explicit row: auto-placement would put a reply and the next question on the same row.
          <div
            key={msg.id}
            className={`vm-said vm-said--${msg.role}`}
            style={{ gridRow: idx + 1 }}
          >
            <p className="vm-said__text">
              {msg.text || (msg.role === "agent" ? <ThinkingPlaceholder compact /> : "")}
            </p>

            {inlineCards.length > 0 && (
              <div className="vm-said__cards">
                {inlineCards.map((card) => (
                  <ContextCardView key={card.id} card={card} />
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
