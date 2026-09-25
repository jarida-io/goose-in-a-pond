import { useState, useEffect, useCallback, useRef } from "react";
import { Plus, RefreshCw, Loader2, Download, Check, X, Store, AlertCircle } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card, Segment } from "./controls";
import { api } from "../../../api/PondApiClient";
import type { Extension, MarketplaceExtension, AddExtensionRequest, Settings } from "../../../api/types";

// ─── Icon path strings ────────────────────────────────────────
const WRENCH_PATH =
  "M14.7 6.3a4 4 0 0 1-5.4 5.4L4 17l3 3 5.3-5.3a4 4 0 0 0 5.4-5.4l-2.5 2.5-2.7-.3-.3-2.7z";
const CHEVD_PATH = "M6 9l6 6 6-6";
const STORE_PATH =
  "M3 9l1-5h16l1 5M4 9v11a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V9M4 9h16M9 21v-6h6v6";

// ─── Mock fallback (offline) ──────────────────────────────────
const MOCK_EXTENSIONS: Extension[] = [
  { name: "giap-weather",      kind: "builtin",         description: "Local & forecast weather",         tools: ["get_current_weather", "get_forecast"],                                  enabled: true,  status: "connected" },
  { name: "giap-knowledge",    kind: "builtin",         description: "Wikipedia, definitions, books, maths", tools: ["get_wikipedia_article", "compute_answer"], enabled: true, status: "connected" },
  { name: "giap-news",         kind: "builtin",         description: "Daily headlines & stories",        tools: ["search_news", "get_headlines"],                      enabled: false, status: undefined },
  { name: "giap-finance",      kind: "builtin",         description: "Crypto & market prices",           tools: ["convert_currency", "get_crypto_price"],              enabled: true,  status: "connected" },
  { name: "giap-memory",       kind: "builtin",         description: "Save & recall memories",           tools: ["save_memory", "recall_memories", "forget_memory"],                      enabled: true,  status: "connected" },
  { name: "giap-schedule",     kind: "builtin",         description: "Tasks & recurring schedules",      tools: ["create_schedule", "list_schedules", "delete_schedule"],                  enabled: true,  status: "connected" },
  { name: "giap-system",       kind: "builtin",         description: "Time, system info & notifications", tools: ["get_current_time", "get_system_info", "send_notification"],      enabled: true,  status: "connected" },
];

// ─── Determine status dot variant ────────────────────────────
function statusVariant(ext: Extension): "ok" | "off" | "err" {
  if (!ext.enabled) return "off";
  if (ext.status === "error") return "err";
  return "ok";
}

// ─── Normalize kind label ─────────────────────────────────────
function kindLabel(kind: string): string {
  if (kind === "streamable_http") return "http";
  return kind; // stdio, builtin, http
}

// ─── Skeleton row ─────────────────────────────────────────────
function SkeletonExtRow() {
  return (
    <div className="ext2" style={{ opacity: 0.45 }}>
      <div className="ext2__head">
        <span style={{ width: 8, height: 8, borderRadius: "50%", background: "#e2e8f0", flexShrink: 0 }} />
        <span className="ext2__icon" style={{ background: "#f1f5f9", borderRadius: 6, width: 28, height: 28 }} />
        <div className="ext2__info" style={{ flex: 1, gap: 4, display: "flex", flexDirection: "column" }}>
          <span style={{ display: "block", height: 11, width: 140, background: "#e2e8f0", borderRadius: 4 }} />
          <span style={{ display: "block", height: 10, width: 200, background: "#f1f5f9", borderRadius: 4 }} />
        </div>
      </div>
    </div>
  );
}

// ─── McpRow ───────────────────────────────────────────────────
interface McpRowProps {
  ext: Extension;
  toggling: boolean;
  onToggle: (name: string, next: boolean) => void;
}

function McpRow({ ext, toggling, onToggle }: McpRowProps) {
  const [open, setOpen] = useState(false);
  const sv = statusVariant(ext);
  return (
    <div className={`ext2${ext.enabled ? "" : " ext2--off"}`}>
      <div className="ext2__head">
        <span className={`ext2__status ext2__status--${sv}`} />
        <span className="ext2__icon">
          <HubIco d={WRENCH_PATH} size={15} color="#7C3AED" />
        </span>
        <div className="ext2__info">
          <div className="ext2__namerow">
            <span className="ext2__name">{ext.name}</span>
            <span className={`ext2__kind ext2__kind--${kindLabel(ext.kind)}`}>{kindLabel(ext.kind)}</span>
            {ext.tools.length > 0 && (
              <span className="ext2__count">{ext.tools.length} {ext.tools.length === 1 ? "tool" : "tools"}</span>
            )}
          </div>
          {ext.description && <span className="ext2__desc">{ext.description}</span>}
          {ext.status === "error" && ext.last_error && (
            <span className="ext2__desc" style={{ color: "#dc2626" }}>{ext.last_error}</span>
          )}
        </div>
        <button
          className="ext2__expand"
          type="button"
          onClick={() => setOpen((o) => !o)}
          aria-label={open ? "Collapse tools" : "Expand tools"}
          style={{ transform: open ? "rotate(180deg)" : "none" }}
          disabled={ext.tools.length === 0}
        >
          <HubIco d={CHEVD_PATH} size={16} color="var(--color-text-tertiary)" />
        </button>
        {/* Controlled toggle — reads ext.enabled directly */}
        <button
          className="htoggle"
          data-on={ext.enabled}
          onClick={() => onToggle(ext.name, !ext.enabled)}
          aria-pressed={ext.enabled}
          type="button"
          disabled={toggling}
          aria-label={`${ext.enabled ? "Disable" : "Enable"} ${ext.name}`}
        >
          {toggling
            ? <Loader2 size={10} style={{ animation: "spin 1s linear infinite", margin: "auto" }} />
            : <span className="htoggle__knob" />
          }
        </button>
      </div>
      {open && ext.tools.length > 0 && (
        <div className="ext2__tools">
          {ext.tools.map((t) => (
            <code key={t} className="ext2__tool">{t}</code>
          ))}
        </div>
      )}
    </div>
  );
}

// ─── Marketplace modal ────────────────────────────────────────
interface MarketplaceModalProps {
  installedNames: Set<string>;
  onClose: () => void;
  onInstalled: (ext: Extension) => void;
}

function MarketplaceModal({ installedNames, onClose, onInstalled }: MarketplaceModalProps) {
  const [items, setItems] = useState<MarketplaceExtension[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [installing, setInstalling] = useState<string | null>(null);
  const [justInstalled, setJustInstalled] = useState<Set<string>>(new Set());

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    api.listMarketplace()
      .then((list) => { if (!cancelled) setItems(list); })
      .catch((err) => { if (!cancelled) setError(String(err)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, []);

  // Close on Escape
  useEffect(() => {
    function onKey(e: KeyboardEvent) { if (e.key === "Escape") onClose(); }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  async function handleInstall(id: string) {
    setInstalling(id);
    try {
      const ext = await api.installMarketplaceExtension(id);
      // TODO: handle required_secrets before install
      setJustInstalled((prev) => new Set(prev).add(id));
      onInstalled(ext);
    } catch {
      // install error — silently mark done so user can retry
    } finally {
      setInstalling(null);
    }
  }

  const sorted = [...items].sort((a, b) => {
    if (a.featured && !b.featured) return -1;
    if (!a.featured && b.featured) return 1;
    return a.name.localeCompare(b.name);
  });

  return (
    <div
      className="ext2-mkt-backdrop"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
      role="dialog"
      aria-modal="true"
      aria-label="Browse marketplace"
    >
      <div className="ext2-mkt-panel">
        {/* Header */}
        <div className="ext2-mkt-header">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <HubIco d={STORE_PATH} size={16} color="#7C3AED" />
            <span style={{ fontWeight: 700, fontSize: 15, color: "var(--ink)" }}>Extension Marketplace</span>
          </div>
          <button
            className="ext2__expand"
            type="button"
            onClick={onClose}
            aria-label="Close marketplace"
            style={{ transform: "none" }}
          >
            <X size={16} color="var(--color-text-tertiary)" />
          </button>
        </div>

        {/* Body */}
        <div className="ext2-mkt-body">
          {loading && (
            <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "16px 0", color: "var(--color-text-tertiary)", fontSize: 13 }}>
              <Loader2 size={14} style={{ animation: "spin 1s linear infinite" }} />
              Loading marketplace…
            </div>
          )}
          {error && (
            <div style={{ display: "flex", alignItems: "center", gap: 6, padding: "12px 0", color: "#dc2626", fontSize: 13 }}>
              <AlertCircle size={14} />
              {error}
            </div>
          )}
          {!loading && !error && sorted.length === 0 && (
            <p style={{ margin: 0, fontSize: 13, color: "var(--color-text-tertiary)", padding: "12px 0" }}>
              No extensions available in the marketplace right now.
            </p>
          )}
          {!loading && !error && sorted.map((m) => {
            const isInstalled = installedNames.has(m.name.toLowerCase()) || installedNames.has(m.id.toLowerCase()) || justInstalled.has(m.id);
            const isInstalling = installing === m.id;
            return (
              <div key={m.id} className="ext2-mkt-card">
                <div className="ext2-mkt-card__head">
                  <span className="ext2__icon">
                    <HubIco d={WRENCH_PATH} size={14} color="#7C3AED" />
                  </span>
                  <div className="ext2__info" style={{ flex: 1 }}>
                    <div className="ext2__namerow">
                      <span className="ext2__name">{m.name}</span>
                      <span className={`ext2__kind ext2__kind--${kindLabel(m.kind)}`}>{kindLabel(m.kind)}</span>
                      {m.tools.length > 0 && (
                        <span className="ext2__count">{m.tools.length} tools</span>
                      )}
                      {m.featured && (
                        <span className="ext2__kind ext2__kind--builtin">featured</span>
                      )}
                    </div>
                    <span className="ext2__desc">{m.description}</span>
                  </div>
                  {isInstalled ? (
                    <span className="mrow__loaded" style={{ fontSize: 12 }}>
                      <Check size={11} color="#16A34A" strokeWidth={3} /> Installed
                    </span>
                  ) : (
                    <button
                      className="mrow__btn"
                      type="button"
                      disabled={isInstalling}
                      onClick={() => handleInstall(m.id)}
                      aria-label={`Install ${m.name}`}
                    >
                      {isInstalling
                        ? <Loader2 size={12} style={{ animation: "spin 1s linear infinite" }} />
                        : <Download size={12} />
                      }
                      {isInstalling ? "Installing…" : "Install"}
                    </button>
                  )}
                </div>
                <div style={{ fontSize: 11, color: "var(--color-text-tertiary)", paddingTop: 2, paddingLeft: 36 }}>
                  by {m.author} · {m.category}
                  {m.required_secrets.length > 0 && " · requires credentials"}
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

// ─── Add Server modal ─────────────────────────────────────────
interface AddServerModalProps {
  onClose: () => void;
  onAdded: (ext: Extension) => void;
}

type AddKind = "stdio" | "streamable_http";

function AddServerModal({ onClose, onAdded }: AddServerModalProps) {
  const [name, setName] = useState("");
  const [kind, setKind] = useState<AddKind>("stdio");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [uri, setUri] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Close on Escape
  useEffect(() => {
    function onKey(e: KeyboardEvent) { if (e.key === "Escape") onClose(); }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim()) { setError("Name is required."); return; }
    if (kind === "stdio" && !command.trim()) { setError("Command is required for stdio extensions."); return; }
    if (kind === "streamable_http" && !uri.trim()) { setError("URI is required for HTTP extensions."); return; }

    const req: AddExtensionRequest = {
      name: name.trim(),
      kind,
      ...(kind === "stdio" && {
        command: command.trim(),
        args: args.trim() ? args.split(",").map((a) => a.trim()).filter(Boolean) : [],
      }),
      ...(kind === "streamable_http" && { uri: uri.trim() }),
    };

    setSubmitting(true);
    setError(null);
    try {
      const ext = await api.addExtension(req);
      onAdded(ext);
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div
      className="ext2-mkt-backdrop"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
      role="dialog"
      aria-modal="true"
      aria-label="Add MCP server"
    >
      <div className="ext2-mkt-panel">
        {/* Header */}
        <div className="ext2-mkt-header">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <HubIco d={WRENCH_PATH} size={16} color="#EA580C" />
            <span style={{ fontWeight: 700, fontSize: 15, color: "var(--ink)" }}>Add MCP Server</span>
          </div>
          <button
            className="ext2__expand"
            type="button"
            onClick={onClose}
            aria-label="Close"
            style={{ transform: "none" }}
          >
            <X size={16} color="var(--color-text-tertiary)" />
          </button>
        </div>

        {/* Form */}
        <form className="ext2-mkt-body" onSubmit={handleSubmit} noValidate>
          {/* Name */}
          <div className="ext2-form-row">
            <label className="ext2-form-label" htmlFor="add-ext-name">
              Name <span style={{ color: "#dc2626" }}>*</span>
            </label>
            <input
              id="add-ext-name"
              className="ext2-form-input"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="my-mcp-server"
              autoComplete="off"
              disabled={submitting}
            />
          </div>

          {/* Kind */}
          <div className="ext2-form-row">
            <label className="ext2-form-label" htmlFor="add-ext-kind">Kind</label>
            <select
              id="add-ext-kind"
              className="ext2-form-input"
              value={kind}
              onChange={(e) => setKind(e.target.value as AddKind)}
              disabled={submitting}
            >
              <option value="stdio">stdio</option>
              <option value="streamable_http">streamable_http</option>
            </select>
          </div>

          {/* stdio fields */}
          {kind === "stdio" && (
            <>
              <div className="ext2-form-row">
                <label className="ext2-form-label" htmlFor="add-ext-command">
                  Command <span style={{ color: "#dc2626" }}>*</span>
                </label>
                <input
                  id="add-ext-command"
                  className="ext2-form-input"
                  value={command}
                  onChange={(e) => setCommand(e.target.value)}
                  placeholder="/usr/local/bin/my-mcp-server"
                  autoComplete="off"
                  disabled={submitting}
                />
              </div>
              <div className="ext2-form-row">
                <label className="ext2-form-label" htmlFor="add-ext-args">
                  Args <span style={{ color: "var(--color-text-tertiary)", fontWeight: 400 }}>(comma-separated)</span>
                </label>
                <input
                  id="add-ext-args"
                  className="ext2-form-input"
                  value={args}
                  onChange={(e) => setArgs(e.target.value)}
                  placeholder="--port, 8080"
                  autoComplete="off"
                  disabled={submitting}
                />
              </div>
            </>
          )}

          {/* http field */}
          {kind === "streamable_http" && (
            <div className="ext2-form-row">
              <label className="ext2-form-label" htmlFor="add-ext-uri">
                URI <span style={{ color: "#dc2626" }}>*</span>
              </label>
              <input
                id="add-ext-uri"
                className="ext2-form-input"
                value={uri}
                onChange={(e) => setUri(e.target.value)}
                placeholder="http://localhost:3001/mcp"
                type="url"
                autoComplete="off"
                disabled={submitting}
              />
            </div>
          )}

          {error && (
            <p style={{ margin: "4px 0 0", fontSize: 12, color: "#dc2626" }}>{error}</p>
          )}

          <div className="ext2-form-actions">
            <button
              type="button"
              className="ghost-btn"
              style={{ padding: "8px 14px", border: "1px solid var(--line)", borderRadius: 10, background: "var(--panel)", fontSize: 13, fontWeight: 600, color: "var(--ink)", cursor: "pointer", fontFamily: "inherit" }}
              onClick={onClose}
              disabled={submitting}
            >
              Cancel
            </button>
            <button
              type="submit"
              className="primary-btn"
              style={{ background: "#EA580C", boxShadow: "0 4px 12px rgba(234,88,12,.25)" }}
              disabled={submitting}
            >
              {submitting
                ? <Loader2 size={13} style={{ animation: "spin 1s linear infinite" }} />
                : <Plus size={13} color="#fff" strokeWidth={2.5} />
              }
              {submitting ? "Adding…" : "Add Server"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}


// ─── Tool loading mode ────────────────────────────────────────
// Tool schemas (~100 tokens each, re-sent every turn) dominate an on-device prompt. "Relevant" picks
// groups once per conversation so the prompt stays cacheable; Goose can load any other group mid-chat.
const TOOL_MODE_LABELS: Record<string, string> = {
  all: "All tools",
  relevant: "Only relevant",
  minimal: "Minimal",
};
const TOOL_MODE_VALUES: Record<string, string> = {
  "All tools": "all",
  "Only relevant": "relevant",
  Minimal: "minimal",
};

function ToolLoadingCard({ onFlash }: { onFlash: (text: string, ok?: boolean) => void }) {
  const [mode, setMode] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.getSettings()
      .then((s: Partial<Settings>) => { if (!cancelled) setMode(s.tool_selection_mode ?? "all"); })
      .catch(() => { if (!cancelled) setMode("all"); });
    return () => { cancelled = true; };
  }, []);

  async function pick(label: string) {
    const next = TOOL_MODE_VALUES[label] ?? "all";
    const prev = mode;
    setMode(next);
    try {
      await api.updateSettings({ tool_selection_mode: next } as Partial<Settings>);
      onFlash(
        next === "relevant"
          ? "New conversations will load only the tool groups they need."
          : next === "minimal"
            ? "New conversations start with no tools; Goose loads a group when it needs one."
            : "All tools will be sent every turn.");
    } catch (e) {
      setMode(prev);
      onFlash(`Could not save tool loading mode: ${String(e)}`, false);
    }
  }

  return (
    <Card title="Tool loading">
      <div className="mrow">
        <div className="mrow__info">
          <span className="mrow__name">Which tools go to the model</span>
          <span className="mrow__meta">
            Sending every tool costs a large slice of the prompt on small local models.
            &ldquo;Only relevant&rdquo; picks the groups a conversation needs when it starts;
            &ldquo;Minimal&rdquo; sends none at all. Goose can load any group itself if it
            needs one, so nothing becomes unreachable.
          </span>
        </div>
        {mode !== null && (
          <Segment
            options={["All tools", "Only relevant", "Minimal"]}
            value={TOOL_MODE_LABELS[mode] ?? "All tools"}
            onChange={pick}
          />
        )}
      </div>
    </Card>
  );
}

// ─── Main component ───────────────────────────────────────────
interface ExtensionsDetailProps {
  go: (route: string) => void;
}

export function ExtensionsDetail({ go }: ExtensionsDetailProps) {
  const [extensions, setExtensions] = useState<Extension[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [toggling, setToggling] = useState<string | null>(null);
  const [flash, setFlash] = useState<{ text: string; ok: boolean } | null>(null);
  const [showMarketplace, setShowMarketplace] = useState(false);
  const [showAddServer, setShowAddServer] = useState(false);
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
      const res = await api.listExtensions();
      const list = Array.isArray(res)
        ? (res as Extension[])
        : ((res as { extensions: Extension[] }).extensions ?? []);
      setExtensions(list);
    } catch (e) {
      console.warn("[ExtensionsDetail] API offline — using mock fallback:", e);
      setError("Could not reach the server. Showing offline view.");
      setExtensions(MOCK_EXTENSIONS);
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

  async function handleToggle(name: string, next: boolean) {
    // Optimistic update
    setExtensions((prev) => prev.map((e) => e.name === name ? { ...e, enabled: next } : e));
    setToggling(name);
    try {
      await api.toggleExtension(name, next);
      showFlash(`${name} ${next ? "enabled" : "disabled"}.`);
    } catch (e) {
      // Revert on failure
      setExtensions((prev) => prev.map((e) => e.name === name ? { ...e, enabled: !next } : e));
      showFlash(`Failed to toggle ${name}: ${String(e)}`, false);
    } finally {
      setToggling(null);
    }
  }

  function handleInstalled(ext: Extension) {
    setExtensions((prev) => {
      const exists = prev.some((e) => e.name === ext.name);
      return exists ? prev : [...prev, ext];
    });
    showFlash(`${ext.name} installed.`);
  }

  function handleAdded(ext: Extension) {
    setExtensions((prev) => {
      const exists = prev.some((e) => e.name === ext.name);
      return exists ? prev : [...prev, ext];
    });
    showFlash(`${ext.name} added.`);
  }

  const installedNames = new Set(extensions.map((e) => e.name.toLowerCase()));

  return (
    <>
      <DetailShell
        title="Extensions"
        subtitle="MCP servers that give Goose new abilities. All run locally."
        accent="#EA580C"
        onBack={() => go("settings")}
        headRight={
          <div style={{ display: "flex", gap: 8 }}>
            <button
              className="mrow__btn"
              type="button"
              onClick={loadData}
              aria-label="Refresh extensions"
              style={{ minWidth: 32, display: "flex", alignItems: "center", justifyContent: "center" }}
            >
              <RefreshCw size={13} strokeWidth={2} style={{ opacity: loading ? 0.4 : 1 }} />
            </button>
            <button
              className="ghost-btn"
              type="button"
              style={{
                padding: "9px 14px",
                border: "1px solid var(--line)",
                borderRadius: 12,
                background: "var(--panel)",
                display: "inline-flex",
                alignItems: "center",
                gap: 6,
                fontSize: 13,
                fontWeight: 700,
                color: "#7C3AED",
                cursor: "pointer",
                fontFamily: "inherit",
              }}
              onClick={() => setShowMarketplace(true)}
            >
              <HubIco d={STORE_PATH} size={14} color="#7C3AED" /> Browse
            </button>
            <button
              className="primary-btn"
              type="button"
              style={{ background: "#EA580C", boxShadow: "0 6px 16px rgba(234,88,12,.28)" }}
              onClick={() => setShowAddServer(true)}
            >
              <Plus size={15} color="#fff" strokeWidth={2.2} /> Add server
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

        <ToolLoadingCard onFlash={showFlash} />

        <Card>
          <div className="ext2list">
            {loading ? (
              <>
                <SkeletonExtRow />
                <SkeletonExtRow />
                <SkeletonExtRow />
                <SkeletonExtRow />
              </>
            ) : extensions.length === 0 ? (
              <div style={{ padding: "12px 0", fontSize: 13, color: "var(--color-text-tertiary)", textAlign: "center" }}>
                No extensions registered. Browse the marketplace or add a server.
              </div>
            ) : (
              extensions.map((ext) => (
                <McpRow
                  key={ext.name}
                  ext={ext}
                  toggling={toggling === ext.name}
                  onToggle={handleToggle}
                />
              ))
            )}
          </div>
        </Card>
      </DetailShell>

      {showMarketplace && (
        <MarketplaceModal
          installedNames={installedNames}
          onClose={() => setShowMarketplace(false)}
          onInstalled={handleInstalled}
        />
      )}

      {showAddServer && (
        <AddServerModal
          onClose={() => setShowAddServer(false)}
          onAdded={handleAdded}
        />
      )}
    </>
  );
}
