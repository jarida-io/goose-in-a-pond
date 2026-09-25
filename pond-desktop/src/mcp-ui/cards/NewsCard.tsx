import { ChevronRight } from "lucide-react";
import { registerMcpCard, type McpCardProps } from "../registry";

interface NewsItem {
  headline: string;
  tag: string;
  timeAgo: string;
  source?: string;
}

// ---- Tag colors (design spec) ----

const TAG_STYLES: Record<string, { bg: string; color: string }> = {
  tech:       { bg: "#EDE9FE", color: "#7C3AED" },
  world:      { bg: "#FEF9C3", color: "var(--color-warning-fg)" },
  business:   { bg: "#DCFCE7", color: "var(--color-success-fg)" },
  health:     { bg: "#CFFAFE", color: "#0E7490" },
  science:    { bg: "#DBEAFE", color: "#1D4ED8" },
  sports:     { bg: "#FFE4E6", color: "var(--color-destructive-fg)" },
  // Common extras
  politics:   { bg: "#FEF3C7", color: "var(--color-warning-fg)" },
  finance:    { bg: "#DCFCE7", color: "var(--color-success-fg)" },
  economy:    { bg: "#DCFCE7", color: "var(--color-success-fg)" },
  education:  { bg: "#DBEAFE", color: "#1D4ED8" },
  culture:    { bg: "#EDE9FE", color: "#7C3AED" },
  entertainment: { bg: "#FCE7F3", color: "#BE185D" },
  climate:    { bg: "#CFFAFE", color: "#0E7490" },
  energy:     { bg: "#FEF9C3", color: "var(--color-warning-fg)" },
};

const FALLBACK_TAG_STYLE = { bg: "#F1F5F9", color: "#475569" };

function resolveTagStyle(tag: string): { bg: string; color: string } {
  return TAG_STYLES[tag.toLowerCase()] ?? FALLBACK_TAG_STYLE;
}

// ---- Speech-bubble icon ----

function HeadlineIcon() {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="#64748B"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flexShrink: 0 }}
    >
      <path d="M4 4h16v12H5.5L4 17.5z" />
    </svg>
  );
}

function NewsCard({ data, variant }: McpCardProps) {
  const items = (data.items ?? data.headlines ?? []) as NewsItem[];
  const isCompact = variant === "compact";
  const visibleItems = items.slice(0, isCompact ? 2 : 6);

  const dateLabel = (() => {
    if (data.date) return String(data.date);
    const now = new Date();
    const months = [
      "Jan", "Feb", "Mar", "Apr", "May", "Jun",
      "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    return `${months[now.getMonth()]} ${now.getDate()}`;
  })();

  return (
    <div>
      {/* ---- Header ---- */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "14px 16px 10px",
          gap: 8,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <HeadlineIcon />
          <span
            style={{
              fontSize: 13,
              fontWeight: 700,
              color: "#18181B",
              lineHeight: 1,
            }}
          >
            Headlines
          </span>
        </div>
        <span
          style={{
            fontSize: 11,
            fontWeight: 500,
            color: "#475569",
            background: "#F1F5F9",
            padding: "3px 10px",
            borderRadius: 6,
            whiteSpace: "nowrap",
          }}
        >
          {dateLabel}
        </span>
      </div>

      {/* ---- Divider ---- */}
      <div style={{ height: 1, background: "#F1F5F9" }} />

      {/* ---- News rows ---- */}
      <div style={{ display: "flex", flexDirection: "column", flex: 1 }}>
        {visibleItems.map((item, i) => {
          const tagStyle = resolveTagStyle(item.tag);
          const isLast = i === visibleItems.length - 1;

          return (
            <div
              key={i}
              style={{
                display: "flex",
                flexDirection: "row",
                alignItems: "flex-start",
                gap: 10,
                padding: "12px 16px",
                borderBottom: isLast ? "none" : "1px solid #F8FAFC",
                cursor: "pointer",
              }}
            >
              {/* Tag pill */}
              <span
                style={{
                  fontSize: 10,
                  fontWeight: 800,
                  padding: "3px 9px",
                  borderRadius: 999,
                  flexShrink: 0,
                  marginTop: 1,
                  letterSpacing: 0.2,
                  background: tagStyle.bg,
                  color: tagStyle.color,
                  whiteSpace: "nowrap",
                  lineHeight: 1.4,
                }}
              >
                {item.tag}
              </span>

              {/* Headline text */}
              <span
                style={{
                  flex: 1,
                  fontSize: 12,
                  fontWeight: 600,
                  color: "#18181B",
                  lineHeight: 1.45,
                }}
              >
                {item.headline}
              </span>

              {/* Time */}
              <span
                style={{
                  fontSize: 11,
                  color: "var(--color-text-tertiary)",
                  flexShrink: 0,
                  marginTop: 1,
                  fontWeight: 500,
                  whiteSpace: "nowrap",
                }}
              >
                {item.timeAgo}
              </span>

              {/* Chevron */}
              <ChevronRight
                size={14}
                style={{ color: "#CBD5E1", flexShrink: 0, marginTop: 1 }}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}

registerMcpCard({
  key: "news",
  label: "News",
  icon: "Newspaper",
  toolPattern: "news",
  component: NewsCard,
  mockTool: "giap-news__get_headlines",
  mockData: {
    date: "May 15",
    items: [
      { headline: "Rust 2024 edition ships with async trait stabilization", tag: "Tech", timeAgo: "2h" },
      { headline: "Global summit addresses climate resilience funding", tag: "World", timeAgo: "4h" },
      { headline: "NVIDIA reports record revenue on Jetson platform growth", tag: "Business", timeAgo: "6h" },
      { headline: "New CRISPR technique targets inherited cardiac conditions", tag: "Health", timeAgo: "8h" },
      { headline: "James Webb telescope captures high-res images of Europa", tag: "Science", timeAgo: "10h" },
      { headline: "Champions League final draw reveals surprise matchups", tag: "Sports", timeAgo: "12h" },
    ],
  },
});
