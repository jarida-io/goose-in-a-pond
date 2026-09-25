import { useState, useEffect, useMemo, useId, useRef } from "react";
import { Button, Chip } from "@heroui/react";
import { Search, Check } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { ModelEntry } from "../api/types";
import { useDialogFocusTrap } from "./shared";

export type ModelRole = "chat";

interface ModelPickerModalProps {
  role: ModelRole;
  currentProvider?: string | null;
  currentModel?: string | null;
  onSelect: (provider: string, model: string) => void;
  onClose: () => void;
}

const ROLE_LABELS: Record<ModelRole, string> = {
  chat:  "Main LLM",
};

export function ModelPickerModal({
  role,
  currentProvider,
  currentModel,
  onSelect,
  onClose,
}: ModelPickerModalProps) {
  const [models, setModels]   = useState<ModelEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError]     = useState<string | null>(null);
  const [search, setSearch]   = useState("");
  const [selected, setSelected] = useState<{ provider: string; model: string } | null>(
    currentProvider && currentModel ? { provider: currentProvider, model: currentModel } : null,
  );

  const titleId = useId();
  const searchInputRef = useRef<HTMLInputElement>(null);
  const dialogRef = useDialogFocusTrap<HTMLDivElement>(true, onClose, searchInputRef);

  useEffect(() => {
    api.listModels()
      .then(setModels)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  const filtered = useMemo(() => {
    const q = search.toLowerCase().trim();
    if (!q) return models;
    return models.filter(
      (m) =>
        (m.display_name ?? m.name).toLowerCase().includes(q) ||
        m.provider.toLowerCase().includes(q) ||
        (m.recommended_role ?? "").toLowerCase().includes(q),
    );
  }, [models, search]);

  const grouped = useMemo(() => {
    const map = new Map<string, ModelEntry[]>();
    for (const m of filtered) {
      const group = map.get(m.provider) ?? [];
      group.push(m);
      map.set(m.provider, group);
    }
    return Array.from(map.entries());
  }, [filtered]);

  function isSelected(m: ModelEntry) {
    return selected?.provider === m.provider && selected?.model === m.name;
  }

  function confirm() {
    if (selected) {
      onSelect(selected.provider, selected.model);
    }
  }

  return (
    <div style={styles.overlay} onClick={onClose}>
      <div
        ref={dialogRef}
        style={styles.modal}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div style={styles.header}>
          <h2 id={titleId} style={styles.title}>Select {ROLE_LABELS[role]} Model</h2>
          <button style={styles.closeBtn} onClick={onClose} aria-label="Close">✕</button>
        </div>

        {/* Search */}
        <div style={styles.searchWrap}>
          <Search size={14} style={styles.searchIcon} />
          <input
            ref={searchInputRef}
            style={styles.searchInput}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search models…"
            aria-label="Search models"
          />
        </div>

        {/* Body */}
        <div style={styles.body}>
          {loading && <p style={styles.hint}>Loading models…</p>}
          {error   && <p style={{ ...styles.hint, color: "var(--color-destructive)" }}>{error}</p>}
          {!loading && !error && grouped.length === 0 && (
            <p style={styles.hint}>No models found.</p>
          )}

          {grouped.map(([provider, group]) => (
            <div key={provider} style={styles.group}>
              <p style={styles.groupLabel}>{provider}</p>
              {group.map((m) => (
                <button
                  key={m.id}
                  style={{
                    ...styles.row,
                    ...(isSelected(m) ? styles.rowSelected : {}),
                  }}
                  onClick={() => setSelected({ provider: m.provider, model: m.name })}
                  aria-pressed={isSelected(m)}
                >
                  <div style={styles.rowLeft}>
                    <span style={styles.modelName}>{m.display_name ?? m.name}</span>
                    {m.recommended_role && (
                      <Chip variant="soft" size="sm">{m.recommended_role}</Chip>
                    )}
                  </div>
                  <div style={styles.rowRight}>
                    {m.ram_estimate_mb && (
                      <span style={styles.ramBadge}>{m.ram_estimate_mb} MB</span>
                    )}
                    {isSelected(m) && <Check size={14} color="var(--color-accent)" />}
                  </div>
                </button>
              ))}
            </div>
          ))}
        </div>

        {/* Footer */}
        <div style={styles.footer}>
          <Button variant="ghost" onPress={onClose}>Cancel</Button>
          <Button
            variant="primary"
            onPress={confirm}
            isDisabled={!selected}
          >
            Select
          </Button>
        </div>
      </div>
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  overlay: {
    position: "fixed",
    inset: 0,
    background: "rgba(23, 22, 22, 0.45)",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    zIndex: 1000,
  },
  modal: {
    background: "var(--color-bg)",
    borderRadius: "var(--radius-xl)",
    boxShadow: "var(--shadow-canvas)",
    width: "560px",
    maxWidth: "calc(100vw - 32px)",
    maxHeight: "80vh",
    display: "flex",
    flexDirection: "column",
    overflow: "hidden",
  },
  header: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "var(--space-5) var(--space-5) var(--space-3)",
    flexShrink: 0,
  },
  title: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-md)",
    color: "var(--color-text)",
    margin: 0,
  },
  closeBtn: {
    background: "none",
    border: "none",
    cursor: "pointer",
    fontSize: "var(--text-sm)",
    color: "var(--color-text-tertiary)",
    padding: "var(--space-1)",
    lineHeight: 1,
  },
  searchWrap: {
    position: "relative",
    margin: "0 var(--space-5) var(--space-3)",
    flexShrink: 0,
  },
  searchIcon: {
    position: "absolute",
    left: "10px",
    top: "50%",
    transform: "translateY(-50%)",
    color: "var(--color-text-tertiary)",
    pointerEvents: "none",
  },
  searchInput: {
    width: "100%",
    height: "36px",
    paddingLeft: "32px",
    paddingRight: "var(--space-3)",
    border: "1px solid var(--color-border-strong)",
    borderRadius: "var(--radius-md)",
    fontSize: "var(--text-base)",
    fontFamily: "var(--font-body)",
    background: "var(--color-bg)",
    color: "var(--color-text)",
    boxSizing: "border-box",
    userSelect: "text",
  },
  body: {
    flex: 1,
    overflowY: "auto",
    padding: "0 var(--space-5)",
  },
  hint: {
    color: "var(--color-text-tertiary)",
    fontSize: "var(--text-sm)",
    margin: "var(--space-4) 0",
    textAlign: "center",
  },
  group: {
    marginBottom: "var(--space-3)",
  },
  groupLabel: {
    fontSize: "var(--text-xs)",
    fontWeight: 600,
    color: "var(--color-text-tertiary)",
    textTransform: "uppercase",
    letterSpacing: "0.06em",
    margin: "0 0 var(--space-1)",
    padding: "var(--space-1) 0",
  },
  row: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    width: "100%",
    padding: "var(--space-2) var(--space-3)",
    borderRadius: "var(--radius-md)",
    border: "1px solid transparent",
    background: "transparent",
    cursor: "pointer",
    textAlign: "left",
    gap: "var(--space-3)",
    transition: "background var(--transition-fast), border-color var(--transition-fast)",
  },
  rowSelected: {
    background: "var(--color-accent-soft)",
    borderColor: "var(--color-border-focus)",
  },
  rowLeft: {
    display: "flex",
    alignItems: "center",
    gap: "var(--space-2)",
    flex: 1,
    minWidth: 0,
  },
  rowRight: {
    display: "flex",
    alignItems: "center",
    gap: "var(--space-2)",
    flexShrink: 0,
  },
  modelName: {
    fontSize: "var(--text-base)",
    fontWeight: 500,
    color: "var(--color-text)",
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
  },
  ramBadge: {
    fontSize: "var(--text-xs)",
    fontFamily: "var(--font-mono)",
    color: "var(--color-text-tertiary)",
    background: "var(--color-border)",
    padding: "2px 6px",
    borderRadius: "var(--radius-xs)",
    whiteSpace: "nowrap",
  },
  footer: {
    display: "flex",
    justifyContent: "flex-end",
    gap: "var(--space-3)",
    padding: "var(--space-4) var(--space-5)",
    borderTop: "1px solid var(--color-border)",
    flexShrink: 0,
  },
};
