import { useState, useEffect, useCallback, useRef } from "react";
import { Plus, RefreshCw, Loader2, Clock } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card, Row, Toggle } from "./controls";
import { api } from "../../../api/PondApiClient";
import type { MemoryFragment, MemorySegment } from "../../../api/types";

// ─── Icon path strings ────────────────────────────────────────
const TRASH_PATH =
  "M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6";

// ─── Segment dot colours (matches legacy section palette) ────
const SEG_COLOR: Record<MemorySegment, string> = {
  identity:     "#7C3AED",
  preference:   "#DB2777",
  correction:   "#D97706",
  relationship: "#DC2626",
  project:      "#2563EB",
  knowledge:    "#16A34A",
  context:      "#6B7280",
};

const SEG_LABEL: Record<MemorySegment, string> = {
  identity:     "Identity",
  preference:   "Preference",
  correction:   "Correction",
  relationship: "Relationship",
  project:      "Project",
  knowledge:    "Knowledge",
  context:      "Context",
};

// ─── Mock fallback (offline / first paint) ───────────────────
const MOCK_MEMS: MemoryFragment[] = [
  {
    id: "mock-1",
    content: "Prefers the house at 70° morning, 66° overnight.",
    segment: "preference",
    created_at: new Date(Date.now() - 3 * 24 * 60 * 60 * 1000).toISOString(),
    access_count: 0,
  },
  {
    id: "mock-2",
    content: "Is a software engineer; works from the office most weekdays.",
    segment: "identity",
    created_at: new Date(Date.now() - 7 * 24 * 60 * 60 * 1000).toISOString(),
    access_count: 0,
  },
  {
    id: "mock-3",
    content: "Lactose intolerant — avoid dairy in recipe suggestions.",
    segment: "preference",
    created_at: new Date(Date.now() - 11 * 24 * 60 * 60 * 1000).toISOString(),
    access_count: 0,
  },
];

// ─── Helpers ─────────────────────────────────────────────────
function formatDate(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

// ─── Skeleton row ─────────────────────────────────────────────
function SkeletonRow() {
  return (
    <div className="memrow" style={{ opacity: 0.5 }}>
      <span
        className="memrow__dot"
        style={{ background: "#e2e8f0", flexShrink: 0 }}
      />
      <span
        className="memrow__text"
        style={{
          display: "block",
          height: 12,
          width: "60%",
          background: "#e2e8f0",
          borderRadius: 4,
        }}
      />
      <span
        className="memrow__date"
        style={{
          display: "block",
          height: 10,
          width: 40,
          background: "#f1f5f9",
          borderRadius: 4,
        }}
      />
    </div>
  );
}

// ─── Composer card (inline add) ───────────────────────────────
interface ComposerProps {
  onSave: (content: string, segment?: MemorySegment) => Promise<void>;
  onCancel: () => void;
}

function Composer({ onSave, onCancel }: ComposerProps) {
  const [content, setContent] = useState("");
  const [segment, setSegment] = useState<MemorySegment | "">("");
  const [saving, setSaving] = useState(false);
  const taRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    taRef.current?.focus();
  }, []);

  async function handleSave() {
    const trimmed = content.trim();
    if (!trimmed) return;
    setSaving(true);
    try {
      await onSave(trimmed, segment !== "" ? segment : undefined);
    } finally {
      setSaving(false);
    }
  }

  const segOptions: MemorySegment[] = [
    "identity", "preference", "correction", "relationship",
    "project", "knowledge", "context",
  ];

  return (
    <div
      style={{
        background: "#F8FAFC",
        border: "1px solid #E2E8F0",
        borderRadius: 10,
        padding: "12px 14px",
        display: "flex",
        flexDirection: "column",
        gap: 10,
      }}
    >
      <textarea
        ref={taRef}
        value={content}
        onChange={(e) => setContent(e.target.value)}
        disabled={saving}
        rows={3}
        placeholder="What should Goose remember?"
        style={{
          width: "100%",
          border: "1px solid #CBD5E1",
          borderRadius: 7,
          padding: "8px 10px",
          fontSize: 13,
          fontFamily: "var(--font-body, system-ui)",
          background: "#fff",
          color: "#1e293b",
          outline: "none",
          resize: "vertical",
          lineHeight: 1.55,
          boxSizing: "border-box",
        }}
        aria-label="Memory content"
      />
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <select
          value={segment}
          onChange={(e) => setSegment(e.target.value as MemorySegment | "")}
          disabled={saving}
          style={{
            flex: 1,
            height: 32,
            padding: "0 8px",
            border: "1px solid #CBD5E1",
            borderRadius: 7,
            fontSize: 12,
            fontFamily: "var(--font-body, system-ui)",
            background: "#fff",
            color: "#475569",
            outline: "none",
            cursor: "pointer",
          }}
          aria-label="Memory segment"
        >
          <option value="">Auto-classify</option>
          {segOptions.map((s) => (
            <option key={s} value={s}>{SEG_LABEL[s]}</option>
          ))}
        </select>

        <button
          className="mrow__btn"
          type="button"
          onClick={onCancel}
          disabled={saving}
          aria-label="Cancel add memory"
          style={{ minWidth: 60 }}
        >
          Cancel
        </button>

        <button
          className="primary-btn"
          type="button"
          onClick={handleSave}
          disabled={saving || !content.trim()}
          style={{
            background: "#0D9488",
            boxShadow: "0 4px 12px rgba(13,148,136,.22)",
            opacity: saving || !content.trim() ? 0.6 : 1,
          }}
          aria-label="Save memory"
        >
          {saving ? (
            <Loader2 size={13} style={{ animation: "spin 1s linear infinite" }} />
          ) : (
            <Plus size={13} color="#fff" strokeWidth={2.2} />
          )}
          {saving ? "Saving…" : "Save"}
        </button>
      </div>
    </div>
  );
}

// ─── Component ───────────────────────────────────────────────
interface MemoryDetailProps {
  go: (route: string) => void;
}

export function MemoryDetail({ go }: MemoryDetailProps) {
  const [mems, setMems] = useState<MemoryFragment[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [showComposer, setShowComposer] = useState(false);
  const [compactionEnabled, setCompactionEnabled] = useState(false);
  // Mirrors the sections Settings view: both UIs ship, so the control must exist in both.
  const [embeddingProvider, setEmbeddingProvider] = useState("fastembed");
  const [flash, setFlash] = useState<{ text: string; ok: boolean } | null>(null);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  function showFlash(text: string, ok = true) {
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlash({ text, ok });
    flashTimer.current = setTimeout(() => setFlash(null), 3000);
  }

  const loadData = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [fragments, settings] = await Promise.all([
        api.listMemories(100),
        api.getSettings().catch(() => null),
      ]);
      // Only show active lifecycle memories; fall back for old API (no lifecycle field)
      const active = fragments.filter(
        (m) => !m.lifecycle || m.lifecycle === "active",
      );
      setMems(active);
      if (settings != null) {
        setCompactionEnabled(
          settings.memory_consolidation_enabled === true,
        );
        if (settings.embedding_provider) {
          setEmbeddingProvider(settings.embedding_provider);
        }
      }
    } catch (e) {
      console.warn("[MemoryDetail] API offline — using mock fallback:", e);
      setMems(MOCK_MEMS);
      setError("Could not reach the server. Showing offline view.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadData();
    return () => {
      if (flashTimer.current) clearTimeout(flashTimer.current);
    };
  }, [loadData]);

  async function handleDelete(mem: MemoryFragment) {
    setDeleting(mem.id);
    // Optimistic removal
    setMems((prev) => prev.filter((m) => m.id !== mem.id));
    try {
      await api.deleteMemory(mem.id);
      showFlash("Memory forgotten.");
    } catch (e) {
      // Rollback
      setMems((prev) => [mem, ...prev]);
      showFlash(`Failed to delete: ${String(e)}`, false);
    } finally {
      setDeleting(null);
    }
  }

  async function handleAdd(content: string, segment?: MemorySegment) {
    try {
      await api.addMemory(content, undefined, segment);
      setShowComposer(false);
      showFlash("Memory saved.");
      await loadData();
    } catch (e) {
      showFlash(`Failed to save: ${String(e)}`, false);
      throw e;
    }
  }

  async function handleChangeEmbeddingProvider(next: string) {
    const previous = embeddingProvider;
    setEmbeddingProvider(next);
    try {
      await api.updateSettings(
        { embedding_provider: next } as Parameters<typeof api.updateSettings>[0],
      );
      showFlash(
        next === "none"
          ? "Embeddings off — recall falls back to keywords"
          : "Embedding provider changed. Existing memories keep their old vectors until they are re-embedded.",
      );
    } catch (e) {
      setEmbeddingProvider(previous);
      showFlash(`Settings update failed: ${String(e)}`, false);
    }
  }

  async function handleToggleCompaction(on: boolean) {
    setCompactionEnabled(on);
    try {
      await api.updateSettings(
        { memory_consolidation_enabled: on } as Parameters<typeof api.updateSettings>[0],
      );
    } catch (e) {
      // Revert on failure
      setCompactionEnabled(!on);
      showFlash(`Settings update failed: ${String(e)}`, false);
    }
  }

  const visibleMems = mems;

  return (
    <DetailShell
      title="Memory"
      subtitle={`${loading ? "…" : visibleMems.length} things Goose remembers about you. Stored on-device.`}
      accent="#0D9488"
      onBack={() => go("settings")}
      headRight={
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <button
            className="mrow__btn"
            type="button"
            onClick={loadData}
            aria-label="Refresh memories"
            style={{ minWidth: 32, display: "flex", alignItems: "center", justifyContent: "center" }}
          >
            <RefreshCw
              size={13}
              strokeWidth={2}
              style={{ opacity: loading ? 0.4 : 1 }}
            />
          </button>
          <button
            className="primary-btn"
            type="button"
            style={{ background: "#0D9488", boxShadow: "0 6px 16px rgba(13,148,136,.28)" }}
            onClick={() => setShowComposer((v) => !v)}
            aria-label={showComposer ? "Close add memory" : "Add a memory"}
          >
            <Plus size={15} color="#fff" strokeWidth={2.2} />
            {showComposer ? "Cancel" : "Add"}
          </button>
        </div>
      }
    >
      {/* Flash feedback */}
      {flash && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: flash.ok ? "#f0fdf4" : "#fef2f2",
            color: flash.ok ? "#16a34a" : "#dc2626",
            border: `1px solid ${flash.ok ? "#bbf7d0" : "#fecaca"}`,
          }}
          role="status"
          aria-live="polite"
        >
          {flash.text}
        </div>
      )}

      {/* Offline error banner */}
      {error && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: "#fffbeb",
            color: "#92400e",
            border: "1px solid #fde68a",
          }}
        >
          {error}
        </div>
      )}

      {/* Inline composer */}
      {showComposer && !loading && (
        <Composer
          onSave={handleAdd}
          onCancel={() => setShowComposer(false)}
        />
      )}

      {/* Auto-compaction */}
      <Card title="Auto-compaction">
        <Row
          label="Compact memory nightly"
          sub="Merge and dedupe at 3:00 AM to keep memory lean"
          control={
            <Toggle
              on={compactionEnabled}
              onChange={handleToggleCompaction}
            />
          }
        />
      </Card>

      {/* How memories are searched */}
      <Card title="Semantic search">
        <Row
          label="Embedding provider"
          sub="GGUF uses the same engine as chat and is the one that starts on a Jetson. Changing this changes the vector space, so existing memories stop matching until they are re-embedded."
          control={
            <select
              className="native-select"
              aria-label="Embedding provider"
              value={embeddingProvider}
              onChange={(e) => handleChangeEmbeddingProvider(e.target.value)}
            >
              <option value="fastembed">FastEmbed (local ONNX)</option>
              <option value="gguf">GGUF (llama.cpp, on-device)</option>
              <option value="none">None (keyword only)</option>
            </select>
          }
        />
      </Card>

      {/* Memory list */}
      <Card title="Remembered">
        <div className="memlist">
          {loading ? (
            <>
              <SkeletonRow />
              <SkeletonRow />
              <SkeletonRow />
            </>
          ) : visibleMems.length === 0 ? (
            <div className="memempty">
              Goose hasn't remembered anything yet — add a fact above.
            </div>
          ) : (
            visibleMems.map((m) => {
              const dotColor = m.segment
                ? SEG_COLOR[m.segment]
                : "var(--color-text-tertiary)";
              const isDeleting = deleting === m.id;

              return (
                <div
                  key={m.id}
                  className="memrow"
                  style={{ opacity: isDeleting ? 0.4 : 1, transition: "opacity 150ms" }}
                >
                  <span
                    className="memrow__dot"
                    style={{ background: dotColor, flexShrink: 0 }}
                  />
                  <span className="memrow__text">{m.content}</span>
                  <span className="memrow__date">
                    <Clock
                      size={9}
                      strokeWidth={1.8}
                      style={{ display: "inline", marginRight: 3, verticalAlign: "middle", opacity: 0.7 }}
                    />
                    {formatDate(m.created_at)}
                  </span>
                  <button
                    className="memrow__del"
                    type="button"
                    onClick={() => handleDelete(m)}
                    disabled={isDeleting}
                    aria-label={`Forget: ${m.content}`}
                  >
                    <HubIco d={TRASH_PATH} size={14} color={isDeleting ? "#E2E8F0" : "#CBD5E1"} />
                  </button>
                </div>
              );
            })
          )}
        </div>
      </Card>
    </DetailShell>
  );
}
