import React from "react";
import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";

// ── Mock data ──────────────────────────────────────────────────

interface NewsItem {
  headline: string;
  tag: string;
  tagColor: string;
  tagBg: string;
  ago: string;
}

const NEWS_ITEMS: NewsItem[] = [
  {
    headline: "Rust 2024 edition ships with async trait stabilization",
    tag: "Tech",     tagColor: "#7C3AED", tagBg: "#EDE9FE", ago: "2h",
  },
  {
    headline: "Global summit addresses climate resilience funding",
    tag: "World",    tagColor: "var(--color-warning-fg)", tagBg: "#FEF9C3", ago: "4h",
  },
  {
    headline: "NVIDIA reports record revenue on Jetson platform growth",
    tag: "Business", tagColor: "var(--color-success-fg)", tagBg: "#DCFCE7", ago: "6h",
  },
  {
    headline: "New CRISPR technique targets inherited cardiac conditions",
    tag: "Health",   tagColor: "#0E7490", tagBg: "#CFFAFE", ago: "8h",
  },
];

/** News card for giap-news.get_top_stories; mock data for now. */
export function NewsCard(): React.ReactElement {
  return (
    <div className="mc">
      <div className="mc-header">
        <div className="mc-header-left">
          {/* Chat/speech-bubble icon repurposed as "headlines" */}
          <HubIco d="M4 4h16v12H5.5L4 17.5z" size={15} color="#64748B" sw={1.75} />
          <span className="mc-title">Headlines</span>
        </div>
        <span className="mc-chip mc-chip--slate">May 15</span>
      </div>
      <div className="mc-divider" />
      <div style={{ flex: 1, display: "flex", flexDirection: "column" }}>
        {NEWS_ITEMS.map((it, i) => (
          <div
            key={i}
            style={{
              display: "flex",
              alignItems: "flex-start",
              gap: 10,
              padding: "12px 16px",
              borderBottom: i < NEWS_ITEMS.length - 1 ? "1px solid #F8FAFC" : "none",
              cursor: "pointer",
            }}
            role="button"
            tabIndex={0}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") e.preventDefault();
            }}
            aria-label={`Read: ${it.headline}`}
          >
            <span
              style={{
                fontSize: 10,
                fontWeight: 800,
                padding: "3px 9px",
                borderRadius: 999,
                background: it.tagBg,
                color: it.tagColor,
                flexShrink: 0,
                marginTop: 1,
                letterSpacing: 0.2,
                whiteSpace: "nowrap",
              }}
            >
              {it.tag}
            </span>
            <span
              style={{
                flex: 1,
                fontSize: 12,
                fontWeight: 600,
                color: "#18181B",
                lineHeight: 1.45,
              }}
            >
              {it.headline}
            </span>
            <span
              style={{
                fontSize: 11,
                color: "var(--color-text-tertiary)",
                flexShrink: 0,
                marginTop: 1,
                fontWeight: 500,
              }}
            >
              {it.ago}
            </span>
            <HubIco d={HP_PATHS.chevR} size={14} color="#CBD5E1" sw={2} />
          </div>
        ))}
      </div>
    </div>
  );
}
