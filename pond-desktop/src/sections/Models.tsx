import { useState, useEffect, useRef, useCallback } from "react";
import { Button, Tabs, Chip } from "@heroui/react";
import {
  Brain, Mic, Volume2, RefreshCw, Download, CheckCircle, XCircle,
  ChevronDown, ChevronUp, Search, Trash2, MessageSquare, Play,
  ScanFace, Loader2, Cpu, Sparkles, Heart,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState } from "../state/AppContext";
import { PageHeader, useConfirm, ErrorBanner } from "../components/shared";
import type {
  ModelEntry, ModelActiveRoles, ModelMemoryStatus, ModelCapabilities,
  HfModel, HfModelFile, DownloadEntry, DiskUsage,
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
        <span key={b.label} className="cap-badge" title={b.title}>{b.label}</span>
      ))}
    </>
  );
}

// ── Design constants ──────────────────────────────────────────

const CAT_COLOR = {
  llm:       "var(--color-role-chat)",
  asr:       "var(--color-role-asr)",
  tts:       "var(--color-role-tts)",
  face:      "#3b82f6",
  embedding: "#8b5cf6",
} as const;

// ── Active Roles Banner ───────────────────────────────────────

/** Maps role keys to role-chip CSS modifier classes */
const ROLE_CHIP_VARIANT: Record<string, string> = {
  chat: "secondary", asr: "primary", tts: "danger", embedding: "accent",
};

/** Infer embedding dimension from well-known model names. */
function inferEmbeddingDimension(modelName: string): string | null {
  const n = modelName.toLowerCase();
  if (n.includes("minilm-l6") || n.includes("minilm_l6")) return "384d";
  if (n.includes("minilm-l12") || n.includes("minilm_l12")) return "384d";
  if (n.includes("bge-small") || n.includes("bge_small")) return "384d";
  if (n.includes("bge-base") || n.includes("bge_base")) return "768d";
  if (n.includes("bge-large") || n.includes("bge_large")) return "1024d";
  if (n.includes("e5-small") || n.includes("e5_small")) return "384d";
  if (n.includes("e5-base") || n.includes("e5_base")) return "768d";
  if (n.includes("e5-large") || n.includes("e5_large")) return "1024d";
  if (n.includes("multilingual-e5")) return "768d";
  if (n.includes("nomic-embed")) return "768d";
  if (n.includes("gte-small") || n.includes("gte_small")) return "384d";
  if (n.includes("gte-base") || n.includes("gte_base")) return "768d";
  if (n.includes("gte-large") || n.includes("gte_large")) return "1024d";
  return null;
}

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
  onNavigate?: (category: "llm" | "asr" | "tts" | "embedding") => void;
}) {
  const ROLE_DEFS: Array<{ key: "chat" | "asr" | "tts" | "embedding"; label: string; icon: React.ReactNode; category: "llm" | "asr" | "tts" | "embedding" }> = [
    { key: "chat",      label: "Main LLM",  icon: <MessageSquare size={12} strokeWidth={1.8} />, category: "llm" },
    { key: "asr",       label: "ASR",       icon: <Mic size={12} strokeWidth={1.8} />,           category: "asr" },
    { key: "tts",       label: "TTS",       icon: <Volume2 size={12} strokeWidth={1.8} />,        category: "tts" },
    { key: "embedding", label: "Embedding", icon: <Cpu size={12} strokeWidth={1.8} />,            category: "embedding" },
  ];
  return (
    <div className="roles-banner">
      <div className="role-grid">
        {ROLE_DEFS.map(({ key, label, icon, category }) => {
          const a = roles?.[key];
          const isSet = !!(a?.provider && a?.model);
          const variant = ROLE_CHIP_VARIANT[key] ?? "secondary";

          // Embedding chip: show model name + dimension + green Active indicator
          if (key === "embedding") {
            const dim = isSet ? inferEmbeddingDimension(a!.model) : null;
            return (
              <div
                key={key}
                className={`role-chip role-chip--${variant}${isSet ? " is-set" : ""}`}
                onClick={!isSet && onNavigate ? () => onNavigate(category) : undefined}
                style={{ cursor: !isSet && onNavigate ? "pointer" : "default" }}
                title={!isSet ? `Click to set ${label} model` : undefined}
              >
                <div className="role-chip__bar" />
                <div className="role-chip__body">
                  <div className="role-chip__head">
                    {icon}
                    <span className="role-chip__role">{label}</span>
                    {isSet && (
                      <span className="role-chip__active-badge">
                        <CheckCircle size={10} strokeWidth={2.5} />
                        Active
                      </span>
                    )}
                  </div>
                  <span className={`role-chip__value ${isSet ? "role-chip__value--set" : "role-chip__value--empty"}`}>
                    {isSet
                      ? <>
                          {a!.model}
                          {dim && (
                            <span className="role-chip__dim">{dim}</span>
                          )}
                        </>
                      : "---"}
                  </span>
                </div>
              </div>
            );
          }

          return (
            <div
              key={key}
              className={`role-chip role-chip--${variant}${isSet ? " is-set" : ""}`}
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
                <span className={`role-chip__value ${isSet ? "role-chip__value--set" : "role-chip__value--empty"}`}>
                  {isSet ? `${a!.provider} / ${a!.model}` : "---"}
                </span>
              </div>
            </div>
          );
        })}
      </div>

      {/* Active model capabilities -- shown inline when set */}
      {capabilities && (capabilities.thinking || capabilities.vision || capabilities.audio_input || capabilities.context_window_tokens > 4096) && (
        <div className="cap-badge-row">
          {capabilities.thinking && <span className="cap-badge cap-badge--brand">Thinking</span>}
          {capabilities.vision && <span className="cap-badge cap-badge--brand">Vision</span>}
          {capabilities.audio_input && <span className="cap-badge cap-badge--brand">Audio</span>}
          {capabilities.structured_output && <span className="cap-badge cap-badge--brand">Structured</span>}
          {capabilities.context_window_tokens > 4096 && (
            <span className="cap-badge cap-badge--brand">
              {Math.round(capabilities.context_window_tokens / 1000)}k ctx
            </span>
          )}
        </div>
      )}
    </div>
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

type RoleKey = "chat" | "asr" | "tts" | "embedding";

const ROLE_LABELS: Record<RoleKey, string> = {
  chat: "Main LLM", asr: "ASR", tts: "TTS", embedding: "Embedding",
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

  if (loading) return <p className="hint">Loading…</p>;
  if (error) return <ErrorBanner error={error} />;
  if (models.length === 0) return <p className="hint">{emptyMessage}</p>;

  function isRoleActive(m: ModelEntry, role: RoleKey) {
    const a = activeRoles?.[role];
    if (!a) return false;
    return a.provider === m.provider && a.model === m.name;
  }

  function activeRolesFor(m: ModelEntry): RoleKey[] {
    return availableRoles.filter((r) => isRoleActive(m, r));
  }

  return (
    <div className="card card--overflow">
      {models.map((m) => {
        const activeFor = activeRolesFor(m);
        const isAnyActive = activeFor.length > 0;

        return (
          <div
            key={m.id}
            className={`model-row${isAnyActive ? " is-active" : ""}`}
          >
            <div>
              <div className="model-row__title-row">
                <span className="model-row__name">{m.display_name ?? m.name}</span>
                {m.ram_estimate_mb && (
                  <Chip size="sm" variant="flat" color="default" className="model-row__size">{m.ram_estimate_mb} MB</Chip>
                )}
                {activeFor.map((r) => (
                  <Chip key={r} size="sm" variant="flat"
                    color={r === "chat" ? "secondary" : r === "asr" ? "primary" : r === "tts" ? "danger" : "default"}
                    className="model-row__tag">
                    {ROLE_LABELS[r]}
                  </Chip>
                ))}
                <CapabilityBadges name={m.name} />
              </div>
              <div className="model-row__file"><code>{m.provider} / {m.name}</code></div>
            </div>
            <div className="model-row__actions">
              <div className="role-select">
                {availableRoles.map((role) => {
                  const active = isRoleActive(m, role);
                  return (
                    <Button
                      key={role}
                      size="sm"
                      variant={active ? "secondary" : "outline"}
                      className="role-select__btn"
                      onPress={() => onActivate(m.provider, m.name, role)}
                      isDisabled={!state.serverOnline}
                      aria-label={`Set ${m.name} as ${role} model`}
                    >
                      {active && <CheckCircle size={11} strokeWidth={1.8} />}
                      {ROLE_LABELS[role]}
                    </Button>
                  );
                })}
              </div>
              <Button
                size="sm"
                variant="ghost"
                isIconOnly
                className="model-row__trash"
                onPress={() => onDelete(m.provider, m.name)}
                isDisabled={!state.serverOnline}
                aria-label={`Delete ${m.name}`}
              >
                <Trash2 size={12} strokeWidth={1.8} />
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
      <button className="acc-header" onClick={() => { setOpen((v) => !v); if (!open && results.length === 0) search(); }}>
        <span className="acc-header__label"><Download size={13} /> Browse HuggingFace</span>
        {open ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
      </button>

      {open && (
        <div className="acc-body">
          <div className="acc-search-row">
            <div className="acc-search-wrap">
              <Search size={13} className="acc-search-icon" />
              <input
                className="acc-search-input"
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

          {searchError && <p className="hint hint--error">{searchError}</p>}
          {dlMsg && <p className={`hint ${dlMsg.startsWith("Error") ? "hint--error" : "hint--success"}`}>{dlMsg}</p>}

          {!searching && results.length === 0 && !searchError && (
            <p className="gh-empty"><Search size={14} /> No results. Search for &quot;gemma&quot;, &quot;llama&quot;, or &quot;mistral&quot;.</p>
          )}

          <div className="acc-list">
            {results.map((model) => {
              const isExpanded = expandedRepo === model.id;
              const files = repoFiles[model.id] ?? [];
              return (
                <div key={model.id} className="hf-repo">
                  <div className="hf-repo__header">
                    <div className="hf-repo__meta">
                      <a href={model.url} target="_blank" rel="noopener noreferrer" className="hf-repo__id">{model.id}</a>
                      <div className="hf-repo__tags">
                        <Chip size="sm" variant="soft">↓ {model.downloads?.toLocaleString() ?? "?"}</Chip>
                        <Chip size="sm" variant="soft"><Heart size={11} /> {model.likes ?? "?"}</Chip>
                        {model.tags?.slice(0, 2).map((tag) => <Chip key={tag} size="sm" variant="soft">{tag}</Chip>)}
                      </div>
                    </div>
                    <Button variant="outline" size="sm" onPress={() => browseFiles(model.id)} isDisabled={!state.serverOnline}>
                      {loadingFiles === model.id ? "…" : isExpanded ? "Hide" : "Files"}
                    </Button>
                  </div>
                  {isExpanded && (
                    <div className="hf-file-list">
                      {loadingFiles === model.id && <p className="hint" style={{ padding: "var(--space-2) var(--space-3)" }}>Loading files…</p>}
                      {!loadingFiles && files.length === 0 && <p className="gh-empty" style={{ padding: "var(--space-2) var(--space-3)" }}>No .gguf files in this repo.</p>}
                      {files.map((file) => (
                        <div key={file.filename} className="hf-file-row">
                          <code className="model-row__name">{file.filename}</code>
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
      <button className="acc-header" onClick={() => { setOpen((v) => !v); if (!open) load(); }}>
        <span className="acc-header__label"><Download size={13} /> Browse GitHub Releases</span>
        {open ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
      </button>
      {open && (
        <div className="acc-body">
          {loading && <p className="hint">Fetching releases…</p>}
          {error && <ErrorBanner error={error} onRetry={() => { setLoaded(false); load(); }} retryLabel="Retry fetch" />}
          {dlMsg && <p className={`hint ${dlMsg.startsWith("Error") ? "hint--error" : "hint--success"}`}>{dlMsg}</p>}
          {!loading && releases.length === 0 && !error && <p className="gh-empty"><Download size={14} /> No llamafile releases found.</p>}
          <div className="acc-list">
            {releases.map((rel) => (
              <div key={rel.name} className="hf-file-row">
                <div className="hf-repo__meta">
                  <code className="model-row__name">{rel.name}</code>
                  <div className="hf-repo__tags">
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
    <div className="ollama-panel">
      {/* Status bar */}
      <div className={`seg-banner ${isRunning ? "seg-banner--info" : "seg-banner--warn"}`}>
        <span className={`giap-status-dot ${ollamaLoading ? "" : isRunning ? "giap-status-dot--online" : "giap-status-dot--offline"}`} />
        <span className="ollama-status__text">
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

      {pullMsg && <p className={`hint ${pullMsg.startsWith("Error") ? "hint--error" : pullMsg.startsWith("Run") ? "hint--secondary" : "hint--success"}`}>{pullMsg}</p>}

      {/* Pull row */}
      <div className="ollama-pull-row">
        <input
          className="pull-input"
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
  const state = useAppState();
  const [provider, setProvider] = useState<LlmProvider>("gguf");
  const [downloadingModel, setDownloadingModel] = useState<string | null>(null);
  const [dlMsg, setDlMsg] = useState<string | null>(null);
  const PROVIDERS: Array<{ key: LlmProvider; label: string }> = [
    { key: "gguf", label: "GGUF" },
    { key: "llamafile", label: "Llamafile" },
    { key: "ollama", label: "Ollama" },
  ];

  const ggufModels = models.filter((m) => m.provider === "gguf");
  const llamafileModels = models.filter((m) => m.provider === "llamafile");
  const ollamaRegistryModels = models.filter((m) => m.provider === "ollama");

  const ggufDownloaded  = ggufModels.filter((m) => m.downloaded !== false);
  const ggufAvailable   = ggufModels.filter((m) => m.downloaded === false);
  const llamaDownloaded = llamafileModels.filter((m) => m.downloaded !== false);
  const llamaAvailable  = llamafileModels.filter((m) => m.downloaded === false);

  async function handleCatalogDownload(category: string, m: ModelEntry) {
    setDownloadingModel(m.name); setDlMsg(null);
    try {
      const res = await api.downloadModel(category, m.name);
      setDlMsg(res.status === "already_downloaded" ? `${m.name} already downloaded.` : `Download started: ${m.name}`);
      onDownloadStarted();
    } catch (e) { setDlMsg(`Error: ${String(e)}`); }
    finally { setDownloadingModel(null); }
  }

  return (
    <div className="llm-tab">
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
        {(ggufModels.length + llamafileModels.length + ollamaRegistryModels.length) > 0 && (
          <div className="seg-toolbar__count">
            <span>{ggufModels.length + llamafileModels.length + ollamaRegistryModels.length} models</span>
          </div>
        )}
      </div>

      {dlMsg && (
        <p className={`hint ${dlMsg.startsWith("Error") ? "hint--error" : "hint--success"}`}>{dlMsg}</p>
      )}

      {/* GGUF panel */}
      {provider === "gguf" && (
        <div className="provider-panel">
          {ggufDownloaded.length > 0 && (
            <>
              <span className="card__label">Installed</span>
              <ModelList
                models={ggufDownloaded}
                loading={modelsLoading}
                error={modelsError}
                activeRoles={activeRoles}
                availableRoles={["chat"]}
                onActivate={onActivate}
                onDelete={onDelete}
                emptyMessage=""
              />
            </>
          )}
          {!modelsLoading && ggufAvailable.length > 0 && (
            <>
              <span className="card__label">Available for download</span>
              <div className="card card--overflow">
                {ggufAvailable.map((m) => (
                  <div key={m.id} className="model-row">
                    <div>
                      <div className="model-row__title-row">
                        <span className="model-row__name">{m.display_name ?? m.name}</span>
                        {m.size_mb != null && <Chip size="sm" variant="flat" color="default" className="model-row__size">{m.size_mb} MB</Chip>}
                        <CapabilityBadges name={m.name} />
                      </div>
                      {m.description && (
                        <div className="model-row__file"><code>{m.description}</code></div>
                      )}
                    </div>
                    <div className="model-row__actions">
                      <Button
                        size="sm"
                        variant="secondary"
                        onPress={() => handleCatalogDownload("gguf", m)}
                        isDisabled={downloadingModel === m.name || !state.serverOnline}
                      >
                        <Download size={11} strokeWidth={1.8} /> {downloadingModel === m.name ? "Starting\u2026" : "Download"}
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            </>
          )}
          {modelsLoading && ggufDownloaded.length === 0 && <p className="hint">Loading\u2026</p>}
          <BrowseHfAccordion onDownloadStarted={onDownloadStarted} />
        </div>
      )}

      {/* Llamafile panel */}
      {provider === "llamafile" && (
        <div className="provider-panel">
          {llamaDownloaded.length > 0 && (
            <>
              <span className="card__label">Installed</span>
              <ModelList
                models={llamaDownloaded}
                loading={modelsLoading}
                error={modelsError}
                activeRoles={activeRoles}
                availableRoles={["chat"]}
                onActivate={onActivate}
                onDelete={onDelete}
                emptyMessage=""
              />
            </>
          )}
          {!modelsLoading && llamaAvailable.length > 0 && (
            <>
              <span className="card__label">Available for download</span>
              <div className="card card--overflow">
                {llamaAvailable.map((m) => (
                  <div key={m.id} className="model-row">
                    <div>
                      <div className="model-row__title-row">
                        <span className="model-row__name">{m.display_name ?? m.name}</span>
                        {m.size_mb != null && <Chip size="sm" variant="flat" color="default" className="model-row__size">{m.size_mb} MB</Chip>}
                        <CapabilityBadges name={m.name} />
                      </div>
                      {m.description && (
                        <div className="model-row__file"><code>{m.description}</code></div>
                      )}
                    </div>
                    <div className="model-row__actions">
                      <Button
                        size="sm"
                        variant="secondary"
                        onPress={() => handleCatalogDownload("llamafile", m)}
                        isDisabled={downloadingModel === m.name || !state.serverOnline}
                      >
                        <Download size={11} strokeWidth={1.8} /> {downloadingModel === m.name ? "Starting\u2026" : "Download"}
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            </>
          )}
          {modelsLoading && llamaDownloaded.length === 0 && <p className="hint">Loading\u2026</p>}
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

type Category = "llm" | "asr" | "tts" | "face" | "embedding";

const CATEGORIES: Array<{ key: Category; label: string; icon: React.ReactNode; color: string }> = [
  { key: "llm",       label: "LLM",       icon: <Brain size={14} strokeWidth={1.8} />,   color: CAT_COLOR.llm },
  { key: "asr",       label: "ASR",       icon: <Mic size={14} strokeWidth={1.8} />,     color: CAT_COLOR.asr },
  { key: "tts",       label: "TTS",       icon: <Volume2 size={14} strokeWidth={1.8} />, color: CAT_COLOR.tts },
  { key: "embedding", label: "Embedding", icon: <Cpu size={14} strokeWidth={1.8} />,     color: CAT_COLOR.embedding },
  { key: "face",      label: "Face",      icon: <ScanFace size={14} strokeWidth={1.8} />, color: "#3b82f6" },
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

  return (
    <div className="face-panel">
      <div className="face-panel__header">
        <ScanFace size={14} style={{ color: CAT_COLOR.face }} />
        <span className="face-panel__title" style={{ color: CAT_COLOR.face }}>
          Face Recognition
        </span>
        {data && (
          <span className="face-panel__badge">
            {data.feature_enabled ? `${installed}/${total} ready` : "feature disabled"}
          </span>
        )}
        <div className="face-panel__spacer" />
        <Button variant="ghost" size="sm" onPress={reload} isDisabled={loading}>
          <RefreshCw size={12} /> Refresh
        </Button>
      </div>

      {loading && <p className="face-panel__hint">Loading...</p>}
      {error && <p className="face-panel__hint face-panel__hint--error">{error}</p>}

      {data && !data.feature_enabled && (
        <p className="face-panel__hint">
          Face recognition is disabled in this build. Rebuild pond-server with
          {" "}<code className="face-panel__code">--features face-onnx</code>{" "}
          to enable per-user identification.
        </p>
      )}

      {data && data.models.length > 0 && (
        <div className="face-model-list">
          {data.models.map(m => (
            <div
              key={m.name}
              className="face-panel__row"
              style={{ borderLeftColor: m.downloaded ? CAT_COLOR.face : "transparent" }}
            >
              <div className="face-model__body">
                <div className="face-model__name-row">
                  <span className="face-model__name">{m.label}</span>
                  <span className="face-panel__badge">{m.role}</span>
                  {m.downloaded ? (
                    <span className="face-panel__badge face-panel__badge--success">
                      {m.size_mb != null ? `${m.size_mb} MB` : "ready"}
                    </span>
                  ) : (
                    <span className="face-panel__badge face-panel__badge--warning">
                      missing · ~{m.expected_mb} MB
                    </span>
                  )}
                </div>
                <span className="face-model__path">{m.name}</span>
              </div>
            </div>
          ))}
        </div>
      )}

      {data?.models_dir && (
        <p className="face-panel__hint face-panel__hint--dir">{data.models_dir}</p>
      )}

      <p className="face-panel__hint">
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
      <div className="dl-progress__track dl-progress__track--mem">
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

// ── ASR Catalog Panel ────────────────────────────────────────

function AsrCatalogPanel({
  models,
  modelsLoading,
  modelsError,
  activeRoles,
  onActivate,
  onDelete,
  onDownloadStarted,
}: {
  models: ModelEntry[];
  modelsLoading: boolean;
  modelsError: string | null;
  activeRoles: ModelActiveRoles | null;
  onActivate: (provider: string, name: string, role: string) => void;
  onDelete: (provider: string, name: string) => void;
  onDownloadStarted: () => void;
}) {
  const state = useAppState();
  const [downloadingModel, setDownloadingModel] = useState<string | null>(null);
  const [dlMsg, setDlMsg] = useState<string | null>(null);

  const downloaded = models.filter((m) => m.downloaded !== false);
  const available  = models.filter((m) => m.downloaded === false);

  async function handleDownload(m: ModelEntry) {
    setDownloadingModel(m.name); setDlMsg(null);
    try {
      const res = await api.downloadModel("whisper", m.name);
      setDlMsg(res.status === "already_downloaded" ? `${m.name} already downloaded.` : `Download started: ${m.name}`);
      onDownloadStarted();
    } catch (e) { setDlMsg(`Error: ${String(e)}`); }
    finally { setDownloadingModel(null); }
  }

  return (
    <div className="provider-panel">
      <div className="seg-banner seg-banner--info">
        <Mic size={14} />
        <span>Automatic Speech Recognition &mdash; Whisper models for voice-to-text</span>
      </div>

      {dlMsg && (
        <p className={`hint ${dlMsg.startsWith("Error") ? "hint--error" : "hint--success"}`}>{dlMsg}</p>
      )}

      {/* Downloaded models */}
      {downloaded.length > 0 && (
        <>
          <span className="card__label">Installed</span>
          <ModelList
            models={downloaded}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["asr"]}
            onActivate={onActivate}
            onDelete={onDelete}
            emptyMessage=""
          />
        </>
      )}

      {/* Available for download */}
      {!modelsLoading && available.length > 0 && (
        <>
          <span className="card__label">Available for download</span>
          <div className="card card--overflow">
            {available.map((m) => (
              <div key={m.id} className="model-row">
                <div>
                  <div className="model-row__title-row">
                    <span className="model-row__name">{m.display_name ?? m.name}</span>
                    {m.asr_language && <span className="cap-badge">{m.asr_language}</span>}
                    {m.size_mb != null && <Chip size="sm" variant="flat" color="default" className="model-row__size">{m.size_mb} MB</Chip>}
                  </div>
                  <div className="model-row__file"><code>whisper / {m.name}</code></div>
                </div>
                <div className="model-row__actions">
                  <Button
                    size="sm"
                    variant="secondary"
                    onPress={() => handleDownload(m)}
                    isDisabled={downloadingModel === m.name || !state.serverOnline}
                  >
                    <Download size={11} strokeWidth={1.8} /> {downloadingModel === m.name ? "Starting\u2026" : "Download"}
                  </Button>
                </div>
              </div>
            ))}
          </div>
        </>
      )}

      {modelsLoading && <p className="hint">Loading…</p>}
      {!modelsLoading && models.length === 0 && (
        <p className="hint">No Whisper models found in catalog. Try refreshing the registry.</p>
      )}
    </div>
  );
}

// ── Embedding Catalog Panel ───────────────────────────────────

function EmbeddingCatalogPanel({
  models,
  modelsLoading,
  modelsError,
  activeRoles,
  onActivate,
  onDelete,
  onDownloadStarted,
}: {
  models: ModelEntry[];
  modelsLoading: boolean;
  modelsError: string | null;
  activeRoles: ModelActiveRoles | null;
  onActivate: (provider: string, name: string, role: string) => void;
  onDelete: (provider: string, name: string) => void;
  onDownloadStarted: () => void;
}) {
  const state = useAppState();
  const [downloadingModel, setDownloadingModel] = useState<string | null>(null);
  const [dlMsg, setDlMsg] = useState<string | null>(null);
  // Track models that returned "ready" immediately — treated as installed
  const [readyModels, setReadyModels] = useState<Set<string>>(new Set());

  const downloaded = models.filter((m) => m.downloaded !== false || readyModels.has(m.name));
  const available  = models.filter((m) => m.downloaded === false && !readyModels.has(m.name));

  const activeEmbedding = activeRoles?.embedding;

  async function handleDownload(m: ModelEntry) {
    setDownloadingModel(m.name); setDlMsg(null);
    try {
      const res = await api.downloadModel("embedding", m.name);
      if (res.status === "ready") {
        // fastembed auto-downloads on first use — mark as installed immediately
        setReadyModels((prev) => new Set([...prev, m.name]));
        setDlMsg(`${m.name} is ready — downloads automatically on first use.`);
      } else if (res.status === "already_downloaded") {
        setDlMsg(`${m.name} already downloaded.`);
        onDownloadStarted();
      } else {
        setDlMsg(`Download started: ${m.name}`);
        onDownloadStarted();
      }
    } catch (e) { setDlMsg(`Error: ${String(e)}`); }
    finally { setDownloadingModel(null); }
  }

  return (
    <div className="provider-panel">
      <div className="seg-banner seg-banner--embed">
        <Cpu size={14} style={{ color: CAT_COLOR.embedding }} />
        <span>Embedding models help the agent understand your intent and route queries to the right tools</span>
      </div>

      {/* First-use note */}
      <div className="seg-banner seg-banner--note">
        <Download size={13} style={{ color: "var(--grey-500)" }} />
        <span>First-time setup: the model downloads 23-86 MB on first use. Check server logs for progress.</span>
      </div>

      {dlMsg && (
        <p className={`hint ${dlMsg.startsWith("Error") ? "hint--error" : "hint--success"}`}>{dlMsg}</p>
      )}

      {!activeEmbedding && !modelsLoading && downloaded.length === 0 && models.length === 0 && (
        <p className="hint hint--italic">No embedding models found. Download one below to enable better tool routing.</p>
      )}

      {!activeEmbedding && !modelsLoading && downloaded.length > 0 && (
        <p className="hint hint--italic">Set a default embedding model for better tool routing.</p>
      )}

      {/* Downloaded / ready models */}
      {downloaded.length > 0 && (
        <>
          <span className="card__label">Installed</span>
          <ModelList
            models={downloaded}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["embedding"]}
            onActivate={onActivate}
            onDelete={onDelete}
            emptyMessage=""
          />
        </>
      )}

      {/* Available for download */}
      {!modelsLoading && available.length > 0 && (
        <>
          <span className="card__label">Available for download</span>
          <div className="card card--overflow">
            {available.map((m) => (
              <div key={m.id} className="model-row">
                <div>
                  <div className="model-row__title-row">
                    <span className="model-row__name">{m.display_name ?? m.name}</span>
                    {m.size_mb != null && <Chip size="sm" variant="flat" color="default" className="model-row__size">{m.size_mb} MB</Chip>}
                  </div>
                  {m.description && (
                    <div className="model-row__file"><code>{m.description}</code></div>
                  )}
                </div>
                <div className="model-row__actions">
                  <Button
                    size="sm"
                    variant="secondary"
                    onPress={() => handleDownload(m)}
                    isDisabled={downloadingModel === m.name || !state.serverOnline}
                  >
                    <Download size={11} strokeWidth={1.8} /> {downloadingModel === m.name ? "Starting…" : "Download"}
                  </Button>
                </div>
              </div>
            ))}
          </div>
        </>
      )}

      {modelsLoading && <p className="hint">Loading…</p>}
      {!modelsLoading && models.length === 0 && (
        <p className="hint">No embedding models found in catalog. Try refreshing the registry.</p>
      )}
    </div>
  );
}

// ── TTS Catalog Panel ────────────────────────────────────────

function TtsCatalogPanel({
  models,
  modelsLoading,
  modelsError,
  activeRoles,
  onActivate,
  onDelete,
  onDownloadStarted,
}: {
  models: ModelEntry[];
  modelsLoading: boolean;
  modelsError: string | null;
  activeRoles: ModelActiveRoles | null;
  onActivate: (provider: string, name: string, role: string) => void;
  onDelete: (provider: string, name: string) => void;
  onDownloadStarted: () => void;
}) {
  const state = useAppState();
  const [downloadingModel, setDownloadingModel] = useState<string | null>(null);
  const [dlMsg, setDlMsg] = useState<string | null>(null);

  const downloaded = models.filter((m) => m.downloaded !== false);
  const available  = models.filter((m) => m.downloaded === false);

  async function handleDownload(m: ModelEntry) {
    setDownloadingModel(m.name); setDlMsg(null);
    try {
      const res = await api.downloadModel("tts", m.name);
      setDlMsg(res.status === "already_downloaded" ? `${m.name} already downloaded.` : `Download started: ${m.name}`);
      onDownloadStarted();
    } catch (e) { setDlMsg(`Error: ${String(e)}`); }
    finally { setDownloadingModel(null); }
  }

  return (
    <div className="provider-panel">
      <div className="seg-banner seg-banner--warn">
        <Volume2 size={14} />
        <span>Text-to-Speech &mdash; Piper voices for spoken output</span>
      </div>

      {dlMsg && (
        <p className={`hint ${dlMsg.startsWith("Error") ? "hint--error" : "hint--success"}`}>{dlMsg}</p>
      )}

      {/* Downloaded voices */}
      {downloaded.length > 0 && (
        <>
          <span className="card__label">Installed</span>
          <ModelList
            models={downloaded}
            loading={modelsLoading}
            error={modelsError}
            activeRoles={activeRoles}
            availableRoles={["tts"]}
            onActivate={onActivate}
            onDelete={onDelete}
            emptyMessage=""
          />
        </>
      )}

      {/* Available for download */}
      {!modelsLoading && available.length > 0 && (
        <>
          <span className="card__label">Available for download</span>
          <div className="card card--overflow">
            {available.map((m) => (
              <div key={m.id} className="model-row">
                <div>
                  <div className="model-row__title-row">
                    <span className="model-row__name">{m.display_name ?? m.name}</span>
                    {m.size_mb != null && <Chip size="sm" variant="flat" color="default" className="model-row__size">{m.size_mb} MB</Chip>}
                  </div>
                  {m.description && (
                    <div className="model-row__file"><code>{m.description}</code></div>
                  )}
                </div>
                <div className="model-row__actions">
                  <Button
                    size="sm"
                    variant="secondary"
                    onPress={() => handleDownload(m)}
                    isDisabled={downloadingModel === m.name || !state.serverOnline}
                  >
                    <Download size={11} strokeWidth={1.8} /> {downloadingModel === m.name ? "Starting…" : "Download"}
                  </Button>
                </div>
              </div>
            ))}
          </div>
        </>
      )}

      {modelsLoading && <p className="hint">Loading…</p>}
      {!modelsLoading && models.length === 0 && (
        <p className="hint">No TTS voices found in catalog. Try refreshing the registry.</p>
      )}
    </div>
  );
}

// ── Main Models Component ─────────────────────────────────────

export function Models() {
  const confirm = useConfirm();
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
  const [diskUsage, setDiskUsage] = useState<DiskUsage | null>(null);
  const [cleaning, setCleaning] = useState(false);
  const pollRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const diskPollRef = useRef<ReturnType<typeof setInterval> | null>(null);

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
    loadRoles(); loadModels(); loadDownloads(); loadDiskUsage();
    api.getMemoryStatus().then(setMemoryStatus).catch(() => {/* non-fatal */});
    api.getModelCapabilities().then(setCapabilities).catch(() => {/* non-fatal */});
    // Refresh disk usage every 30s while the section is open.
    diskPollRef.current = setInterval(() => { loadDiskUsage(); }, 30_000);
    return () => {
      if (pollRef.current) { clearTimeout(pollRef.current); pollRef.current = null; }
      if (diskPollRef.current) { clearInterval(diskPollRef.current); diskPollRef.current = null; }
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function flash(text: string, ok = true) {
    setActionMsg({ text, ok });
    setTimeout(() => setActionMsg(null), 3000);
  }

  async function handleActivate(provider: string, name: string, role: string) {
    try {
      await api.activateModel(provider, name, role);
      flash(`${name} set as ${ROLE_LABELS[role as RoleKey] ?? role} model.`);
      await loadRoles();
    } catch (e) { flash(String(e), false); }
  }

  async function handleDelete(provider: string, name: string) {
    if (!await confirm(`Delete "${name}"? This removes the file from disk.`, { title: "Delete Model", confirmLabel: "Delete", destructive: true })) return;
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

  const loadDiskUsage = useCallback(async () => {
    try { setDiskUsage(await api.getDiskUsage()); }
    catch { /* non-fatal */ }
  }, []);

  async function handleCleanup() {
    if (cleaning) return;
    setCleaning(true);
    flash("Reclaiming disk space...");
    try {
      const res = await api.cleanupModels();
      const n = res.removed.length;
      flash(`Reclaimed ${fmtBytes(res.reclaimed_bytes)}${n ? ` (${n} item${n === 1 ? "" : "s"})` : ""}.`);
      await Promise.all([loadModels(), loadDiskUsage()]);
    } catch (e) { flash(String(e), false); }
    finally { setCleaning(false); }
  }

  const asrModels       = models.filter((m) => m.provider === "whisper");
  const ttsModels       = models.filter((m) => m.provider === "tts" || m.provider === "tts_piper" || m.provider === "tts_http");
  const embeddingModels = models.filter((m) => m.provider === "embedding" || m.category === "embedding");

  const NON_LLM_PROVIDERS = new Set(["whisper", "tts", "tts_piper", "tts_http", "embedding"]);
  const llmCount = models.filter((m) => !NON_LLM_PROVIDERS.has(m.provider) && m.category !== "embedding").length;

  return (
    <div className="screen">
      {/* Page header */}
      <PageHeader
        title="Models"
        action={
          <div style={{ display: "inline-flex", gap: 6, alignItems: "center" }}>
            {diskUsage && (
              <span
                style={{ fontSize: "11px", color: "var(--grey-500)", fontFamily: "var(--font-mono)" }}
                data-testid="models-disk-usage"
              >
                {fmtBytes(diskUsage.total_bytes)} used · {fmtBytes(diskUsage.hf_cache_bytes)} in cache
              </span>
            )}
            <Button
              size="sm"
              variant="ghost"
              onPress={handleCleanup}
              isDisabled={cleaning || downloads.some((d) => d.status === "downloading")}
              data-testid="models-cleanup-btn"
            >
              <Sparkles size={14} strokeWidth={1.8} /> {cleaning ? "Cleaning…" : "Free up space"}
            </Button>
            <Button size="sm" variant="ghost" onPress={handleScan}>
              <RefreshCw size={14} strokeWidth={1.8} /> Scan
            </Button>
          </div>
        }
      />

      {/* Active Roles Card */}
      <div className="card">
        <div className="card-header">
          <span className="card__label">Active model roles</span>
          <div className="card-header__right">
            {memoryStatus && memoryStatus.total_mb > 0 && (
              <span className="mem-stat">
                {(((memoryStatus.total_mb - memoryStatus.available_for_llm_mb) / memoryStatus.total_mb) * 100).toFixed(0)}%
                {" · "}
                {(memoryStatus.available_for_llm_mb / 1024).toFixed(1)} / {(memoryStatus.total_mb / 1024).toFixed(1)} GB
                {memoryStatus.loaded_model && (
                  <Chip size="sm" variant="flat" color="secondary" className="mem-stat__chip">{memoryStatus.loaded_model}</Chip>
                )}
              </span>
            )}
            <Button size="sm" variant="ghost" isIconOnly onPress={loadRoles} isDisabled={rolesLoading} aria-label="Refresh roles">
              <RefreshCw size={13} strokeWidth={1.8} style={{ opacity: rolesLoading ? 0.4 : 1, transition: "opacity 0.2s" }} />
            </Button>
          </div>
        </div>
        <div className="card-body--roles">
          <ActiveRolesBanner
            roles={activeRoles}
            memoryStatus={memoryStatus}
            capabilities={capabilities}
            onRefresh={loadRoles}
            loading={rolesLoading}
            onNavigate={(cat) => setCategory(cat)}
          />
        </div>
      </div>

      {/* Download Progress */}
      <DownloadProgress downloads={downloads} onScanModels={handleScan} />

      {/* Action feedback */}
      {actionMsg && (
        <p className={`hint ${actionMsg.ok ? "hint--success" : "hint--error"}`}>
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
              {CATEGORIES.map(({ key, label, icon }) => {
                const count =
                  key === "llm" ? llmCount :
                  key === "asr" ? asrModels.length :
                  key === "tts" ? ttsModels.length :
                  key === "embedding" ? embeddingModels.length : 0;
                return (
                  <Tabs.Tab key={key} id={key} onClick={() => setCategory(key as Category)}>
                    <Tabs.Indicator />
                    <div className="tab-title">
                      {icon}
                      <span>{label}</span>
                      {count > 0 && <span className="tab-title__count">{count}</span>}
                    </div>
                  </Tabs.Tab>
                );
              })}
            </Tabs.List>
          </Tabs.ListContainer>
        </Tabs>
        <div className="models-toolbar__right">
          <div className="models-toolbar__search">
            <Search size={13} strokeWidth={1.8} className="search-icon" />
            <input
              className="models-search-input"
              placeholder="Search models..."
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
        <AsrCatalogPanel
          models={asrModels}
          modelsLoading={modelsLoading}
          modelsError={modelsError}
          activeRoles={activeRoles}
          onActivate={handleActivate}
          onDelete={handleDelete}
          onDownloadStarted={() => { startDownloadPoll(); loadDownloads(); }}
        />
      )}

      {category === "tts" && (
        <TtsCatalogPanel
          models={ttsModels}
          modelsLoading={modelsLoading}
          modelsError={modelsError}
          activeRoles={activeRoles}
          onActivate={handleActivate}
          onDelete={handleDelete}
          onDownloadStarted={() => { startDownloadPoll(); loadDownloads(); }}
        />
      )}

      {category === "embedding" && (
        <EmbeddingCatalogPanel
          models={embeddingModels}
          modelsLoading={modelsLoading}
          modelsError={modelsError}
          activeRoles={activeRoles}
          onActivate={handleActivate}
          onDelete={handleDelete}
          onDownloadStarted={() => { startDownloadPoll(); loadDownloads(); }}
        />
      )}

      {category === "face" && <FacePanel />}
    </div>
  );
}

