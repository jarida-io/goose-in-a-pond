import { useState, useEffect, useRef } from "react";
import { Card, CardContent, Button, Chip } from "@heroui/react";
import {
  BrainCircuit,
  Shield,
  Heart,
  Wrench,
  FolderOpen,
  BookOpen,
  Clock,
  Star,
  Trash2,
  Plus,
  RefreshCw,
  X,
  Search,
  Pencil,
  Save,
  GitMerge,
  MessageSquare,
  Layers,
  Radio,
  Sparkles,
  Brain,
  Radar,
  AlertTriangle,
  Check,
  Minus,
  Repeat,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { PageHeader, useConfirm, SkeletonList } from "../components/shared";
import type {
  ContextCorpus,
  ContextCorpusCoverage,
  ContextIndexHealth,
  ExtractionStatus,
  MemoryFragment,
  MemorySegment,
  MemoryTier,
  Settings,
} from "../api/types";

// ── Segment metadata ──────────────────────────────────────────

type SegmentKey = MemorySegment | "all";
type TierKey = MemoryTier | "all";
type SourceKey = "auto" | "mcp" | "chat" | "all";

const SEGMENTS: Record<
  MemorySegment,
  { label: string; icon: React.ReactNode; cssClass: string; importanceDefault: number }
> = {
  identity:     { label: "Identity",     icon: <Shield size={13} strokeWidth={1.8} />,    cssClass: "identity",     importanceDefault: 0.8 },
  preference:   { label: "Preference",   icon: <Heart size={13} strokeWidth={1.8} />,     cssClass: "preference",   importanceDefault: 0.7 },
  correction:   { label: "Correction",   icon: <Wrench size={13} strokeWidth={1.8} />,    cssClass: "correction",   importanceDefault: 0.9 },
  relationship: { label: "Relationship", icon: <Heart size={13} strokeWidth={1.8} />,     cssClass: "relationship", importanceDefault: 0.7 },
  project:      { label: "Project",      icon: <FolderOpen size={13} strokeWidth={1.8} />,cssClass: "project",      importanceDefault: 0.6 },
  routine:      { label: "Routine",      icon: <Repeat size={13} strokeWidth={1.8} />,    cssClass: "routine",      importanceDefault: 0.65 },
  knowledge:    { label: "Knowledge",    icon: <BookOpen size={13} strokeWidth={1.8} />,  cssClass: "knowledge",    importanceDefault: 0.5 },
  context:      { label: "Context",      icon: <Clock size={13} strokeWidth={1.8} />,     cssClass: "context",      importanceDefault: 0.3 },
};

const SEGMENT_IMPORT_FILL: Record<MemorySegment, string> = {
  identity:     "var(--mem-identity)",
  preference:   "var(--mem-preference)",
  correction:   "var(--mem-correction)",
  relationship: "var(--mem-relationship)",
  project:      "var(--mem-project)",
  routine:      "var(--mem-routine)",
  knowledge:    "var(--mem-knowledge)",
  context:      "var(--mem-context)",
};

const SEGMENT_ORDER: MemorySegment[] = [
  "identity", "preference", "correction", "relationship", "project", "knowledge", "context",
];

// ── Helpers ───────────────────────────────────────────────────

function relativeTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  if (days < 30) return `${days}d ago`;
  return new Date(iso).toLocaleDateString();
}

function sourceLabel(source?: string): string {
  if (!source) return "";
  if (source === "extraction") return "auto";
  if (source === "mcp_tool") return "mcp";
  if (source === "chat") return "chat";
  if (source === "note") return "note";
  return source;
}

function normalizedSource(source?: string): SourceKey {
  const label = sourceLabel(source);
  if (label === "auto") return "auto";
  if (label === "mcp") return "mcp";
  if (label === "chat") return "chat";
  return "auto";
}

// ── Stats bar ─────────────────────────────────────────────────

function MemStatsBar({ items }: { items: MemoryFragment[] }) {
  const counts = SEGMENT_ORDER.reduce<Partial<Record<MemorySegment, number>>>((acc, seg) => {
    const n = items.filter((m) => m.segment === seg).length;
    if (n > 0) acc[seg] = n;
    return acc;
  }, {});

  if (Object.keys(counts).length === 0) return null;

  return (
    <div className="mem-stats">
      {SEGMENT_ORDER.filter((s) => counts[s]).map((seg) => (
        <span
          key={seg}
          className={`mem-stats__chip mem-seg-badge--${seg}`}
          title={SEGMENTS[seg].label}
        >
          <span className="mem-stats__dot" style={{ background: SEGMENT_IMPORT_FILL[seg] }} />
          {counts[seg]} {SEGMENTS[seg].label}
        </span>
      ))}
    </div>
  );
}

// ── Filter row ────────────────────────────────────────────────

function MemFilterRow({
  activeSegment,
  activeTier,
  activeSource,
  segCounts,
  tierCounts,
  sourceCounts,
  total,
  onSegmentChange,
  onTierChange,
  onSourceChange,
}: {
  activeSegment: SegmentKey;
  activeTier: TierKey;
  activeSource: SourceKey;
  segCounts: Partial<Record<MemorySegment, number>>;
  tierCounts: Partial<Record<MemoryTier, number>>;
  sourceCounts: Partial<Record<SourceKey, number>>;
  total: number;
  onSegmentChange: (seg: SegmentKey) => void;
  onTierChange: (tier: TierKey) => void;
  onSourceChange: (source: SourceKey) => void;
}) {
  const tierLabels: Record<MemoryTier, string> = {
    short: "Short",
    long: "Long",
    permanent: "Permanent",
  };

  const sourceLabels: Record<SourceKey, string> = {
    all: "All Sources",
    auto: "Auto",
    mcp: "MCP",
    chat: "Chat",
  };

  const hasTiers = Object.keys(tierCounts).length > 0;
  const hasSources = Object.keys(sourceCounts).filter((k) => k !== "all").length > 0;

  return (
    <div className="mem-filter-col">
      {/* Segment filter */}
      <div className="mem-filter">
        <button
          className={`mem-filter__btn${activeSegment === "all" ? " is-active" : ""}`}
          onClick={() => onSegmentChange("all")}
        >
          All
          <span className="mem-filter__count">{total}</span>
        </button>
        {SEGMENT_ORDER.filter((s) => segCounts[s]).map((seg) => (
          <button
            key={seg}
            className={`mem-filter__btn${activeSegment === seg ? " is-active" : ""}`}
            onClick={() => onSegmentChange(seg)}
          >
            {SEGMENTS[seg].label}
            <span className="mem-filter__count">{segCounts[seg]}</span>
          </button>
        ))}
      </div>

      {/* Tier + Source secondary filters */}
      {(hasTiers || hasSources) && (
        <div className="mem-filter__sub-row">
          {hasTiers && (
            <div className="mem-filter__sub-group">
              <span className="mem-filter__sub-label">
                <Layers size={10} strokeWidth={2} />
                Tier
              </span>
              {(["all", "short", "long", "permanent"] as const).map((tier) => {
                const count = tier === "all" ? total : tierCounts[tier];
                if (tier !== "all" && !count) return null;
                return (
                  <button
                    key={tier}
                    className={`mem-filter__btn mem-filter__btn--sm${activeTier === tier ? " is-active" : ""}`}
                    onClick={() => onTierChange(tier)}
                  >
                    {tier === "all" ? "Any" : tierLabels[tier]}
                    {tier !== "all" && count !== undefined && (
                      <span className="mem-filter__count">{count}</span>
                    )}
                  </button>
                );
              })}
            </div>
          )}

          {hasTiers && hasSources && (
            <span className="mem-filter__divider" />
          )}

          {hasSources && (
            <div className="mem-filter__sub-group">
              <span className="mem-filter__sub-label">
                <Radio size={10} strokeWidth={2} />
                Source
              </span>
              {(["all", "auto", "mcp", "chat"] as const).map((src) => {
                const count = src === "all" ? total : sourceCounts[src];
                if (src !== "all" && !count) return null;
                return (
                  <button
                    key={src}
                    className={`mem-filter__btn mem-filter__btn--sm${activeSource === src ? " is-active" : ""}`}
                    onClick={() => onSourceChange(src)}
                  >
                    {sourceLabels[src]}
                    {src !== "all" && count !== undefined && (
                      <span className="mem-filter__count">{count}</span>
                    )}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ── Memory row (design-spec layout) ──────────────────────────

function MemRow({
  mem,
  onDelete,
  onUpdate,
}: {
  mem: MemoryFragment;
  onDelete: (id: string) => void;
  onUpdate: (id: string, newContent: string) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [editContent, setEditContent] = useState(mem.content);
  const [saving, setSaving] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const seg = mem.segment;
  const segMeta = seg ? SEGMENTS[seg] : null;
  const segClass = seg ? seg : "none";
  const importanceFill = seg ? SEGMENT_IMPORT_FILL[seg] : "var(--grey-300)";
  const importance = mem.importance ?? (seg ? segMeta?.importanceDefault : undefined);

  function startEdit() {
    setEditContent(mem.content);
    setEditing(true);
    setTimeout(() => textareaRef.current?.focus(), 0);
  }

  function cancelEdit() {
    setEditing(false);
    setEditContent(mem.content);
  }

  async function handleSave() {
    const trimmed = editContent.trim();
    if (!trimmed || trimmed === mem.content) {
      cancelEdit();
      return;
    }
    setSaving(true);
    try {
      await onUpdate(mem.id, trimmed);
      setEditing(false);
    } finally {
      setSaving(false);
    }
  }

  if (editing) {
    // Editing state — spans full row width
    return (
      <div className="mem-row mem-row--editing">
        <div className="mem-row__edit-body">
          <textarea
            ref={textareaRef}
            className="mem-row__edit-textarea"
            value={editContent}
            onChange={(e) => setEditContent(e.target.value)}
            disabled={saving}
            rows={3}
            aria-label="Edit memory content"
          />
          <div className="mem-row__edit-actions">
            <button
              className="mem-row__save-btn"
              onClick={handleSave}
              disabled={saving || !editContent.trim()}
              aria-label="Save edit"
            >
              <Save size={12} strokeWidth={2} />
              {saving ? "Saving…" : "Save"}
            </button>
            <button
              className="mem-row__cancel-btn"
              onClick={cancelEdit}
              disabled={saving}
              aria-label="Cancel edit"
            >
              Cancel
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="mem-row">
      {/* Bullet: segment icon bubble */}
      <div className={`mem-row__bullet mem-card__seg-icon--${segClass}`}>
        {segMeta ? segMeta.icon : <Sparkles size={12} strokeWidth={1.8} />}
      </div>

      {/* Main text + meta */}
      <div className="mem-row__text">
        <div>{mem.content}</div>
        {/* Badges + importance line */}
        <div className="mem-row__meta">
          {seg && (
            <span className={`mem-seg-badge mem-seg-badge--${segClass}`}>
              {segMeta?.icon}
              {segMeta?.label ?? seg}
            </span>
          )}
          {mem.tier && (
            <span className={`mem-tier-badge mem-tier-badge--${mem.tier}`}>
              {mem.tier}
            </span>
          )}
          {mem.source && (
            <span className="mem-source-badge">{sourceLabel(mem.source)}</span>
          )}
          {importance !== undefined && (
            <span className="mem-card__meta-item mem-card__meta-item--importance">
              <span className="mem-importance__track">
                <span
                  className="mem-importance__fill"
                  style={{
                    width: `${Math.round(importance * 100)}%`,
                    background: importanceFill,
                  }}
                />
              </span>
              <span className="mem-importance__label">{Math.round(importance * 10) / 10}</span>
            </span>
          )}
          {mem.access_count > 0 && (
            <span className="mem-card__meta-item">
              <Star size={10} strokeWidth={1.8} />
              {mem.access_count}x
            </span>
          )}
          {mem.tags && mem.tags.length > 0 && (
            <span className="mem-card__meta-item">
              {mem.tags.slice(0, 3).join(", ")}
            </span>
          )}
        </div>
      </div>

      {/* Date */}
      <div className="mem-row__date">
        <Clock size={10} strokeWidth={1.8} style={{ display: "inline", marginRight: 3, verticalAlign: "middle" }} />
        {relativeTime(mem.created_at)}
      </div>

      {/* Actions */}
      <div className="mem-row__actions-group">
        <button
          className="mem-card__delete"
          onClick={startEdit}
          aria-label="Edit memory"
          title="Edit memory"
        >
          <Pencil size={12} strokeWidth={1.8} />
        </button>
        <button
          className="mem-card__delete"
          onClick={() => onDelete(mem.id)}
          aria-label="Delete memory"
          title="Delete memory"
        >
          <Trash2 size={12} strokeWidth={1.8} />
        </button>
      </div>
    </div>
  );
}

// ── Add memory modal ──────────────────────────────────────────

function AddMemoryModal({
  onAdd,
  onClose,
}: {
  onAdd: (
    content: string,
    segment?: MemorySegment,
    importance?: number,
    tier?: MemoryTier,
  ) => Promise<void>;
  onClose: () => void;
}) {
  const [content, setContent] = useState("");
  const [saving, setSaving] = useState(false);
  const [segment, setSegment] = useState<MemorySegment | "">("");
  const [importance, setImportance] = useState(0.7);
  const [tier, setTier] = useState<MemoryTier | "">("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [onClose]);

  async function handleSave() {
    const trimmed = content.trim();
    if (!trimmed) return;
    setSaving(true);
    try {
      await onAdd(
        trimmed,
        segment !== "" ? (segment as MemorySegment) : undefined,
        segment !== "" ? importance : undefined,
        tier !== "" ? (tier as MemoryTier) : undefined,
      );
      onClose();
    } finally {
      setSaving(false);
    }
  }

  const segmentColor = segment !== "" ? SEGMENT_IMPORT_FILL[segment as MemorySegment] : undefined;

  return (
    <div className="mem-modal__overlay" onClick={onClose}>
      <div className="mem-modal__dialog" onClick={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label="Add memory">
        {/* Header */}
        <div className="mem-modal__header">
          <div className="mem-modal__header-left">
            <div className="mem-modal__header-icon">
              <BrainCircuit size={16} strokeWidth={1.8} />
            </div>
            <h2 className="mem-modal__title">Add Memory</h2>
          </div>
          <button className="mem-modal__close" onClick={onClose} aria-label="Close">
            <X size={16} strokeWidth={1.8} />
          </button>
        </div>

        <div className="mem-modal__divider" />

        {/* Body */}
        <div className="mem-modal__body">
          {/* Content textarea */}
          <div className="mem-modal__field">
            <label className="mem-modal__label" htmlFor="mem-content">Content</label>
            <textarea
              id="mem-content"
              ref={textareaRef}
              className="mem-modal__textarea"
              placeholder="What should the assistant remember?"
              value={content}
              onChange={(e) => setContent(e.target.value)}
              disabled={saving}
              rows={4}
              aria-label="Memory content"
            />
          </div>

          {/* Segment dropdown */}
          <div className="mem-modal__field">
            <label className="mem-modal__label" htmlFor="mem-segment">Segment</label>
            <div className="mem-modal__select-wrap">
              {segment !== "" && (
                <span className="mem-modal__seg-dot" style={{ background: segmentColor }} />
              )}
              <select
                id="mem-segment"
                className="mem-modal__select"
                value={segment}
                onChange={(e) => {
                  const val = e.target.value as MemorySegment | "";
                  setSegment(val);
                  if (val !== "") {
                    setImportance(SEGMENTS[val as MemorySegment].importanceDefault);
                  }
                }}
                disabled={saving}
              >
                <option value="">Auto-classify</option>
                {SEGMENT_ORDER.map((s) => (
                  <option key={s} value={s}>{SEGMENTS[s].label}</option>
                ))}
              </select>
            </div>
          </div>

          {/* Importance slider */}
          <div className="mem-modal__field">
            <div className="mem-modal__importance-row">
              <label className="mem-modal__label" htmlFor="mem-importance">Importance</label>
              <span className="mem-modal__importance-val">
                {segment !== "" ? importance.toFixed(2) : "–"}
              </span>
            </div>
            <input
              id="mem-importance"
              type="range"
              className="mem-modal__range"
              style={{
                accentColor: segmentColor ?? "var(--color-accent)",
                opacity: segment === "" ? 0.4 : 1,
                cursor: segment === "" ? "not-allowed" : "pointer",
              }}
              min={0}
              max={1}
              step={0.05}
              value={importance}
              onChange={(e) => setImportance(parseFloat(e.target.value))}
              disabled={saving || segment === ""}
              title={segment === "" ? "Select a segment first to set importance" : `Importance: ${importance}`}
            />
            <div className="mem-modal__range-labels">
              <span>Low</span>
              <span>High</span>
            </div>
          </div>

          {/* Tier selector */}
          <div className="mem-modal__field">
            <label className="mem-modal__label">Tier</label>
            <div className="mem-modal__tier-row">
              {(["", "short", "long", "permanent"] as const).map((t) => {
                const labels: Record<string, string> = {
                  "": "Auto",
                  short: "Short",
                  long: "Long",
                  permanent: "Permanent",
                };
                const isActive = tier === t;
                return (
                  <button
                    key={t}
                    className={`mem-modal__tier-btn${isActive ? " is-active" : ""}`}
                    onClick={() => setTier(t as MemoryTier | "")}
                    disabled={saving}
                    type="button"
                    aria-pressed={isActive}
                  >
                    {labels[t]}
                  </button>
                );
              })}
            </div>
          </div>
        </div>

        {/* Footer */}
        <div className="mem-modal__footer">
          <Button variant="ghost" size="sm" onPress={onClose} isDisabled={saving}>
            Cancel
          </Button>
          <Button
            size="sm"
            variant="secondary"
            onPress={handleSave}
            isDisabled={!content.trim() || saving}
          >
            <Plus size={14} strokeWidth={2} />
            {saving ? "Saving…" : "Save Memory"}
          </Button>
        </div>
      </div>
    </div>
  );
}

// ── Consolidation progress banner ─────────────────────────────

type ConsolidationStatus = "idle" | "running" | "done" | "error";

function ConsolidationBanner({
  status,
  message,
  onStop,
  onDismiss,
}: {
  status: ConsolidationStatus;
  message: string;
  onStop: () => void;
  onDismiss: () => void;
}) {
  if (status === "idle") return null;

  const isRunning = status === "running";
  const isError = status === "error";

  const bannerBg = isError
    ? "rgba(239, 68, 68, 0.06)"
    : isRunning
    ? "rgba(147, 51, 234, 0.05)"
    : "rgba(34, 197, 94, 0.06)";

  const bannerBorder = isError
    ? "rgba(239, 68, 68, 0.2)"
    : isRunning
    ? "rgba(147, 51, 234, 0.2)"
    : "rgba(34, 197, 94, 0.2)";

  const textColor = isError
    ? "var(--color-destructive)"
    : isRunning
    ? "var(--purple-700, #6d28d9)"
    : "var(--color-success-fg)";

  return (
    <div
      className="consol-banner"
      style={{ border: `1px solid ${bannerBorder}`, background: bannerBg, color: textColor }}
      role="status"
      aria-live="polite"
    >
      <GitMerge size={14} strokeWidth={1.8} className="consol-banner__icon" />
      <span className="consol-banner__text">
        {isRunning ? "Consolidating memories…" : ""}{" "}
        {message}
      </span>
      {isRunning && (
        <button className="consol-banner__stop-btn" onClick={onStop} aria-label="Stop consolidation">
          Stop
        </button>
      )}
      {!isRunning && (
        <button className="consol-banner__dismiss-btn" onClick={onDismiss} aria-label="Dismiss">
          <X size={13} strokeWidth={2} />
        </button>
      )}
    </div>
  );
}

// ── Memory settings toggles ──────────────────────────────────

function MemorySettingsCard({ settings, onToggle }: {
  settings: Partial<Settings>;
  onToggle: (key: string, value: boolean) => void;
}) {
  const toggles: { key: string; label: string; desc: string; value: boolean }[] = [
    {
      key: "agent_memory_inject",
      label: "Inject into prompt",
      desc: "Include recent memories in the LLM system prompt each turn",
      value: settings.agent_memory_inject ?? true,
    },
    {
      key: "memory_extraction_enabled",
      label: "Auto-extraction",
      desc: "Automatically extract facts from conversations in the background",
      value: settings.memory_extraction_enabled ?? true,
    },
    {
      key: "memory_cleanup_enabled",
      label: "Decay and cleanup",
      desc: "Archive low-scoring memories based on time decay (every 6 hours)",
      value: settings.memory_cleanup_enabled ?? true,
    },
    {
      key: "memory_consolidation_enabled",
      label: "Consolidation",
      desc: "Use the LLM to merge similar memories and prune duplicates (every 24 hours)",
      value: settings.memory_consolidation_enabled ?? false,
    },
  ];

  return (
    <Card className="card">
      <CardContent>
        <div className="mem-settings__header">Memory Settings</div>
        <div className="mem-settings__list">
          {toggles.map((t) => (
            <label key={t.key} className="mem-settings__toggle">
              <input
                type="checkbox"
                className="mem-settings__checkbox"
                checked={t.value}
                onChange={(e) => onToggle(t.key, e.target.checked)}
              />
              <div>
                <div className="mem-settings__toggle-label">{t.label}</div>
                <div className="mem-settings__toggle-desc">{t.desc}</div>
              </div>
            </label>
          ))}
        </div>
      </CardContent>
    </Card>
  );
}

// ── Semantic index coverage ───────────────────────────────────
//
// This screen has always shown what the pond has STORED. It has never shown how
// much of that the assistant can actually find, and those are different numbers:
// retrieval reaches a memory only through a vector stamped with the embedding
// model currently configured, and everything else falls back to recency. An
// index sitting at roughly 2% populated survived six landed phases precisely
// because the figure lived in a log line — the pond answered every question,
// slightly worse, and nothing on any screen disagreed.
//
// So the panel's whole job is to make a corpus at zero impossible to scroll
// past. A percentage alone does not do that: averaged into one figure, two
// healthy corpora hid a third that could never populate at all. A row each,
// with the empty one tinted and named in words, is the shape that shows it.

const CORPUS_META: Record<ContextCorpus, { label: string; holds: string }> = {
  memory:  { label: "Memories",   holds: "facts the assistant extracted from conversation" },
  context: { label: "Context",    holds: "items ingested from sensors and sources" },
  summary: { label: "Summaries",  holds: "the rolling summary of each conversation" },
};

/**
 * What a corpus's two numbers mean, as four cases rather than a percentage.
 *
 * `vacant` and `unindexed` both read 0 indexed and must never be shown the same
 * way: one is a store nobody has written to yet, which is fine and will fix
 * itself, and the other is a store full of rows the assistant cannot reach,
 * which is the defect this panel exists to surface.
 */
export type CoverageState = "vacant" | "excluded" | "unindexed" | "partial" | "complete";

export function coverageState(row: {
  rows: number;
  indexed_rows: number;
  source_rows?: number;
}): CoverageState {
  // Checked BEFORE "vacant", because both are zero qualifying rows and only one
  // of them is fine. A corpus holding rows that all fail the filter is broken in
  // a way embedding cannot touch, and calling that "nothing stored yet" is the
  // sentence that let it hide.
  if (row.rows === 0 && (row.source_rows ?? 0) > 0) return "excluded";
  if (row.rows === 0) return "vacant";
  if (row.indexed_rows === 0) return "unindexed";
  if (row.indexed_rows >= row.rows) return "complete";
  return "partial";
}

/**
 * A coverage fraction as a percentage a person can read.
 *
 * `null` is the server saying "no qualifying rows", which is not 0% and not
 * 100% — see `ContextCorpusCoverage` — so it prints as a dash and the row's own
 * counts carry the meaning instead.
 *
 * Anything above nothing floors at "<1%" rather than rounding to zero. Rounding
 * would print the same "0%" for an index with a handful of vectors and one with
 * none at all, and telling those apart is the reason this number is on screen.
 */
export function formatCoverage(coverage: number | null | undefined): string {
  if (coverage === null || coverage === undefined) return "—";
  const pct = coverage * 100;
  if (pct > 0 && pct < 1) return "<1%";
  return `${Math.round(pct)}%`;
}

/** The row's state said in words, because a bar at zero is easy to read as a bar. */
export function coverageNote(row: ContextCorpusCoverage): string {
  const stale = row.mismatched > 0
    ? ` ${row.mismatched} carry a vector from another model.`
    : "";
  switch (coverageState(row)) {
    case "vacant":
      return "Nothing stored yet.";
    case "excluded":
      return `${row.source_rows} stored, none reachable — a filter excludes every one. Embedding will not fix this.`;
    case "unindexed":
      return `Not searchable — the assistant can only reach these by recency.${stale}`;
    case "partial":
      return `${row.missing_rows} still to embed.${stale}`;
    case "complete":
      return `Fully searchable.${stale}`;
  }
}

const STATE_TONE: Record<CoverageState, { fg: string; tint: string; icon: React.ReactNode }> = {
  excluded:  { fg: "var(--color-destructive)",  tint: "color-mix(in srgb, var(--color-destructive) 9%, transparent)", icon: <AlertTriangle size={13} strokeWidth={2} /> },
  vacant:    { fg: "var(--grey-400)",          tint: "transparent",                                                 icon: <Minus size={13} strokeWidth={2} /> },
  unindexed: { fg: "var(--color-destructive)",  tint: "color-mix(in srgb, var(--color-destructive) 9%, transparent)", icon: <AlertTriangle size={13} strokeWidth={2} /> },
  partial:   { fg: "var(--color-warning-fg)",   tint: "color-mix(in srgb, var(--color-warning) 9%, transparent)",     icon: <AlertTriangle size={13} strokeWidth={2} /> },
  complete:  { fg: "var(--color-success-fg)",   tint: "transparent",                                                 icon: <Check size={13} strokeWidth={2.2} /> },
};

function CorpusCoverageRow({ row }: { row: ContextCorpusCoverage }) {
  const state = coverageState(row);
  const tone = STATE_TONE[state];
  const meta = CORPUS_META[row.corpus];

  // A sliver rather than an honest 2% bar: at this width two percent draws less
  // than one pixel and reads as empty, which is the exact confusion the panel is
  // here to end. The counts beside it stay exact.
  const pct = (row.coverage ?? 0) * 100;
  const barWidth = pct > 0 ? Math.max(3, pct) : 0;

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "7px 8px",
        borderRadius: 8,
        background: tone.tint,
      }}
    >
      <span style={{ color: tone.fg, display: "flex", flexShrink: 0 }} aria-hidden="true">
        {tone.icon}
      </span>

      <div style={{ flex: 1, minWidth: 0 }}>
        {/* Falls back to the raw name because the server owns the corpus list:
            a fourth store added there should show up here unlabelled rather
            than blank, which is the failure mode this panel is about. */}
        <div className="mem-settings__toggle-label">{meta?.label ?? row.corpus}</div>
        <div
          className="mem-settings__toggle-desc"
          title={meta?.holds}
          // The words are the loudest part of the row, so the one state worth
          // alarm gets to keep the alarm colour instead of the muted grey.
          style={{ color: state === "unindexed" || state === "excluded" ? tone.fg : undefined }}
        >
          {coverageNote(row)}
        </div>
      </div>

      <span className="mem-importance__track" style={{ flex: "0 0 84px", maxWidth: 84 }}>
        <span
          className="mem-importance__fill"
          style={{ width: `${barWidth}%`, background: tone.fg }}
        />
      </span>

      <span
        className="mem-importance__label"
        style={{ color: tone.fg, minWidth: 62, fontWeight: 600 }}
      >
        {row.indexed_rows} / {row.rows}
      </span>
    </div>
  );
}

export function IndexCoveragePanel() {
  const confirm = useConfirm();
  const [health, setHealth] = useState<ContextIndexHealth | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [rebuilding, setRebuilding] = useState(false);
  const [notice, setNotice] = useState("");

  function load() {
    setLoading(true);
    setError(null);
    api
      .getContextIndexHealth()
      .then(setHealth)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  async function handleRebuild() {
    const ok = await confirm(
      "Clear every stored vector and embed each row again? Nothing is lost — they are all recomputable — but until the pond catches up, retrieval falls back to recency.",
      { title: "Reindex", confirmLabel: "Reindex" },
    );
    if (!ok) return;
    setRebuilding(true);
    setNotice("");
    try {
      const result = await api.rebuildContextIndex();
      // Said explicitly, because the figures below drop to near zero the instant
      // this returns: the route clears the table and the sweep re-embeds in the
      // background, so a reader who was not told would take the repair for the
      // damage.
      setNotice(
        result.indexed
          ? `Cleared ${result.cleared} ${result.cleared === 1 ? "vector" : "vectors"}. The pond embeds them again in the background, so the figures start near zero and climb.`
          : result.reason ?? "There was no index to clear.",
      );
      load();
    } catch (e) {
      setError(String(e));
    } finally {
      setRebuilding(false);
    }
  }

  const overall = health?.indexed
    ? coverageState({ rows: health.rows ?? 0, indexed_rows: health.matching ?? 0 })
    : null;

  return (
    <Card className="card">
      <CardContent>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10 }}>
          <span
            className="mem-settings__header"
            style={{ marginBottom: 0, display: "inline-flex", alignItems: "center", gap: 6 }}
          >
            <Radar size={14} strokeWidth={1.8} />
            What the assistant can find
          </span>
          <span style={{ flex: 1 }} />
          {health?.indexed && health.model_id && (
            <Chip size="sm" variant="soft" title="Every stored vector has to match this model to be reachable">
              {health.model_id}
              {health.dims ? ` · ${health.dims}d` : ""}
            </Chip>
          )}
          <Button
            size="sm"
            variant="secondary"
            onPress={handleRebuild}
            isDisabled={rebuilding || loading}
          >
            <RefreshCw size={14} strokeWidth={1.8} style={{ opacity: rebuilding ? 0.4 : 1 }} />
            {rebuilding ? "Reindexing…" : "Reindex"}
          </Button>
        </div>

        {error && <p className="inline-error">{error}</p>}
        {notice && (
          <p className="mem-settings__toggle-desc" role="status" style={{ marginBottom: 8 }}>
            {notice}
          </p>
        )}

        {loading ? (
          <div className="mem-card__meta-item">Reading the index…</div>
        ) : !health ? null : !health.indexed ? (
          // Embeddings switched off is a working pond, not a fault — the server
          // answers 200 for it deliberately — so this state gets the server's own
          // sentence and no percentages at all. A 0% here would read as breakage.
          <div style={{ display: "flex", alignItems: "flex-start", gap: 10, padding: "4px 8px" }}>
            <span style={{ color: "var(--color-warning-fg)", display: "flex", flexShrink: 0, marginTop: 2 }} aria-hidden="true">
              <AlertTriangle size={14} strokeWidth={2} />
            </span>
            <div>
              <div className="mem-settings__toggle-label">Nothing is indexed</div>
              <div className="mem-settings__toggle-desc">
                {health.reason ?? "This pond has no semantic index."}
              </div>
            </div>
          </div>
        ) : (
          <>
            <div style={{ display: "flex", alignItems: "baseline", gap: 8, marginBottom: 8 }}>
              <span
                style={{
                  fontSize: 22,
                  fontWeight: 600,
                  fontFamily: "var(--font-mono)",
                  color: overall ? STATE_TONE[overall].fg : "var(--fg)",
                }}
              >
                {formatCoverage(health.coverage)}
              </span>
              <span className="mem-card__meta-item">
                {health.matching ?? 0} of {health.rows ?? 0} rows searchable
              </span>
              {(health.mismatched ?? 0) > 0 && (
                <span className="mem-card__meta-item" style={{ color: "var(--color-warning-fg)" }}>
                  <Layers size={10} strokeWidth={2} />
                  {health.mismatched} from another model
                </span>
              )}
            </div>

            <div className="mem-settings__list">
              {health.corpora.map((row) => (
                <CorpusCoverageRow key={row.corpus} row={row} />
              ))}
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}

// ── What the extraction engine is doing ───────────────────────

/// How long the engine may go without a successful pass before the household is
/// told. A day: passes need the house to be quiet for fifteen minutes and then
/// to win a tick against five other background jobs, so a few hours of silence
/// is ordinary and a full day is not.
const STALE_PASS_MS = 24 * 60 * 60 * 1000;

/// A warning where a person will see it, rather than a field in a JSON body.
///
/// The failures this covers are quiet ones. A pond whose embedder never loaded
/// reads nothing and looks exactly like a pond with nothing left to read. A
/// pond with several members and no way to tell them apart skips one person's
/// conversations forever and keeps working perfectly for everybody else. Both
/// cost months of history, and neither announces itself.
export function ExtractionBanner({ status }: { status: ExtractionStatus | null }) {
  if (!status) return null;

  const stale =
    status.running &&
    status.last_pass_at !== null &&
    Date.now() - new Date(status.last_pass_at).getTime() > STALE_PASS_MS;

  // Said plainly, and only about what is actually known. A blocked engine and a
  // stale one are different sentences because they call for different things.
  let warning: string | null = null;
  if (!status.running) {
    warning =
      "Nothing is reading conversations into memory on this pond. The batch engine is not " +
      "running here, so new conversations will not be remembered.";
  } else if (status.blocked_on === "no_embedder") {
    warning =
      "Memory extraction is stopped: no embedding model is loaded, so the pond cannot tell a " +
      "new memory from one it already has. Nothing new is being remembered.";
  } else if (status.blocked_on === "no_provider" || status.blocked_on === "provider_error") {
    warning =
      "Memory extraction is stopped: the language model could not be reached on the last pass.";
  } else if (status.blocked_on === "unnameable_subject") {
    // Deliberately no sentence of its own: `skipping` below says the same
    // thing with the count and with what releases it, and it says it whether
    // or not the pass managed to read something else.
    warning = null;
  } else if (stale) {
    warning =
      "Memory extraction has not completed a pass in over a day. It only runs when the house " +
      "is quiet, so this can be ordinary — but nothing new has been remembered since then.";
  }

  // Said whether or not anything else is wrong, and not only when the rest of
  // the pond looks healthy. A pond can be reading typed conversations perfectly
  // and never remembering a word anybody says out loud: the voice surface runs
  // as its own process with no request behind it, so nothing on its path can
  // say who is speaking, and on a household with more than one member every one
  // of those conversations is left alone rather than filed under a guess. That
  // is the case where this number reads highest and the banner used to hide it.
  const skipping =
    (status.unattributed_sessions ?? 0) > 0
      ? `${status.unattributed_sessions} conversation(s) are not being remembered at all, ` +
        "because more than one person lives here and nothing said whose they are. Anything " +
        "spoken to the pond is the usual reason. Identifying one — a paired phone, a face, or " +
        "choosing the person in the conversation — is what releases it."
      : null;

  // Every date the last pass threw away, counted per refused note by the engine
  // and rendered here for the first time. It used to be computed, exposed over
  // HTTP, documented in the TS interface, and drawn by nothing: on a pond whose
  // model puts dates in notes and files no reminders at all — one measured model
  // did that on 432 of 432 opportunities — the two numbers this panel did read
  // were both 0, so it showed no banner while every refused date was discarded.
  //
  // The cause clause is here because the two causes need different answers: a
  // write that failed is the POND, and somebody can go and look at the store; no
  // reminder filed at all is the MODEL, and the answer is a different model.
  const remindersLost = status.last_pass_reminders_lost ?? 0;
  const datesGone = status.last_pass_dates_lost ?? 0;
  // Both halves of the pair, because the ratio is the thing: 1 of 20 is a model
  // slipping and 20 of 20 is a model that never files a reminder at all.
  const datesRefused = Math.max(status.last_pass_dated ?? 0, datesGone);
  const datesLost =
    datesGone > 0
      ? `${datesGone} of the ${datesRefused} date(s) the last pass refused were not kept ` +
        "anywhere. A one-off date is never kept as a memory — it is kept as a reminder " +
        "instead — and no saved reminder matches the notes those dates were in. " +
        (remindersLost > 0
          ? `The pond could not save ${remindersLost} reminder(s) from that pass.`
          : "The model filed no reminder for them.")
      : null;

  // The same failure where no dated note was involved at all: the model filed a
  // reminder, the store would not take it, and nothing else on this panel has a
  // symptom for that. Only when the sentence above is not already saying it.
  const remindersFailed =
    remindersLost > 0 && datesGone === 0
      ? `${remindersLost} reminder(s) from the last pass could not be saved.`
      : null;

  // The other half of the same sentence, and the half that was missing. Saying
  // only what was lost lets a silent panel mean either "nothing was refused" or
  // "everything was refused and nothing was kept" — and for one release it
  // always meant the second, because nothing stored a reminder at all. This is
  // the pond saying what it actually has: a count of rows it wrote, not an
  // inference from a zero somewhere else.
  const datesKept =
    (status.last_pass_reminders_written ?? 0) > 0
      ? `${status.last_pass_reminders_written} date(s) from the last pass were kept as ` +
        "reminders rather than as memories — a one-off date read back months later would be " +
        "false, so the pond keeps the words that were said and not a date it worked out."
      : null;

  return (
    <Card className="mem-extraction">
      <CardContent>
        <div className="mem-extraction__row">
          {warning || skipping || datesLost || remindersFailed ? (
            <AlertTriangle size={14} strokeWidth={1.8} className="mem-extraction__warn" />
          ) : (
            <BrainCircuit size={14} strokeWidth={1.8} />
          )}
          <span className="mem-extraction__text">
            {warning ?? (
              <>
                {status.sessions_pending} of {status.sessions_total} conversation(s) still to
                read. The pond reads them in its own time, when the house is quiet — a long
                history takes a few nights.
              </>
            )}
            {skipping ? ` ${skipping}` : ""}
            {datesLost ? ` ${datesLost}` : ""}
            {remindersFailed ? ` ${remindersFailed}` : ""}
            {datesKept ? ` ${datesKept}` : ""}
          </span>
          {status.mode === "shadow" && (
            <Chip size="sm" variant="soft">
              reading only
            </Chip>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

// ── Main Memory component ─────────────────────────────────────

export function Memory() {
  const confirm = useConfirm();
  const [items, setItems]           = useState<MemoryFragment[]>([]);
  const PAGE = 20;
  const [loading, setLoading]       = useState(true);
  const [error, setError]           = useState<string | null>(null);
  const [activeSegment, setSegment] = useState<SegmentKey>("all");
  const [activeTier, setTier]       = useState<TierKey>("all");
  const [activeSource, setSource]   = useState<SourceKey>("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [visibleCount, setVisibleCount] = useState(PAGE);
  const [inlineDraft, setInlineDraft] = useState("");
  const [inlineAdding, setInlineAdding] = useState(false);
  const [showAddModal, setShowAddModal] = useState(false);
  const [memSettings, setMemSettings] = useState<Partial<Settings>>({});
  const [extraction, setExtraction] = useState<ExtractionStatus | null>(null);

  // Consolidation state
  const [consolidationStatus, setConsolidationStatus] = useState<ConsolidationStatus>("idle");
  const [consolidationMsg, setConsolidationMsg] = useState("");

  function load() {
    setLoading(true);
    setError(null);
    api
      .listMemories(100)
      .then(setItems)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  function loadSettings() {
    api.getSettings()
      .then(setMemSettings)
      .catch(() => {}); // non-fatal
  }

  function loadExtraction() {
    api.getExtractionStatus()
      .then(setExtraction)
      .catch(() => setExtraction(null)); // an older server has no such route
  }

  useEffect(() => { load(); loadSettings(); loadExtraction(); }, []);

  async function handleToggleSetting(key: string, value: boolean) {
    const patch = { [key]: value } as Partial<Settings>;
    setMemSettings((prev) => ({ ...prev, ...patch }));
    try {
      await api.updateSettings(patch);
    } catch (e) {
      setError(String(e));
      loadSettings();
    }
  }

  async function handleAdd(
    content: string,
    segment?: MemorySegment,
    importance?: number,
    tier?: MemoryTier,
  ) {
    try {
      await api.addMemory(content, undefined, segment, importance, tier);
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  // Inline quick-add (no segment/tier selection)
  async function handleInlineAdd() {
    const trimmed = inlineDraft.trim();
    if (!trimmed) return;
    setInlineAdding(true);
    try {
      await api.addMemory(trimmed, undefined, undefined, undefined, undefined);
      setInlineDraft("");
      load();
    } catch (e) {
      setError(String(e));
    } finally {
      setInlineAdding(false);
    }
  }

  async function handleDelete(id: string) {
    if (!await confirm("Delete this memory? This cannot be undone.", { title: "Delete Memory", confirmLabel: "Delete", destructive: true })) return;
    try {
      await api.deleteMemory(id);
      setItems((prev) => prev.filter((m) => m.id !== id));
    } catch (e) {
      setError(String(e));
    }
  }

  // Edit = delete old + create new with same metadata
  async function handleUpdate(id: string, newContent: string) {
    const original = items.find((m) => m.id === id);
    if (!original) return;
    try {
      await api.addMemory(
        newContent,
        original.tags,
        original.segment,
        original.importance,
        original.tier,
      );
      await api.deleteMemory(id);
      setItems((prev) =>
        prev.map((m) =>
          m.id === id
            ? { ...m, content: newContent }
            : m,
        ),
      );
      load();
    } catch (e) {
      setError(String(e));
      throw e;
    }
  }

  // Only show active lifecycle memories — with fallback for old API without lifecycle
  const visibleItems = items.filter(
    (m) => !m.lifecycle || m.lifecycle === "active",
  );

  async function handleDeleteAll() {
    const count = visibleItems.length;
    if (!count) return;
    if (!await confirm(`Delete all ${count} memories? This cannot be undone.`, { title: "Delete All Memories", confirmLabel: "Delete All", destructive: true })) return;
    const errs: string[] = [];
    for (const m of visibleItems) {
      try { await api.deleteMemory(m.id); }
      catch (e) { errs.push(String(e)); }
    }
    load();
    if (errs.length) setError(`${errs.length} deletion(s) failed.`);
  }

  // Consolidation
  async function handleConsolidate() {
    if (consolidationStatus === "running") return;
    setConsolidationStatus("running");
    setConsolidationMsg("");
    try {
      for await (const event of api.streamConsolidation()) {
        switch (event.type) {
          case "started":
            setConsolidationMsg(`Analyzing ${event.memory_count ?? ""} memories...`);
            break;
          case "proposer_done":
            setConsolidationMsg(`Proposer found ${(event.proposals as unknown[])?.length ?? 0} changes`);
            break;
          case "adversary_done":
            setConsolidationMsg("Adversary reviewing proposals...");
            break;
          case "judge_done":
            setConsolidationMsg("Judge making final decisions...");
            break;
          case "applied":
            setConsolidationMsg("Applying accepted changes...");
            break;
          case "completed":
            if (event.result) {
              const { accepted_count, rejected_count, duration_ms } = event.result;
              const secs = (duration_ms / 1000).toFixed(1);
              setConsolidationMsg(
                `Done in ${secs}s — ${accepted_count} accepted, ${rejected_count} rejected.`,
              );
            } else {
              setConsolidationMsg("Done.");
            }
            setConsolidationStatus("done");
            load();
            break;
          case "error":
            setConsolidationMsg(event.message ?? "Consolidation failed.");
            setConsolidationStatus("error");
            break;
          case "cancelled":
            setConsolidationMsg("Cancelled.");
            setConsolidationStatus("done");
            break;
        }
        if (event.type === "completed" || event.type === "error" || event.type === "cancelled") break;
      }
    } catch (e) {
      setConsolidationMsg(String(e));
      setConsolidationStatus("error");
    }
  }

  async function handleStopConsolidation() {
    try {
      await api.stopConsolidation();
    } catch {
      // best-effort
    }
    setConsolidationStatus("done");
    setConsolidationMsg("Cancelled.");
  }

  // Segment counts for filter row
  const segCounts = SEGMENT_ORDER.reduce<Partial<Record<MemorySegment, number>>>((acc, seg) => {
    const n = visibleItems.filter((m) => m.segment === seg).length;
    if (n > 0) acc[seg] = n;
    return acc;
  }, {});

  // Tier counts
  const tierCounts = (["short", "long", "permanent"] as MemoryTier[]).reduce<Partial<Record<MemoryTier, number>>>((acc, tier) => {
    const n = visibleItems.filter((m) => m.tier === tier).length;
    if (n > 0) acc[tier] = n;
    return acc;
  }, {});

  // Source counts
  const sourceCounts = (["auto", "mcp", "chat"] as SourceKey[]).reduce<Partial<Record<SourceKey, number>>>((acc, src) => {
    const n = visibleItems.filter((m) => normalizedSource(m.source) === src).length;
    if (n > 0) acc[src] = n;
    return acc;
  }, {});

  // Apply all filters
  const filtered = visibleItems.filter((m) => {
    if (activeSegment !== "all" && m.segment !== activeSegment) return false;
    if (activeTier !== "all" && m.tier !== activeTier) return false;
    if (activeSource !== "all" && normalizedSource(m.source) !== activeSource) return false;
    if (searchQuery) {
      const q = searchQuery.toLowerCase();
      if (!m.content.toLowerCase().includes(q)) return false;
    }
    return true;
  });

  const hasActiveFilters = activeSegment !== "all" || activeTier !== "all" || activeSource !== "all" || searchQuery !== "";

  return (
    <div className="screen">
      {/* Page header */}
      <PageHeader
        title="Memory"
        action={
          <>
            <Chip size="sm" variant="soft" className="mem-count-chip">
              {loading ? "…" : visibleItems.length} memories
            </Chip>
            <span className="mem-header__sep" />
            <Button size="sm" variant="secondary" onPress={load} isDisabled={loading}>
              <RefreshCw size={14} strokeWidth={1.8} style={{ opacity: loading ? 0.4 : 1 }} />
              Refresh
            </Button>
            <Button
              size="sm"
              variant="secondary"
              onPress={handleConsolidate}
              isDisabled={consolidationStatus === "running" || loading || visibleItems.length === 0}
            >
              <GitMerge size={14} strokeWidth={1.8} />
              Consolidate
            </Button>
            {!loading && visibleItems.length > 0 && (
              <Button size="sm" variant="danger-soft" onPress={handleDeleteAll}>
                <Trash2 size={14} strokeWidth={1.8} />
                Delete All
              </Button>
            )}
          </>
        }
      />

      {/* What the extraction engine is doing, and what it cannot do */}
      <ExtractionBanner status={extraction} />

      {/* Add memory modal */}
      {showAddModal && (
        <AddMemoryModal
          onAdd={handleAdd}
          onClose={() => setShowAddModal(false)}
        />
      )}

      {/* Consolidation progress */}
      <ConsolidationBanner
        status={consolidationStatus}
        message={consolidationMsg}
        onStop={handleStopConsolidation}
        onDismiss={() => { setConsolidationStatus("idle"); setConsolidationMsg(""); }}
      />

      {/* Error */}
      {error && <p className="inline-error">{error}</p>}

      {/* Inline add form — design-spec card */}
      <Card className="card">
        <CardContent>
          <div className="mem-add">
            <div className="mem-add__icon-wrap">
              <span className="mem-add__icon">
                <Sparkles size={14} strokeWidth={1.8} />
              </span>
              <input
                type="text"
                className="mem-add__input-field"
                placeholder="Add a memory…"
                value={inlineDraft}
                onChange={(e) => setInlineDraft(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") handleInlineAdd(); }}
                disabled={inlineAdding}
                aria-label="Quick add memory"
              />
            </div>
            <Button
              size="sm"
              variant="primary"
              onPress={handleInlineAdd}
              isDisabled={!inlineDraft.trim() || inlineAdding}
            >
              <Plus size={14} strokeWidth={2} />
              {inlineAdding ? "Adding…" : "Add"}
            </Button>
            <Button
              size="sm"
              variant="secondary"
              onPress={() => setShowAddModal(true)}
            >
              Advanced
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* What retrieval can reach — sits above the list because a coverage
          figure below twenty memories is a coverage figure nobody scrolls to,
          which is how the 2% index stayed unnoticed in the first place. */}
      <IndexCoveragePanel />

      {/* Stats row */}
      {!loading && visibleItems.length > 0 && (
        <MemStatsBar items={visibleItems} />
      )}

      {/* Search bar */}
      {!loading && visibleItems.length > 0 && (
        <div className="mem-search">
          <span className="mem-search__icon">
            <Search size={14} strokeWidth={1.8} />
          </span>
          <input
            className="mem-search__input"
            type="search"
            placeholder="Search memories…"
            value={searchQuery}
            onChange={(e) => { setSearchQuery(e.target.value); setVisibleCount(PAGE); }}
            aria-label="Search memories"
          />
          {searchQuery && (
            <button
              className="mem-search__clear"
              onClick={() => setSearchQuery("")}
              aria-label="Clear search"
              type="button"
            >
              <X size={11} strokeWidth={2.5} />
            </button>
          )}
        </div>
      )}

      {/* Filter row */}
      {!loading && visibleItems.length > 0 && (
        <MemFilterRow
          activeSegment={activeSegment}
          activeTier={activeTier}
          activeSource={activeSource}
          segCounts={segCounts}
          tierCounts={tierCounts}
          sourceCounts={sourceCounts}
          total={visibleItems.length}
          onSegmentChange={(v) => { setSegment(v); setVisibleCount(PAGE); }}
          onTierChange={(v) => { setTier(v); setVisibleCount(PAGE); }}
          onSourceChange={(v) => { setSource(v); setVisibleCount(PAGE); }}
        />
      )}

      {/* Memory list card */}
      {loading ? (
        <SkeletonList rows={5} />
      ) : visibleItems.length === 0 ? (
        <Card className="card">
          <CardContent>
            <div className="empty-state">
              <Brain size={36} strokeWidth={1.2} />
              <div style={{ maxWidth: 320 }}>
                <div className="mem-empty__title">No memories yet</div>
                <div className="mem-empty__desc">
                  The assistant builds up memory as you chat — facts about you, your preferences, and ongoing projects get saved automatically.
                </div>
                <div className="mem-empty__tips">
                  <div className="mem-empty__tip">
                    <MessageSquare size={13} strokeWidth={1.8} className="mem-empty__tip-icon" />
                    Chat with the assistant to create memories automatically
                  </div>
                  <div className="mem-empty__tip">
                    <Plus size={13} strokeWidth={2} className="mem-empty__tip-icon" />
                    Or add one manually with the form above
                  </div>
                </div>
              </div>
            </div>
          </CardContent>
        </Card>
      ) : filtered.length === 0 ? (
        <div className="empty-state empty-state--inline">
          <Search size={18} strokeWidth={1.8} />
          <span>
            {searchQuery
              ? `No memories match "${searchQuery}".`
              : hasActiveFilters
              ? "No memories match the current filters."
              : "No memories."}
          </span>
        </div>
      ) : (
        <>
          <Card className="card">
            <CardContent className="card-body--list">
              {filtered.slice(0, visibleCount).map((m) => (
                <MemRow
                  key={m.id}
                  mem={m}
                  onDelete={handleDelete}
                  onUpdate={handleUpdate}
                />
              ))}
            </CardContent>
          </Card>
          {filtered.length > visibleCount && (
            <button className="empty-state__cta" style={{ alignSelf: "center" }} onClick={() => setVisibleCount((c) => c + PAGE)}>
              Show {Math.min(PAGE, filtered.length - visibleCount)} more
              <span style={{ color: "var(--grey-400)", fontWeight: "normal" }}> · {filtered.length - visibleCount} remaining</span>
            </button>
          )}
        </>
      )}

      {/* Memory lifecycle settings */}
      <MemorySettingsCard settings={memSettings} onToggle={handleToggleSetting} />
    </div>
  );
}
