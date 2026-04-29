import { useState, useEffect, useRef, useCallback } from "react";
import { Button, Tabs, Card, CardContent, Chip, ProgressBar } from "@heroui/react";
import {
  Brain, Mic, Volume2, RefreshCw, Download, CheckCircle, XCircle,
  ChevronDown, ChevronUp, Search, Trash2, MessageSquare, Wrench, Play,
  ScanFace, Loader2, Puzzle,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState } from "../state/AppContext";
import type {
  ModelEntry, ModelActiveRoles, ModelMemoryStatus, ModelCapabilities,
  HfModel, HfModelFile, DownloadEntry,
  OllamaModel, LlamafileRelease, FaceModelsResponse,
} from "../api/types";
import { ApiError } from "../api/types";

// ── Capability detection from model name (frontend heuristic) ──

/** Infer capabilities from a model name string (mirrors backend ModelCapabilities::from_model_name). */
function inferCapabilities(name: string): Partial<ModelCapabilities> {
  const n = name.toLowerCase();
  const caps: Partial<ModelCapabilities> = {};

  // Thinking
  if (/gemma[-_]?4|qwen3|qwq|deepseek[-_]?r1/.test(n)) caps.thinking = true;
  // Vision
  if (/gemma[-_]?4|llava|bakllava|moondream/.test(n)) caps.vision = true;
  // Audio
  if (/gemma[-_]?4/.test(n) && /e[24]b/i.test(n)) caps.audio_input = true;
  // Context window
  if (/gemma[-_]?4/.test(n)) caps.context_window_tokens = 128_000;
  else if (/llama[-_]?3/.test(n)) caps.context_window_tokens = 8_192;
  else if (/qwen/.test(n)) caps.context_window_tokens = 32_768;
  else if (/mistral/.test(n)) caps.context_window_tokens = 32_768;

  return caps;
}

/** Compact capability badge list for a model name. */
function CapabilityBadges({ name }: { name: string }) {
  const caps = inferCapabilities(name);
  const badges: Array<{ label: string; title: string }> = [];
  if (caps.thinking) badges.push({ label: "Thinking", title: "Supports internal chain-of-thought reasoning" });
  if (caps.vision) badges.push({ label: "Vision", title: "Accepts image input (multimodal)" });
  if (caps.audio_input) badges.push({ label: "Audio", title: "Accepts raw audio input" });
  if (caps.context_window_tokens && caps.context_window_tokens > 8192)
    badges.push({ label: `${Math.round(caps.context_window_tokens / 1000)}k ctx`, title: `${caps.context_window_tokens.toLocaleString()} token context window` });

  if (badges.length === 0) return null;
  return (
    <>
      {badges.map(b => (
        <Chip key={b.label} size="sm" variant="soft" color="default" title={b.title}>{b.label}</Chip>
      ))}
    </>
  );
}

// ── Design constants ──────────────────────────────────────────

const CAT_COLOR = {
  llm:  "var(--color-role-chat)",
  asr:  "var(--color-role-asr)",
  tts:  "var(--color-role-tts)",
  face: "#3b82f6",
} as const;

// ── Active Roles Banner ───────────────────────────────────────

/** Maps role keys to role-chip CSS modifier classes */
const ROLE_CHIP_VARIANT: Record<string, string> = {
  chat: "secondary", tool: "success", asr: "primary", tts: "danger",
};

function ActiveRolesBanner({
  roles,
  memoryStatus,
  capabilities,
  onRefresh,
  loading,
  onNavigate,
}: {
  roles: ModelActiveRoles | null;
  memoryStatus: ModelMemoryStatus | null;
  capabilities: ModelCapabilities | null;
  onRefresh: () => void;
  loading: boolean;
  onNavigate?: (category: "llm" | "asr" | "tts") => void;
}) {
  const ROLE_DEFS: Array<{ key: "chat" | "asr" | "tts"; label: string; icon: React.ReactNode; category: "llm" | "asr" | "tts" }> = [
    { key: "chat", label: "Main LLM", icon: <MessageSquare size={10} />, category: "llm" },
    { key: "asr",  label: "ASR",      icon: <Mic size={10} />,           category: "asr" },
    { key: "tts",  label: "TTS",      icon: <Volume2 size={10} />,       category: "tts" },
  ];
  const toolModel = roles?.tool?.model;

  const memPct = memoryStatus && memoryStatus.total_mb > 0
    ? Math.round(((memoryStatus.total_mb - memoryStatus.available_for_llm_mb) / memoryStatus.total_mb) * 100)
    : null;

  return (
    <Card className="giap-card">
      <CardContent style={{ padding: "14px 16px", display: "flex", flexDirection: "column", gap: 12 }}>
        <div className="card-header" style={{ padding: 0 }}>
          <span className="card__label">Active Model Roles</span>
          <div className="card-header__right">
            {memoryStatus && memoryStatus.total_mb > 0 && (
              <div style={{ width: 160, display: "flex", flexDirection: "column", gap: 4 }}>
                <div className="mem-progress__label">
                  <span>Memory</span>
                  <span className="mem-progress__num">
                    {memoryStatus.available_for_llm_mb.toLocaleString()} / {memoryStatus.total_mb.toLocaleString()} MB
                  </span>
                </div>
                <ProgressBar
                  size="sm"
                  value={memPct ?? 0}
                  color={memPct != null && memPct > 85 ? "warning" : "accent"}
                  aria-label="Memory usage"
                >
                  <ProgressBar.Track><ProgressBar.Fill /></ProgressBar.Track>
                </ProgressBar>
              </div>
            )}
            <Button size="sm" variant="ghost" isIconOnly onPress={onRefresh} isDisabled={loading} aria-label="Refresh roles">
              <RefreshCw size={13} style={{ opacity: loading ? 0.4 : 1, transition: "opacity 0.2s" }} />
            </Button>
          </div>
        </div>

        <div className="role-grid">
          {ROLE_DEFS.map(({ key, label, icon, category }) => {
            const a = roles?.[key];
            const isSet = !!(a?.provider && a?.model);
            const variant = ROLE_CHIP_VARIANT[key] ?? "secondary";
            return (
              <div
                key={key}
                className={`role-chip role-chip--${variant}`}
                onClick={!isSet && onNavigate ? () => onNavigate(category) : undefined}
                style={{ cursor: !isSet && onNavigate ? "pointer" : "default" }}
                title={!isSet ? `Click to set ${label} model` : undefined}
              >
                <div className="role-chip__bar" />
                <div className="role-chip__body">
                  <div className="role-chip__head">
                    {icon}
                    <span className="role-chip__role">{label}</span>
                  </div>
                  <span style={{
                    fontSize: "var(--text-xs)", fontFamily: "var(--font-mono)",
                    color: isSet ? "var(--fg)" : "var(--grey-500)", fontStyle: isSet ? "normal" : "italic",
                    whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", display: "block",
                  }}>
                    {isSet ? `${a!.provider} / ${a!.model}` : "Not set"}
                  </span>
                </div>
              </div>
            );
          })}
          {/* Tool Caller chip */}
          <div
            className="role-chip role-chip--success"
            onClick={!toolModel && onNavigate ? () => onNavigate("llm") : undefined}
            style={{ cursor: !toolModel && onNavigate ? "pointer" : "default" }}
            title={!toolModel ? "Click to set a tool-calling specialist model" : undefined}
          >
            <div className="role-chip__bar" />
            <div className="role-chip__body">
              <div className="role-chip__head">
                <Puzzle size={10} />
                <span className="role-chip__role">Tool Caller</span>
              </div>
              <span style={{
                fontSize: "var(--text-xs)", fontFamily: "var(--font-mono)",
                color: toolModel ? "var(--fg)" : "var(--grey-500)", fontStyle: toolModel ? "normal" : "italic",
                whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", display: "block",
              }}>
                {toolModel ?? "Not set"}
              </span>
            </div>
          </div>
        </div>

        {/* Active model capabilities */}
        {capabilities && (capabilities.thinking || capabilities.vision || capabilities.audio_input || capabilities.context_window_tokens > 4096) && (
          <div style={{
            display: "flex", gap: 6, flexWrap: "wrap", alignItems: "center",
            padding: "6px 0 0", borderTop: "1px solid var(--grey-200)",
          }}>
            <span style={{ fontSize: "var(--text-xs)", color: "var(--grey-500)", marginRight: 4 }}>
              Model features:
            </span>
            {capabilities.thinking && <Chip size="sm" variant="soft" color="accent">Thinking</Chip>}
            {capabilities.vision && <Chip size="sm" variant="soft" color="accent">Vision</Chip>}
            {capabilities.audio_input && <Chip size="sm" variant="soft" color="accent">Audio</Chip>}
            {capabilities.structured_output && <Chip size="sm" variant="soft" color="accent">Structured Output</Chip>}
            {capabilities.context_window_tokens > 4096 && (
              <Chip size="sm" variant="soft" color="accent">
                {Math.round(capabilities.context_window_tokens / 1000)}k context
              </Chip>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

// ── Download Progress ─────────────────────────────────────────

/** Format bytes as human-readable size. */
function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function DownloadProgress({ downloads, onScanModels }: { downloads: DownloadEntry[]; onScanModels: () => void }) {
  if (downloads.length === 0) return null;
  const allDone = downloads.every((d) => d.status === "done" || d.status === "error");
  return (
    <div className="dl-progress">
      <div className="dl-progress__header">
        <span className="dl-progress__title">Downloads</span>
        {allDone && (
          <Button variant="outline" size="sm" onPress={onScanModels}>
            <RefreshCw size={12} /> Scan & index
          </Button>
        )}
      </div>
      {downloads.map((d) => {
        const pct = d.total_bytes ? Math.round((d.downloaded_bytes / d.total_bytes) * 100) : 0;
        const sizeLabel = d.total_bytes
          ? `${fmtBytes(d.downloaded_bytes)} / ${fmtBytes(d.total_bytes)}`
          : fmtBytes(d.downloaded_bytes);
        return (
          <div key={d.filename} className="dl-progress__item">
            <div className="dl-progress__item-header">
              <code className="dl-progress__filename">{d.filename}</code>
              <span className={`dl-progress__status dl-progress__status--${d.status}`}>
                {d.status === "done" ? "Complete" : d.status === "error" ? (d.error ?? "Error") : `${pct}% — ${sizeLabel}`}
              </span>
            </div>
            <div className="dl-progress__track">
              <div
                className={`dl-progress__fill dl-progress__fill--${d.status}`}
                style={{ width: d.status === "done" ? "100%" : `${pct}%` }}
              />
            </div>
          </div>
        );
      })}
    </div>
  );
}

// ── Shared ModelList ──────────────────────────────────────────

type RoleKey = "chat" | "tool" | "asr" | "tts";

const ROLE_LABELS: Record<RoleKey, string> = {
  chat: "Main LLM", tool: "Tool Caller", asr: "ASR", tts: "TTS",
};

function ModelList({
  models,
  loading,
  error,
  activeRoles,
  availableRoles,
  onActivate,
  onDelete,
  emptyMessage,
}: {
  models: ModelEntry[];
  loading: boolean;
  error: string | null;
  activeRoles: ModelActiveRoles | null;
  availableRoles: RoleKey[];
  onActivate: (provider: string, name: string, role: string) => void;
  onDelete: (provider: string, name: string) => void;
  emptyMessage: string;
}) {
  const state = useAppState();

  if (loading) return <p style={hint}>Loading…</p>;
  if (error) return <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>;
  if (models.length === 0) return <p style={hint}>{emptyMessage}</p>;

  function isRoleActive(m: ModelEntry, role: RoleKey) {
    if (role === "tool") {
      return activeRoles?.tool?.model === m.name;
    }
    const a = activeRoles?.[role];
    if (!a) return false;
    return a.provider === m.provider && a.model === m.name;
  }

  function activeRolesFor(m: ModelEntry): RoleKey[] {
    return availableRoles.filter((r) => isRoleActive(m, r));
  }

  return (
    <div className="models-list">
      {models.map((m) => {
        const activeFor = activeRolesFor(m);
        const isAnyActive = activeFor.length > 0;

        return (
          <div
            key={m.id}
            className={`giap-model-row${isAnyActive ? " is-active" : ""}`}
          >
            <div>
              <div className="giap-model-row__title-row">
                <span className="giap-model-row__name">{m.display_name ?? m.name}</span>
                {m.ram_estimate_mb && (
                  <Chip size="sm" variant="soft">{m.ram_estimate_mb} MB</Chip>
                )}
                {m.recommended_role && (
                  <Chip size="sm" variant="soft">{m.recommended_role}</Chip>
                )}
                {activeFor.map((r) => (
                  <Chip key={r} size="sm" color="accent" variant="soft">{ROLE_LABELS[r]}</Chip>
                ))}
                <CapabilityBadges name={m.name} />
              </div>
              <div className="giap-model-row__file"><code>{m.provider} / {m.name}</code></div>
            </div>
            <div className="giap-model-row__actions">
              <div className="role-select">
                {availableRoles.map((role) => {
                  const active = isRoleActive(m, role);
                  return (
                    <Button
                      key={role}
                      size="sm"
                      variant={active ? "secondary" : "outline"}
                      onPress={() => onActivate(m.provider, m.name, role)}
                      isDisabled={!state.serverOnline}
                      aria-label={`Set ${m.name} as ${role} model`}
                    >
                      {active && <CheckCircle size={11} />}
                      {ROLE_LABELS[role]}
                    </Button>
                  );
                })}
              </div>
              <Button
                size="sm"
                variant="outline"
                isIconOnly
                className="giap-model-row__trash"
                onPress={() => onDelete(m.provider, m.name)}
                isDisabled={!state.serverOnline}
                aria-label={`Delete ${m.name}`}
              >
                <Trash2 size={12} />
              </Button>
            </div>
          </div>
        );
      })}
    </div>
  );
}

// ── Browse HuggingFace Accordion ──────────────────────────────

function BrowseHfAccordion({ onDownloadStarted }: { onDownloadStarted: () => void }) {
  const state = useAppState();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("gemma");
  const [searching, setSearching] = useState(false);
  const [results, setResults] = useState<HfModel[]>([]);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [expandedRepo, setExpandedRepo] = useState<string | null>(null);
  const [repoFiles, setRepoFiles] = useState<Record<string, HfModelFile[]>>({});
  const [loadingFiles, setLoadingFiles] = useState<string | null>(null);
  const [downloadingFile, setDownloadingFile] = useState<string | null>(null);
  const [dlMsg, setDlMsg] = useState<string | null>(null);

  const search = useCallback(async () => {
    if (!query.trim()) return;
    setSearching(true); setSearchError(null); setResults([]); setExpandedRepo(null);
    try { setResults((await api.searchGgufModels(query.trim())).models ?? []); }
    catch (e) { setSearchError(String(e)); }
    finally { setSearching(false); }
  }, [query]);

  async function browseFiles(repoId: string) {
    if (expandedRepo === repoId) { setExpandedRepo(null); return; }
    setExpandedRepo(repoId);
    if (repoFiles[repoId]) return;
    setLoadingFiles(repoId);
    try { setRepoFiles((p) => ({ ...p, [repoId]: [] }));
      const res = await api.listHfModelFiles(repoId);
      setRepoFiles((p) => ({ ...p, [repoId]: res.files ?? [] }));
    } catch { /* empty */ } finally { setLoadingFiles(null); }
  }

  async function startDownload(file: HfModelFile) {
    setDownloadingFile(file.filename); setDlMsg(null);
    try {
      const res = await api.downloadModelFromUrl(file.url, "gguf", file.filename);
      setDlMsg(res.status === "already_downloaded" ? `${file.filename} already downloaded.` : `Download started: ${file.filename}`);
      onDownloadStarted();
    } catch (e) { setDlMsg(`Error: ${String(e)}`); }
    finally { setDownloadingFile(null); }
  }

  return (
    <div className="github-accordion">
      <button className="acc-title" style={accordionSt.header} onClick={() => { setOpen((v) => !v); if (!open && results.length === 0) search(); }}>
        <span style={accordionSt.headerLabel}><Download size={13} /> Browse HuggingFace</span>
        {open ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
      </button>

      {open && (
        <div style={accordionSt.body}>
          <div style={accordionSt.searchRow}>
            <div style={{ position: "relative", flex: 1 }}>
              <Search size={13} style={{ position: "absolute", left: 9, top: "50%", transform: "translateY(-50%)", color: "var(--grey-500)", pointerEvents: "none" }} />
              <input
                style={accordionSt.searchInput}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && search()}
                placeholder="Search GGUF models…"
                aria-label="Search HuggingFace GGUF models"
              />
            </div>
            <Button variant="secondary" size="sm" onPress={search} isDisabled={searching || !state.serverOnline}>
              {searching ? "…" : "Search"}
            </Button>
          </div>

          {searchError && <p style={{ ...hint, color: "var(--color-destructive)" }}>{searchError}</p>}
          {dlMsg && <p style={{ ...hint, color: dlMsg.startsWith("Error") ? "var(--color-destructive)" : "var(--color-success)" }}>{dlMsg}</p>}

          {!searching && results.length === 0 && !searchError && (
            <p className="gh-empty"><Search size={14} /> No results. Search for &quot;gemma&quot;, &quot;llama&quot;, or &quot;mistral&quot;.</p>
          )}

          <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-1)" }}>
            {results.map((model) => {
              const isExpanded = expandedRepo === model.id;
              const files = repoFiles[model.id] ?? [];
              return (
                <div key={model.id} style={accordionSt.repoCard}>
                  <div style={accordionSt.repoHeader}>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <a href={model.url} target="_blank" rel="noopener noreferrer" style={accordionSt.repoId}>{model.id}</a>
                      <div style={{ display: "flex", flexWrap: "wrap" as const, gap: "4px", marginTop: "2px" }}>
                        <Chip size="sm" variant="soft">↓ {model.downloads?.toLocaleString() ?? "?"}</Chip>
                        <Chip size="sm" variant="soft">♥ {model.likes ?? "?"}</Chip>
                        {model.tags?.slice(0, 2).map((tag) => <Chip key={tag} size="sm" variant="soft">{tag}</Chip>)}
                      </div>
                    </div>
                    <Button variant="outline" size="sm" onPress={() => browseFiles(model.id)} isDisabled={!state.serverOnline}>
                      {loadingFiles === model.id ? "…" : isExpanded ? "Hide" : "Files"}
                    </Button>
                  </div>
                  {isExpanded && (
                    <div style={accordionSt.fileList}>
                      {loadingFiles === model.id && <p style={{ ...hint, padding: "var(--space-2) var(--space-3)" }}>Loading files…</p>}
                      {!loadingFiles && files.length === 0 && <p className="gh-empty" style={{ padding: "var(--space-2) var(--space-3)" }}>No .gguf files in this repo.</p>}
                      {files.map((file) => (
                        <div key={file.filename} style={accordionSt.fileRow}>
                          <code className="giap-model-row__name">{file.filename}</code>
                          {file.size_mb != null && <Chip size="sm" variant="soft">{file.size_mb.toLocaleString()} MB</Chip>}
                          <Button
                            variant="secondary" size="sm"
                            onPress={() => startDownload(file)}
                            isDisabled={downloadingFile === file.filename || !state.serverOnline}
                          >
                            <Download size={11} /> {downloadingFile === file.filename ? "Starting…" : "Download"}
                          </Button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

// ── Browse Llamafile GitHub Accordion ─────────────────────────

function BrowseGithubAccordion({ onDownloadStarted }: { onDownloadStarted: () => void }) {
  const state = useAppState();
  const [open, setOpen] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [releases, setReleases] = useState<LlamafileRelease[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [dlMsg, setDlMsg] = useState<string | null>(null);
  const [downloadingFile, setDownloadingFile] = useState<string | null>(null);

  async function load() {
    if (loaded) return;
    setLoading(true); setError(null);
    try {
      const res = await api.searchLlamafileModels();
      setReleases(res.models ?? []);
      setLoaded(true);
    } catch (e) { setError(String(e)); }
    finally { setLoading(false); }
  }

  async function startDownload(rel: LlamafileRelease) {
    setDownloadingFile(rel.name); setDlMsg(null);
    try {
      const res = await api.downloadModelFromUrl(rel.download_url, "llamafile", rel.name);
      setDlMsg(res.status === "already_downloaded" ? `${rel.name} already downloaded.` : `Download started: ${rel.name}`);
      onDownloadStarted();
    } catch (e) { setDlMsg(`Error: ${String(e)}`); }
    finally { setDownloadingFile(null); }
  }

  return (
    <div className="github-accordion">
      <button className="acc-title" style={accordionSt.header} onClick={() => { setOpen((v) => !v); if (!open) load(); }}>
        <span style={accordionSt.headerLabel}><Download size={13} /> Browse GitHub Releases</span>
        {open ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
      </button>
      {open && (
        <div style={accordionSt.body}>
          {loading && <p style={hint}>Fetching releases…</p>}
          {error && <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>}
          {dlMsg && <p style={{ ...hint, color: dlMsg.startsWith("Error") ? "var(--color-destructive)" : "var(--color-success)" }}>{dlMsg}</p>}
          {!loading && releases.length === 0 && !error && <p className="gh-empty"><Download size={14} /> No llamafile releases found.</p>}
          <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-1)" }}>
            {releases.map((rel) => (
              <div key={rel.name} style={accordionSt.fileRow}>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <code className="giap-model-row__name">{rel.name}</code>
                  <div style={{ display: "flex", gap: "4px", marginTop: "2px" }}>
                    <Chip size="sm" variant="soft">{rel.tag}</Chip>
                    {rel.size_mb != null && <Chip size="sm" variant="soft">{rel.size_mb.toLocaleString()} MB</Chip>}
                  </div>
                </div>
                <Button
                  variant="secondary" size="sm"
                  onPress={() => startDownload(rel)}
                  isDisabled={downloadingFile === rel.name || !state.serverOnline}
                >
                  <Download size={11} /> {downloadingFile === rel.name ? "Starting…" : "Download"}
                </Button>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

const accordionSt: Record<string, React.CSSProperties> = {
  root: {
    border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-md)",
    overflow: "hidden",
    background: "var(--color-bg)",
  },
  header: {
    display: "flex", alignItems: "center", justifyContent: "space-between",
    width: "100%", background: "rgba(23,22,22,0.02)", border: "none",
    padding: "var(--space-3) var(--space-4)", cursor: "pointer",
    color: "var(--color-text-secondary)", transition: "background var(--transition-fast)",
  },
  headerLabel: {
    display: "flex", alignItems: "center", gap: "var(--space-2)",
    fontSize: "var(--text-sm)", fontWeight: 600,
  },
  body: {
    display: "flex", flexDirection: "column" as const, gap: "var(--space-2)",
    padding: "var(--space-3) var(--space-4)",
    borderTop: "1px solid var(--color-border)",
  },
  searchRow: { display: "flex", gap: "var(--space-2)", alignItems: "center" },
  searchInput: {
    width: "100%", height: "32px", paddingLeft: "32px", paddingRight: "var(--space-3)",
    border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)",
    fontSize: "var(--text-sm)", fontFamily: "var(--font-body)",
    background: "var(--color-bg)", color: "var(--color-text)", outline: "none",
    boxSizing: "border-box" as const,
  },
  repoCard: {
    border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)",
    overflow: "hidden", background: "rgba(23,22,22,0.01)",
  },
  repoHeader: {
    display: "flex", alignItems: "flex-start", justifyContent: "space-between",
    gap: "var(--space-3)", padding: "var(--space-2) var(--space-3)",
  },
  repoId: {
    fontWeight: 600, fontSize: "var(--text-xs)", color: "var(--color-accent)",
    textDecoration: "none", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const,
    display: "block",
  },
  fileList: { borderTop: "1px solid var(--color-border)", background: "rgba(23,22,22,0.02)" },
  fileRow: {
    display: "flex", alignItems: "center", justifyContent: "space-between",
    gap: "var(--space-3)", padding: "var(--space-2) var(--space-3)",
    borderBottom: "1px solid var(--color-border)",
  },
};

// ── Ollama Panel ──────────────────────────────────────────────

function OllamaPanel({
  models,
  modelsLoading,
  modelsError,
  activeRoles,
  onActivate,
  onDelete,
}: {
  models: ModelEntry[];
  modelsLoading: boolean;
  modelsError: string | null;
  activeRoles: ModelActiveRoles | null;
  onActivate: (provider: string, name: string, role: string) => void;
  onDelete: (provider: string, name: string) => void;
}) {
  const state = useAppState();
  const [ollamaModels, setOllamaModels] = useState<OllamaModel[]>([]);
  const [ollamaError, setOllamaError] = useState<string | null>(null);
  const [ollamaLoading, setOllamaLoading] = useState(true);
  const [pullInput, setPullInput] = useState("");
  const [pulling, setPulling] = useState(false);
  const [pullMsg, setPullMsg] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  async function loadOllamaModels() {
    setOllamaLoading(true);
    try {
      const res = await api.listOllamaModels();
      setOllamaModels(res.models ?? []);
      setOllamaError(res.error ?? null);
    } catch (e) { setOllamaError(String(e)); }
    finally { setOllamaLoading(false); }
  }

  useEffect(() => {
    loadOllamaModels();
    return () => { if (pollRef.current) clearInterval(pollRef.current); };
  }, []);

  async function handlePull() {
    if (!pullInput.trim()) return;
    setPulling(true); setPullMsg(null);
    try {
      await api.pullOllamaModel(pullInput.trim());
      setPullMsg(`Pulling "${pullInput.trim()}"… This may take a few minutes.`);
      // Poll for new models every 5s while pulling
      if (!pollRef.current) {
        pollRef.current = setInterval(loadOllamaModels, 5000);
        setTimeout(() => { if (pollRef.current) { clearInterval(pollRef.current); pollRef.current = null; } }, 120_000);
      }
    } catch (e) { setPullMsg(`Error: ${String(e)}`); }
    finally { setPulling(false); }
  }

  const isRunning = !ollamaError || !ollamaError.toLowerCase().includes("not running");
  const ollamaModelEntries: ModelEntry[] = ollamaModels.map((om) => ({
    id: `ollama/${om.name}`,
    provider: "ollama",
    name: om.name,
    display_name: om.name,
    is_active: false,
    ram_estimate_mb: om.size != null ? Math.round(om.size / (1024 * 1024)) : undefined,
  }));

  const allOllamaModels = [
    ...models, // from registry (may include ollama entries already scanned)
    ...ollamaModelEntries.filter((om) => !models.some((m) => m.name === om.name && m.provider === "ollama")),
  ];

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      {/* Status bar */}
      <div className={`seg-banner ${isRunning ? "seg-banner--info" : "seg-banner--warn"}`}>
        <span className={`giap-status-dot ${ollamaLoading ? "" : isRunning ? "giap-status-dot--online" : "giap-status-dot--offline"}`} />
        <span style={{ flex: 1 }}>
          {ollamaLoading ? "Checking Ollama…" : isRunning ? `Ollama running — ${ollamaModels.length} model${ollamaModels.length !== 1 ? "s" : ""}` : "Ollama not running"}
        </span>
        {!isRunning && !ollamaLoading && (
          <Button variant="outline" size="sm" onPress={() => {
            setPullMsg("Run `ollama serve` in a terminal to start Ollama, then refresh.");
          }}>
            <Play size={12} /> How to start
          </Button>
        )}
        <Button variant="ghost" size="sm" isIconOnly onPress={loadOllamaModels} isDisabled={ollamaLoading}>
          <RefreshCw size={12} />
        </Button>
      </div>

      {pullMsg && <p style={{ ...hint, color: pullMsg.startsWith("Error") ? "var(--color-destructive)" : pullMsg.startsWith("Run") ? "var(--color-text-secondary)" : "var(--color-success)" }}>{pullMsg}</p>}

      {/* Pull row */}
      <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
        <input
          style={pullInputSt}
          value={pullInput}
          onChange={(e) => setPullInput(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handlePull()}
          placeholder="Pull model, e.g. llama3.2:3b"
          aria-label="Ollama model to pull"
          disabled={!state.serverOnline || !isRunning}
        />
        <Button
          variant="secondary" size="sm"
          onPress={handlePull}
          isDisabled={!pullInput.trim() || pulling || !state.serverOnline || !isRunning}
        >
          {pulling ? "Starting…" : "Pull model"}
        </Button>
      </div>

      {/* Model list */}
      <ModelList
        models={allOllamaModels}
        loading={modelsLoading && ollamaLoading}
        error={modelsError}
        activeRoles={activeRoles}
        availableRoles={["chat"]}
        onActivate={onActivate}
        onDelete={onDelete}
        emptyMessage={isRunning ? "No Ollama models found. Pull a model above." : "Ollama is not running. Start it to see available models."}
      />
    </div>
  );
}

const pullInputSt: React.CSSProperties = {
  flex: 1, height: "34px", padding: "0 12px",
  border: "1px solid var(--grey-200)", borderRadius: "8px",
  fontSize: "var(--text-sm)", fontFamily: "var(--font-body)",
  background: "#fff", color: "var(--fg)", outline: "none",
};

// ── LLM Tab ───────────────────────────────────────────────────

type LlmProvider = "gguf" | "llamafile" | "ollama";

function LlmTab({
  models,
  modelsLoading,
  modelsError,
  activeRoles,
  onActivate,
  onDelete,
  onDownloadStarted,
  onScanModels,
}: {
  models: ModelEntry[];
  modelsLoading: boolean;
  modelsError: string | null;
  activeRoles: ModelActiveRoles | null;
  onActivate: (provider: string, name: string, role: string) => void;
  onDelete: (provider: string, name: string) => void;
  onDownloadStarted: () => void;
  onScanModels: () => void;
}) {
  const [provider, setProvider] = useState<LlmProvider>("gguf");
  const PROVIDERS: Array<{ key: LlmProvider; label: string }> = [
    { key: "gguf", label: "GGUF" },
    { key: "llamafile", label: "Llamafile" },
    { key: "ollama", label: "Ollama" },
  ];

  const ggufModels = models.filter((m) => m.provider === "gguf");
  const llamafileModels = models.filter((m) => m.provider === "llamafile");
  const ollamaRegistryModels = models.filter((m) => m.provider === "ollama");

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      {/* Provider sub-tabs */}
      <div className="seg-toolbar">
        <Tabs
          selectedKey={provider}
          onSelectionChange={(k) => setProvider(String(k) as LlmProvider)}
        >
          <Tabs.ListContainer>
            <Tabs.List aria-label="LLM providers">
              {PROVIDERS.map(({ key, label }) => (
                <Tabs.Tab key={key} id={key} onClick={() => setProvider(key as LlmProvider)}>
                  <Tabs.Indicator />
                  {label}
                </Tabs.Tab>
              ))}
            </Tabs.List>
          </Tabs.ListContainer>
        </Tabs>
        <div className="seg-toolbar__count">
          <span>{ggufModels.length + llamafileModels.length + ollamaRegistryModels.length} models</span>
        </div>
      </div>

      {/* GGUF panel */}
      {provider === "gguf" && (
        <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
          <ModelList
            models={ggufModels}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["chat", "tool"]}
            onActivate={onActivate}
            onDelete={onDelete}
            emptyMessage="No GGUF models found. Download one below."
          />
          <BrowseHfAccordion onDownloadStarted={onDownloadStarted} />
        </div>
      )}

      {/* Llamafile panel */}
      {provider === "llamafile" && (
        <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
          <ModelList
            models={llamafileModels}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["chat"]}
            onActivate={onActivate}
            onDelete={onDelete}
            emptyMessage="No Llamafile models found. Download one below."
          />
          <BrowseGithubAccordion onDownloadStarted={onDownloadStarted} />
        </div>
      )}

      {/* Ollama panel */}
      {provider === "ollama" && (
        <OllamaPanel
          models={ollamaRegistryModels}
          modelsLoading={modelsLoading}
          modelsError={modelsError}
          activeRoles={activeRoles}
          onActivate={onActivate}
          onDelete={onDelete}
        />
      )}
    </div>
  );
}

// llmSt removed — provider pills now use HeroUI Tabs + seg-toolbar CSS classes

// ── Category Tabs ─────────────────────────────────────────────

type Category = "llm" | "asr" | "tts" | "face";

const CATEGORIES: Array<{ key: Category; label: string; icon: React.ReactNode; color: string }> = [
  { key: "llm", label: "LLM", icon: <Brain size={14} />, color: CAT_COLOR.llm },
  { key: "asr", label: "ASR", icon: <Mic size={14} />, color: CAT_COLOR.asr },
  { key: "tts", label: "TTS", icon: <Volume2 size={14} />, color: CAT_COLOR.tts },
  { key: "face", label: "Face", icon: <ScanFace size={14} />, color: "#3b82f6" },
];

// ── Face Recognition Panel ───────────────────────────────────
//
// Read-only status for the face models (ArcFace R50 + SCRFD 10G +
// Silent-Face PAD). pond-server downloads them automatically on first
// boot when built with `--features face-onnx`, so there is no per-model
// "Download" button — operators just watch progress here. When the
// feature is disabled the card surfaces the rebuild instruction.
function FacePanel() {
  const [data, setData]       = useState<FaceModelsResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError]     = useState<string | null>(null);

  const reload = useCallback(async () => {
    setLoading(true); setError(null);
    try { setData(await api.listFaceModels()); }
    catch (e) { setError(String(e)); }
    finally { setLoading(false); }
  }, []);

  useEffect(() => { reload(); }, [reload]);

  const installed = data?.models.filter(m => m.downloaded).length ?? 0;
  const total     = data?.models.length ?? 0;

  const fpHint: React.CSSProperties = {
    color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0,
  };
  const fpBadge: React.CSSProperties = {
    fontSize: "var(--text-xs)", fontFamily: "var(--font-mono)",
    color: "var(--color-text-tertiary)", background: "rgba(23,22,22,0.05)",
    padding: "1px 6px", borderRadius: "var(--radius-xs, 4px)", flexShrink: 0,
  };
  const fpRow: React.CSSProperties = {
    display: "flex", alignItems: "center", gap: "var(--space-3)",
    padding: "var(--space-3) var(--space-4)",
    background: "var(--color-bg)", border: "1px solid var(--color-border)",
    borderLeft: "4px solid", transition: "border-color 120ms",
  };
  const fpInlineCode: React.CSSProperties = {
    fontFamily: "var(--font-mono)", fontSize: "0.85em",
    background: "rgba(23,22,22,0.06)", padding: "1px 5px", borderRadius: 4,
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
      <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
        <ScanFace size={14} style={{ color: CAT_COLOR.face }} />
        <span style={{
          fontFamily: "var(--font-display)", fontWeight: 700,
          fontSize: "var(--text-sm)", textTransform: "uppercase" as const,
          letterSpacing: "0.06em", color: CAT_COLOR.face,
        }}>
          Face Recognition
        </span>
        {data && (
          <span style={fpBadge}>
            {data.feature_enabled ? `${installed}/${total} ready` : "feature disabled"}
          </span>
        )}
        <div style={{ flex: 1 }} />
        <Button variant="ghost" size="sm" onPress={reload} isDisabled={loading}>
          <RefreshCw size={12} /> Refresh
        </Button>
      </div>

      {loading && <p style={fpHint}>Loading...</p>}
      {error && <p style={{ ...fpHint, color: "var(--color-destructive)" }}>{error}</p>}

      {data && !data.feature_enabled && (
        <p style={fpHint}>
          Face recognition is disabled in this build. Rebuild pond-server with
          {" "}<code style={fpInlineCode}>--features face-onnx</code>{" "}
          to enable per-user identification.
        </p>
      )}

      {data && data.models.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-2)" }}>
          {data.models.map(m => (
            <div
              key={m.name}
              style={{ ...fpRow, borderLeftColor: m.downloaded ? CAT_COLOR.face : "transparent" }}
            >
              <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 2, minWidth: 0 }}>
                <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)", flexWrap: "wrap" as const }}>
                  <span style={{
                    fontWeight: 600, fontSize: "var(--text-sm)", color: "var(--color-text)",
                    overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const,
                  }}>
                    {m.label}
                  </span>
                  <span style={fpBadge}>{m.role}</span>
                  {m.downloaded ? (
                    <span style={{ ...fpBadge, color: "var(--color-success)" }}>
                      {m.size_mb != null ? `${m.size_mb} MB` : "ready"}
                    </span>
                  ) : (
                    <span style={{ ...fpBadge, color: "#e5a000" }}>
                      missing · ~{m.expected_mb} MB
                    </span>
                  )}
                </div>
                <span style={{
                  fontSize: "var(--text-xs)", fontFamily: "var(--font-mono)",
                  color: "var(--color-text-tertiary)",
                }}>
                  {m.name}
                </span>
              </div>
            </div>
          ))}
        </div>
      )}

      {data?.models_dir && (
        <p style={{ ...fpHint, fontFamily: "var(--font-mono)", fontSize: "var(--text-xs)", opacity: 0.6 }}>
          {data.models_dir}
        </p>
      )}

      <p style={fpHint}>
        Models auto-download on first server boot. The buffalo_l fallback zip ships
        ArcFace R50 + SCRFD 10G; Glint-R100 + SCRFD 34G are fetched separately when
        their mirrors are reachable. Once installed, use the <strong>Faces</strong>
        section (left sidebar) to enroll household members.
      </p>
    </div>
  );
}

// ── Memory Status Bar ─────────────────────────────────────────

function MemoryStatusBar({ status }: { status: ModelMemoryStatus | null }) {
  if (!status) return null;
  const { total_mb, available_for_llm_mb, loaded_model } = status;
  if (total_mb <= 0) return null;
  const usedMb = total_mb - available_for_llm_mb;
  const usedPct = Math.round((usedMb / total_mb) * 100);
  const totalGb = (total_mb / 1024).toFixed(1);
  const availGb = (available_for_llm_mb / 1024).toFixed(1);
  return (
    <div className="sys-stats">
      <div className="sys-stats__row">
        <span className="sys-stats__label">Memory</span>
        <span className="sys-stats__value">{availGb} GB free / {totalGb} GB</span>
        {loaded_model && <Chip size="sm" variant="soft">{loaded_model}</Chip>}
      </div>
      <div className="dl-progress__track" style={{ height: 8 }}>
        <div
          className="dl-progress__fill"
          style={{
            width: `${usedPct}%`,
            background: usedPct > 85 ? "var(--color-destructive)" : usedPct > 60 ? "#f59e0b" : "var(--color-success)",
          }}
        />
      </div>
    </div>
  );
}

// ── Main Models Component ─────────────────────────────────────

export function Models() {
  const [category, setCategory] = useState<Category>("llm");
  const [activeRoles, setActiveRoles] = useState<ModelActiveRoles | null>(null);
  const [rolesLoading, setRolesLoading] = useState(false);
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [modelsLoading, setModelsLoading] = useState(true);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [downloads, setDownloads] = useState<DownloadEntry[]>([]);
  const [actionMsg, setActionMsg] = useState<{ text: string; ok: boolean } | null>(null);
  const [memoryStatus, setMemoryStatus] = useState<ModelMemoryStatus | null>(null);
  const [capabilities, setCapabilities] = useState<ModelCapabilities | null>(null);
  const pollRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const loadRoles = useCallback(async () => {
    setRolesLoading(true);
    try { setActiveRoles(await api.getActiveRoles()); }
    catch { /* non-fatal */ }
    finally { setRolesLoading(false); }
  }, []);

  const loadModels = useCallback(async () => {
    setModelsLoading(true); setModelsError(null);
    try { setModels(await api.listModels()); }
    catch (e) { setModelsError(String(e)); }
    finally { setModelsLoading(false); }
  }, []);

  const loadDownloads = useCallback(async () => {
    try { setDownloads((await api.getDownloadProgress()).downloads ?? []); }
    catch { /* non-fatal */ }
  }, []);

  // Exponential-backoff download poll: starts at 2s, doubles each unchanged tick, caps at 15s.
  const startDownloadPoll = useCallback(() => {
    if (pollRef.current) return;
    let delay = 2000;

    async function tick() {
      const { downloads: dl } = await api.getDownloadProgress().catch(() => ({ downloads: [] as DownloadEntry[] }));
      setDownloads(dl ?? []);

      const allDone = dl.every((d) => d.status === "done" || d.status === "error" || d.status === "completed");
      if (allDone) {
        pollRef.current = null;
        loadModels();
        return;
      }

      // Double the delay each tick (unchanged progress = slow download), cap at 15s
      delay = Math.min(delay * 2, 15_000);
      pollRef.current = setTimeout(tick, delay);
    }

    pollRef.current = setTimeout(tick, delay);
  }, [loadDownloads, loadModels]);

  useEffect(() => {
    loadRoles(); loadModels(); loadDownloads();
    api.getMemoryStatus().then(setMemoryStatus).catch(() => {/* non-fatal */});
    api.getModelCapabilities().then(setCapabilities).catch(() => {/* non-fatal */});
    return () => { if (pollRef.current) { clearTimeout(pollRef.current); pollRef.current = null; } };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function flash(text: string, ok = true) {
    setActionMsg({ text, ok });
    setTimeout(() => setActionMsg(null), 3000);
  }

  async function handleActivate(provider: string, name: string, role: string) {
    try {
      if (role === "tool") {
        // Tool caller is a settings field, not a model-role assignment
        await api.updateSettings({ tool_model: name });
        flash(`${name} set as Tool Caller.`);
      } else {
        await api.activateModel(provider, name, role);
        flash(`${name} set as ${ROLE_LABELS[role as RoleKey] ?? role} model.`);
      }
      await loadRoles();
    } catch (e) { flash(String(e), false); }
  }

  async function handleDelete(provider: string, name: string) {
    if (!confirm(`Delete "${name}"? This removes the file from disk.`)) return;
    try {
      await api.deleteModel(provider, name);
      flash(`${name} deleted.`);
      await loadModels();
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        flash(`Cannot delete "${name}" — it is currently assigned to an active role. Deactivate it first.`, false);
      } else {
        flash(String(e), false);
      }
    }
  }

  async function handleScan() {
    try {
      const res = await api.scanModels();
      flash(`Scanned: ${res.found} model${res.found !== 1 ? "s" : ""} found.`);
      await loadModels();
    } catch (e) { flash(String(e), false); }
  }

  const asrModels = models.filter((m) => m.provider === "whisper");
  const ttsModels = models.filter((m) => m.provider === "tts" || m.provider === "tts_piper" || m.provider === "tts_http");

  const llmCount = models.filter((m) => m.provider !== "whisper" && m.provider !== "tts" && m.provider !== "tts_piper" && m.provider !== "tts_http").length;

  return (
    <div className="screen">
      {/* Page header */}
      <div className="page-header">
        <h1 className="page-header__title">Models</h1>
        <div className="page-header__action">
          <Button size="sm" variant="outline" onPress={handleScan}>
            <RefreshCw size={14} /> Scan
          </Button>
        </div>
      </div>

      {/* Active Roles Banner (with memory progress) */}
      <ActiveRolesBanner
        roles={activeRoles}
        memoryStatus={memoryStatus}
        capabilities={capabilities}
        onRefresh={loadRoles}
        loading={rolesLoading}
        onNavigate={(cat) => setCategory(cat)}
      />

      {/* Download Progress */}
      <DownloadProgress downloads={downloads} onScanModels={handleScan} />

      {/* Action feedback */}
      {actionMsg && (
        <p style={{ ...hint, color: actionMsg.ok ? "var(--color-success)" : "var(--color-destructive)" }}>
          {actionMsg.text}
        </p>
      )}

      {/* Category tabs toolbar */}
      <div className="models-toolbar">
        <Tabs
          selectedKey={category}
          onSelectionChange={(k) => setCategory(String(k) as Category)}
        >
          <Tabs.ListContainer>
            <Tabs.List aria-label="Model categories" className="models-toolbar__tabs">
              {CATEGORIES.map(({ key, label, icon }) => (
                <Tabs.Tab key={key} id={key} onClick={() => setCategory(key as Category)}>
                  <Tabs.Indicator />
                  <div className="tab-title">
                    {icon}
                    <span>{label}</span>
                    <span className="tab-title__count">
                      {key === "llm" ? llmCount : key === "asr" ? asrModels.length : ttsModels.length}
                    </span>
                  </div>
                </Tabs.Tab>
              ))}
            </Tabs.List>
          </Tabs.ListContainer>
        </Tabs>
        <div className="models-toolbar__right">
          <div className="models-toolbar__search" style={{ position: "relative" }}>
            <Search size={13} style={{ position: "absolute", left: 9, top: "50%", transform: "translateY(-50%)", color: "var(--grey-500)", pointerEvents: "none" }} />
            <input
              style={{
                width: "100%", height: "32px", paddingLeft: "32px", paddingRight: "12px",
                border: "1px solid var(--grey-200)", borderRadius: "8px",
                fontSize: "var(--text-sm)", fontFamily: "var(--font-body)",
                background: "#fff", color: "var(--fg)", outline: "none",
                boxSizing: "border-box" as const,
              }}
              placeholder="Search models…"
              aria-label="Search models"
            />
          </div>
        </div>
      </div>

      {/* Tab content */}
      {category === "llm" && (
        <LlmTab
          models={models}
          modelsLoading={modelsLoading}
          modelsError={modelsError}
          activeRoles={activeRoles}
          onActivate={handleActivate}
          onDelete={handleDelete}
          onDownloadStarted={() => { startDownloadPoll(); loadDownloads(); }}
          onScanModels={handleScan}
        />
      )}

      {category === "asr" && (
        <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          <div className="seg-banner seg-banner--info">
            <Mic size={14} />
            <span>Automatic Speech Recognition</span>
          </div>
          <ModelList
            models={asrModels}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["asr"]}
            onActivate={handleActivate}
            onDelete={handleDelete}
            emptyMessage="No Whisper models found. Run a model scan or download from HuggingFace."
          />
          <p className="muted-foot">Whisper models power voice-to-text transcription. Place <code>.bin</code> files in <code>models/whisper/</code> and click Scan.</p>
        </div>
      )}

      {category === "tts" && (
        <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          <div className="seg-banner seg-banner--warn">
            <Volume2 size={14} />
            <span>Text-to-Speech</span>
          </div>
          <ModelList
            models={ttsModels}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["tts"]}
            onActivate={handleActivate}
            onDelete={handleDelete}
            emptyMessage="No TTS models found. Place Piper .onnx files in models/tts/ and scan."
          />
          <p className="muted-foot">TTS models power the voice output. Piper voices use <code>.onnx</code> + <code>.json</code> pairs in <code>models/tts/</code>.</p>
        </div>
      )}

      {category === "face" && <FacePanel />}
    </div>
  );
}

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };
