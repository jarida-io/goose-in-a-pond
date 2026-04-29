import { useState, useEffect, useCallback, useRef } from 'react'
import { api, isPreviewMode, type ModelStatusEntry, type OllamaModel, type Settings, type MemoryStatus, type HuggingFaceModel, type HuggingFaceFile, type LlamafileAsset, type DownloadEntry } from '../api'

interface Props {
  token: string
}

const CATEGORY_LABELS: Record<string, string> = {
  whisper:   'Whisper ASR',
  llamafile: 'Llamafile (LLM)',
  gguf:      'GGUF (Local LLM)',
  tts:       'Text-to-Speech',
}

const CATEGORY_DESC: Record<string, string> = {
  whisper:   'Speech-to-text models for voice input.',
  llamafile: 'Self-contained LLM executables for the llamafile provider.',
  gguf:      'GGUF weights for the local (Goose GGUF) provider. Requires no server.',
  tts:       'Voice synthesis models and servers.',
}

const ROLE_LABELS: Record<string, string> = { chat: 'Chat', think: 'Think', task: 'Task' }
const ROLE_ICONS:  Record<string, string> = { chat: '💬',   think: '🧠',    task: '⚙️'  }

function formatSize(mb: number): string {
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`
  return `${mb} MB`
}

/** Infer capabilities from a model name and render compact badges. */
function WebCapabilityBadges({ name }: { name: string }) {
  const n = name.toLowerCase()
  const badges: string[] = []
  if (/gemma[-_]?4|qwen3|qwq|deepseek[-_]?r1/.test(n)) badges.push('Thinking')
  if (/gemma[-_]?4|llava|bakllava|moondream/.test(n)) badges.push('Vision')
  if (/gemma[-_]?4/.test(n) && /e[24]b/i.test(n)) badges.push('Audio')
  if (/gemma[-_]?4/.test(n)) badges.push('128k ctx')
  else if (/qwen/.test(n) || /mistral/.test(n)) badges.push('32k ctx')

  if (badges.length === 0) return null
  return (
    <div style={{ display: 'flex', gap: '0.3rem', flexWrap: 'wrap', marginTop: '0.25rem' }}>
      {badges.map(b => (
        <span key={b} style={{
          fontSize: '0.62rem', fontWeight: 600, padding: '0.08rem 0.4rem',
          borderRadius: '10px', background: 'rgba(99,179,237,0.12)',
          color: 'rgba(99,179,237,0.9)', border: '1px solid rgba(99,179,237,0.2)',
        }}>{b}</span>
      ))}
    </div>
  )
}

// Map a ModelStatusEntry to the provider string GIAP uses in settings
function providerForEntry(entry: ModelStatusEntry): string {
  if (entry.category === 'llamafile') return 'llamafile'
  if (entry.category === 'gguf')      return 'local'
  return 'ollama'
}

function roleForEntry(entry: ModelStatusEntry, settings: Settings | null, ollamaName?: string): string | null {
  if (!settings) return null
  const provider = ollamaName ? 'ollama' : providerForEntry(entry)
  const name = ollamaName ?? entry.name
  if (settings.chat_provider === provider && settings.chat_model === name) return 'chat'
  if (settings.tool_model === name) return 'tool'
  return null
}

// ── Role Assignment Panel ──────────────────────────────────────────────────────

// ── Download progress bar ─────────────────────────────────────────────────────

function DownloadBar({ entry }: { entry: DownloadEntry }) {
  const pct = entry.total_bytes
    ? Math.min(100, Math.round((entry.downloaded_bytes / entry.total_bytes) * 100))
    : null

  const dlMb    = (entry.downloaded_bytes / 1_048_576).toFixed(1)
  const totalMb = entry.total_bytes ? ` / ${(entry.total_bytes / 1_048_576).toFixed(0)} MB` : ''
  const color   = entry.status === 'error' ? '#e55' : entry.status === 'done' ? '#5c5' : '#a96ff5'

  return (
    <div style={{ marginTop: '0.5rem' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.7rem', opacity: 0.7, marginBottom: '0.2rem' }}>
        <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', maxWidth: '70%' }}>{entry.filename}</span>
        <span>
          {entry.status === 'done'  && 'Done'}
          {entry.status === 'error' && 'Error'}
          {entry.status === 'downloading' && `${dlMb} MB${totalMb}${pct != null ? ` (${pct}%)` : ''}`}
        </span>
      </div>
      <div style={{ height: '6px', borderRadius: '3px', background: 'rgba(128,128,128,0.15)', overflow: 'hidden' }}>
        <div style={{
          height: '100%',
          borderRadius: '3px',
          background: color,
          width: pct != null ? `${pct}%` : '100%',
          transition: 'width 0.4s ease',
          animation: pct == null && entry.status === 'downloading' ? 'pulse 1.5s ease-in-out infinite' : 'none',
          opacity: pct == null ? 0.5 : 1,
        }} />
      </div>
    </div>
  )
}

// ── Face Recognition card ────────────────────────────────────────────────────
//
// Mirrors the visual treatment of the LLM/ASR/TTS category cards but binds
// to GET /api/v1/faces/models — a read-only status panel since the three
// models (ArcFace R50 + SCRFD 10G + Silent-Face PAD) are auto-managed by
// pond-server's boot-time downloader. There is intentionally no per-model
// "Download" button: the buffalo_l zip ships embedder + detector together
// and the antispoof file is only ~2 MB.
function FaceModelsCard({ token }: { token: string }) {
  type FaceModel = { name: string; label: string; role: string; expected_mb: number; size_mb: number | null; downloaded: boolean }
  const [enabled, setEnabled]   = useState<boolean | null>(null)
  const [modelsDir, setModelsDir] = useState<string | null>(null)
  const [models, setModels]     = useState<FaceModel[]>([])
  const [error, setError]       = useState<string | null>(null)

  const reload = useCallback(async () => {
    try {
      const r = await api.listFaceModels(token)
      setEnabled(r.feature_enabled)
      setModelsDir(r.models_dir)
      setModels(r.models)
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load face models')
    }
  }, [token])

  useEffect(() => { void reload() }, [reload])

  const installed = models.filter(m => m.downloaded).length

  return (
    <div className="db-card" style={{ marginTop: '1.25rem' }}>
      <div className="db-card-header">
        <h3>
          Face Recognition
          {installed > 0 && (
            <span style={{ marginLeft: '0.5rem', fontSize: '0.72rem', fontWeight: 600, color: '#4ade80', background: 'rgba(74,222,128,0.12)', border: '1px solid rgba(74,222,128,0.3)', borderRadius: '20px', padding: '0.1rem 0.5rem' }}>
              {installed} of {models.length} installed
            </span>
          )}
          {enabled === false && (
            <span style={{ marginLeft: '0.5rem', fontSize: '0.72rem', fontWeight: 600, color: '#e8a020', background: 'rgba(232,160,32,0.12)', border: '1px solid rgba(232,160,32,0.3)', borderRadius: '20px', padding: '0.1rem 0.5rem' }}>
              feature disabled
            </span>
          )}
        </h3>
        <span style={{ fontSize: '0.75rem', opacity: 0.55 }}>
          ArcFace embeddings + SCRFD detector + Silent-Face anti-spoof. Auto-downloaded on first server boot.
        </span>
      </div>

      {error && <p style={{ fontSize: '0.8rem', color: '#e55', padding: '0.5rem 0' }}>{error}</p>}

      {enabled === false && (
        <p style={{ fontSize: '0.8rem', opacity: 0.7, padding: '0.5rem 0' }}>
          Rebuild pond-server with <code>--features face-onnx</code> to enable per-user identification.
        </p>
      )}

      <div className="db-model-list">
        {models.map(m => (
          <div className="db-model-card" key={m.name} data-downloaded={m.downloaded ? 'true' : 'false'}>
            <div className="db-model-card-info">
              <div className="db-model-card-name">
                {m.label}
                {m.downloaded ? (
                  <span className="db-badge db-badge-green">ready</span>
                ) : (
                  <span className="db-badge" style={{ background: 'rgba(232,160,32,0.18)', color: '#e8a020', border: '1px solid rgba(232,160,32,0.3)' }}>missing</span>
                )}
                <span className="db-badge" style={{ background: 'rgba(169,111,245,0.18)', color: '#a96ff5', border: '1px solid rgba(169,111,245,0.3)' }}>
                  {m.role}
                </span>
              </div>
              <div className="db-model-card-size">
                {m.size_mb != null ? formatSize(m.size_mb) : `~${formatSize(m.expected_mb)} expected`}
                <span style={{ marginLeft: '0.5rem', opacity: 0.55, fontFamily: 'monospace' }}>{m.name}</span>
              </div>
            </div>
          </div>
        ))}
      </div>

      {modelsDir && (
        <p style={{ fontSize: '0.7rem', opacity: 0.45, marginTop: '0.5rem', fontFamily: 'monospace' }}>
          {modelsDir}
        </p>
      )}

      <div style={{ display: 'flex', gap: '0.5rem', marginTop: '0.75rem' }}>
        <a href="#faces" className="db-btn-sm" onClick={(e) => { e.preventDefault(); window.dispatchEvent(new CustomEvent('pond-nav', { detail: 'faces' })) }}>
          Open Face Enrollment →
        </a>
        <button className="db-btn-sm" onClick={() => void reload()}>Refresh</button>
      </div>
    </div>
  )
}

function MemoryBar({ status }: { status: MemoryStatus | null }) {
  if (!status || status.total_mb === 0) return null
  const usedMb   = status.total_mb - status.available_for_llm_mb
  const pct      = Math.min(100, Math.round((usedMb / status.total_mb) * 100))
  const totalGb  = (status.total_mb / 1024).toFixed(1)
  const freeGb   = (status.available_for_llm_mb / 1024).toFixed(1)
  const color    = pct > 85 ? '#e55' : pct > 60 ? '#e8a020' : '#a96ff5'

  return (
    <div style={{ marginTop: '0.75rem' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.72rem', opacity: 0.65, marginBottom: '0.25rem' }}>
        <span>RAM available for LLMs</span>
        <span>{freeGb} / {totalGb} GB free{status.loaded_model ? ` · loaded: ${status.loaded_model}` : ''}</span>
      </div>
      <div style={{ height: '8px', borderRadius: '4px', background: 'rgba(128,128,128,0.15)', overflow: 'hidden' }}>
        <div style={{ height: '100%', width: `${pct}%`, borderRadius: '4px', background: color, transition: 'width 0.4s ease' }} />
      </div>
    </div>
  )
}

// ── Search Panels ─────────────────────────────────────────────────────────────

function HfFileList({ repo, token, onDownloaded, dlProgress }: { repo: string; token: string; onDownloaded: () => void; dlProgress: Record<string, DownloadEntry> }) {
  const [files, setFiles]         = useState<HuggingFaceFile[] | null>(null)
  const [loading, setLoading]     = useState(true)
  const [error, setError]         = useState<string | null>(null)
  const [downloading, setDownloading] = useState<Set<string>>(new Set())
  const [done, setDone]           = useState<Set<string>>(new Set())

  useEffect(() => {
    api.listHfModelFiles(repo, token)
      .then(r => {
        if (r.error) setError(r.error)
        else setFiles(r.files)
      })
      .catch(e => setError(e instanceof Error ? e.message : 'Failed to list files'))
      .finally(() => setLoading(false))
  }, [repo, token])

  async function handleDownload(file: HuggingFaceFile) {
    setDownloading(prev => new Set(prev).add(file.filename))
    try {
      await api.downloadModelFromUrl(file.url, 'gguf', file.filename, token)
      setDone(prev => new Set(prev).add(file.filename))
      onDownloaded()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Download failed')
    } finally {
      setDownloading(prev => { const n = new Set(prev); n.delete(file.filename); return n })
    }
  }

  if (loading) return <p style={{ fontSize: '0.72rem', opacity: 0.5, padding: '0.25rem 0.5rem' }}>Loading files…</p>
  if (error)   return <p style={{ fontSize: '0.72rem', color: '#e55', padding: '0.25rem 0.5rem' }}>{error}</p>
  if (!files?.length) return <p style={{ fontSize: '0.72rem', opacity: 0.5, padding: '0.25rem 0.5rem' }}>No .gguf files found in this repo.</p>

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '0.3rem', paddingTop: '0.3rem' }}>
      {files.map(f => (
        <div key={f.filename} style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', padding: '0.3rem 0.5rem', borderRadius: '5px', background: 'rgba(128,128,128,0.05)' }}>
          <div>
            <div style={{ fontSize: '0.78rem', fontFamily: 'monospace' }}>{f.filename}</div>
            {f.size_mb != null && <div style={{ fontSize: '0.68rem', opacity: 0.5 }}>{formatSize(f.size_mb)}</div>}
          </div>
          {done.has(f.filename) || dlProgress[f.filename]?.status === 'done' ? (
            <span style={{ fontSize: '0.72rem', color: '#5c5', opacity: 0.8 }}>Done ✓</span>
          ) : dlProgress[f.filename]?.status === 'error' ? (
            <span style={{ fontSize: '0.72rem', color: '#e55' }}>Error</span>
          ) : (
            <button
              className="db-btn-sm"
              style={{ fontSize: '0.7rem' }}
              disabled={downloading.has(f.filename) || dlProgress[f.filename]?.status === 'downloading'}
              onClick={() => handleDownload(f)}
            >
              {(downloading.has(f.filename) || dlProgress[f.filename]?.status === 'downloading') ? '…' : 'Download'}
            </button>
          )}
        </div>
      ))}
      {/* Progress bars for active downloads in this file list */}
      {files && files.some(f => dlProgress[f.filename]?.status === 'downloading') && (
        <div style={{ marginTop: '0.5rem', display: 'flex', flexDirection: 'column', gap: '0.25rem' }}>
          {files.filter(f => dlProgress[f.filename]?.status === 'downloading').map(f => (
            <DownloadBar key={f.filename} entry={dlProgress[f.filename]} />
          ))}
        </div>
      )}
    </div>
  )
}

function GgufSearchPanel({ token, onDownloaded, dlProgress }: { token: string; onDownloaded: () => void; dlProgress: Record<string, DownloadEntry> }) {
  const [query, setQuery]         = useState('')
  const [results, setResults]     = useState<HuggingFaceModel[] | null>(null)
  const [loading, setLoading]     = useState(false)
  const [error, setError]         = useState<string | null>(null)
  const [expanded, setExpanded]   = useState<string | null>(null)

  async function doSearch(q: string) {
    setLoading(true); setError(null); setExpanded(null)
    try {
      const r = await api.searchGgufModels(q, token)
      if (r.error) { setError(r.error); setResults([]) }
      else setResults(r.models)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Search failed')
    } finally {
      setLoading(false)
    }
  }

  // Search as user types (debounced 400 ms)
  useEffect(() => {
    const timer = setTimeout(() => doSearch(query), 400)
    return () => clearTimeout(timer)
  }, [query]) // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div>
      <div style={{ position: 'relative', marginBottom: '0.75rem' }}>
        <input
          className="db-settings-input"
          style={{ width: '100%', margin: 0, paddingRight: loading ? '2.5rem' : undefined, boxSizing: 'border-box' }}
          placeholder="Search HuggingFace (e.g. llama, mistral, deepseek)…"
          value={query}
          onChange={e => setQuery(e.target.value)}
        />
        {loading && (
          <span style={{ position: 'absolute', right: '0.75rem', top: '50%', transform: 'translateY(-50%)', opacity: 0.5, fontSize: '0.8rem', pointerEvents: 'none' }}>
            …
          </span>
        )}
      </div>
      {error && <p style={{ fontSize: '0.78rem', color: '#e55', marginBottom: '0.5rem' }}>{error}</p>}
      {results !== null && results.length === 0 && !loading && (
        <p style={{ fontSize: '0.78rem', opacity: 0.55 }}>No results.</p>
      )}
      {results && results.length > 0 && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: '0.5rem', maxHeight: '420px', overflowY: 'auto', paddingRight: '2px' }}>
          {results.map(m => (
            <div key={m.id} style={{ borderRadius: '8px', border: '1px solid rgba(128,128,128,0.14)', background: 'rgba(128,128,128,0.04)' }}>
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', padding: '0.55rem 0.75rem', gap: '0.75rem' }}>
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div style={{ fontSize: '0.82rem', fontWeight: 600, whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>{m.id}</div>
                  <div style={{ fontSize: '0.7rem', opacity: 0.55, marginTop: '0.1rem' }}>
                    {m.downloads?.toLocaleString()} downloads · {m.likes} likes
                  </div>
                </div>
                <div style={{ display: 'flex', gap: '0.35rem', flexShrink: 0 }}>
                  <button
                    className="db-btn-sm"
                    style={{ fontSize: '0.72rem', background: expanded === m.id ? 'rgba(169,111,245,0.2)' : undefined }}
                    onClick={() => setExpanded(expanded === m.id ? null : m.id)}
                  >
                    {expanded === m.id ? 'Hide' : 'Files'}
                  </button>
                  <a
                    href={m.url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="db-btn-sm"
                    style={{ textDecoration: 'none', fontSize: '0.72rem' }}
                  >
                    HF ↗
                  </a>
                </div>
              </div>
              {expanded === m.id && (
                <div style={{ borderTop: '1px solid rgba(128,128,128,0.1)', padding: '0.5rem 0.75rem' }}>
                  <HfFileList repo={m.id} token={token} onDownloaded={onDownloaded} dlProgress={dlProgress} />
                </div>
              )}
            </div>
          ))}
        </div>
      )}
      {results === null && !loading && (
        <p style={{ fontSize: '0.72rem', opacity: 0.45 }}>
          Type to search HuggingFace. Click "Files" on a result to pick and download a specific .gguf file directly.
        </p>
      )}
    </div>
  )
}

function LlamafileSearchPanel({ token }: { token: string }) {
  const [query, setQuery]       = useState('')
  const [results, setResults]   = useState<LlamafileAsset[] | null>(null)
  const [loading, setLoading]   = useState(false)
  const [error, setError]       = useState<string | null>(null)

  async function doSearch(q: string) {
    setLoading(true); setError(null)
    try {
      const r = await api.searchLlamafileModels(q, token)
      if (r.error) { setError(r.error); setResults([]) }
      else setResults(r.models)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Search failed')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { doSearch('') }, []) // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div>
      <div style={{ display: 'flex', gap: '0.5rem', marginBottom: '0.75rem' }}>
        <input
          className="db-settings-input"
          style={{ flex: 1, margin: 0 }}
          placeholder="Filter by name (e.g. llama, gemma)…"
          value={query}
          onChange={e => setQuery(e.target.value)}
          onKeyDown={e => e.key === 'Enter' && doSearch(query)}
        />
        <button className="db-btn-sm" onClick={() => doSearch(query)} disabled={loading}>
          {loading ? '…' : 'Filter'}
        </button>
      </div>
      {error && <p style={{ fontSize: '0.78rem', color: '#e55', marginBottom: '0.5rem' }}>{error}</p>}
      {results !== null && results.length === 0 && !loading && (
        <p style={{ fontSize: '0.78rem', opacity: 0.55 }}>No llamafile releases found.</p>
      )}
      {results && results.length > 0 && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: '0.4rem', maxHeight: '320px', overflowY: 'auto' }}>
          {results.map(a => (
            <div key={a.url} style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', padding: '0.4rem 0.5rem', borderRadius: '6px', background: 'rgba(128,128,128,0.06)' }}>
              <div>
                <div style={{ fontSize: '0.82rem', fontWeight: 600 }}>{a.name}</div>
                <div style={{ fontSize: '0.7rem', opacity: 0.55 }}>
                  {a.version} · {formatSize(a.size_mb)}
                </div>
              </div>
              <div style={{ display: 'flex', gap: '0.35rem' }}>
                <a
                  href={a.url}
                  className="db-btn-sm"
                  style={{ textDecoration: 'none', fontSize: '0.72rem' }}
                >
                  Download ↓
                </a>
                <a
                  href={a.release_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="db-btn-sm"
                  style={{ textDecoration: 'none', fontSize: '0.72rem' }}
                >
                  Release ↗
                </a>
              </div>
            </div>
          ))}
        </div>
      )}
      <p style={{ fontSize: '0.72rem', opacity: 0.45, marginTop: '0.5rem' }}>
        Place downloaded <code>.llamafile</code> files in <code>$DATA_DIR/models/llm/</code> then refresh the model list above.
      </p>
    </div>
  )
}

function OllamaSearchPanel({ token, onPull }: { token: string; onPull: () => void }) {
  const [modelName, setModelName] = useState('')
  const [pulling, setPulling]     = useState(false)
  const [pullMsg, setPullMsg]     = useState<string | null>(null)

  async function handlePull() {
    if (!modelName.trim()) return
    setPulling(true); setPullMsg(null)
    try {
      await api.pullOllamaModel(modelName.trim(), token)
      setPullMsg(`Pulling "${modelName}" — this runs in the background. Refresh in a few minutes.`)
      onPull()
    } catch (e) {
      setPullMsg(e instanceof Error ? e.message : 'Pull failed')
    } finally {
      setPulling(false)
    }
  }

  return (
    <div>
      <p style={{ fontSize: '0.78rem', opacity: 0.65, marginBottom: '0.5rem' }}>
        Browse the{' '}
        <a href="https://ollama.com/library" target="_blank" rel="noopener noreferrer" style={{ color: '#a96ff5' }}>
          Ollama Library ↗
        </a>
        {' '}then pull a model by name:
      </p>
      <div style={{ display: 'flex', gap: '0.5rem', marginBottom: '0.5rem' }}>
        <input
          className="db-settings-input"
          style={{ flex: 1, margin: 0 }}
          placeholder="e.g. llama3.2, deepseek-r1, mistral"
          value={modelName}
          onChange={e => setModelName(e.target.value)}
          onKeyDown={e => e.key === 'Enter' && handlePull()}
        />
        <button className="db-btn-sm" onClick={handlePull} disabled={pulling || !modelName.trim()}>
          {pulling ? 'Pulling…' : 'Pull'}
        </button>
      </div>
      {pullMsg && <p style={{ fontSize: '0.78rem', opacity: 0.75 }}>{pullMsg}</p>}
      <p style={{ fontSize: '0.72rem', opacity: 0.45 }}>
        Requires <code>ollama</code> to be installed and running. The pull runs server-side in the background.
      </p>
    </div>
  )
}

type ActiveRolesResponse = {
  chat:  { provider: string; model: string }
  think: { provider: string | null; model: string | null }
  task:  { provider: string | null; model: string | null }
  asr:   { model_id: string | null }
  tts:   { model_id: string | null }
  router_name: string
}

function RolePanel({
  token,
  settings,
  memory,
  onAssignRole,
  onRefresh,
}: {
  token: string
  settings: Settings | null
  memory: MemoryStatus | null
  onAssignRole: (role: 'chat' | 'think' | 'task', provider: string | null, model: string | null) => void
  onRefresh?: () => void
}) {
  const llmRoles: Array<'chat' | 'think' | 'task'> = ['chat', 'think', 'task']
  const [activeRoles, setActiveRoles] = useState<ActiveRolesResponse | null>(null)
  const [justUpdated, setJustUpdated] = useState<string | null>(null)
  const [caps, setCaps] = useState<{ thinking: boolean; vision: boolean; audio_input: boolean; context_window_tokens: number; structured_output: boolean } | null>(null)

  const reload = () => {
    if (!token || token === 'dev-mock-token') return
    api.getActiveRoles(token).then(setActiveRoles).catch(() => {})
    api.getModelCapabilities(token).then(setCaps).catch(() => {})
  }

  useEffect(reload, [token, settings]) // eslint-disable-line react-hooks/exhaustive-deps

  function getLlmAssignment(role: 'chat' | 'think' | 'task') {
    if (activeRoles) {
      return { provider: activeRoles[role]?.provider || null, model: activeRoles[role]?.model || null }
    }
    if (!settings) return { provider: null, model: null }
    const p = settings[`${role}_provider`] as string | null
    const m = settings[`${role}_model`]    as string | null
    return { provider: p || null, model: m || null }
  }

  async function handleClear(role: 'chat' | 'think' | 'task') {
    await onAssignRole(role, null, null)
    setJustUpdated(role)
    setTimeout(() => setJustUpdated(null), 2000)
    reload()
  }

  async function handleClearMediaRole(role: 'asr' | 'tts') {
    try {
      await api.saveSettings(
        role === 'asr'
          ? { active_whisper_model: '' }
          : { active_tts_model: '' },
        token,
      )
      setJustUpdated(role)
      setTimeout(() => setJustUpdated(null), 2000)
      reload()
      onRefresh?.()
    } catch { /* ignore */ }
  }

  const ROLE_DESC: Record<string, string> = {
    chat:  'General conversation, quick answers',
    think: 'Deep reasoning, analysis, explanations',
    task:  'Actions, scheduling, device control',
    asr:   'Speech-to-text (Whisper)',
    tts:   'Text-to-speech (Piper)',
  }

  const ALL_ROLE_ICONS: Record<string, string> = {
    ...ROLE_ICONS, asr: '🎙', tts: '🔊',
  }
  const ALL_ROLE_LABELS: Record<string, string> = {
    ...ROLE_LABELS, asr: 'ASR', tts: 'TTS',
  }

  // Extract model name from model_id ("whisper/base" → "base")
  const modelName = (modelId: string | null | undefined) =>
    modelId ? (modelId.split('/')[1] ?? modelId) : null

  return (
    <div className="db-card" style={{ marginBottom: '1.25rem' }}>
      <div className="db-card-header">
        <h3>
          AI Pipeline
          <span style={{ marginLeft: '0.5rem', fontSize: '0.68rem', fontWeight: 600, color: '#4ade80', background: 'rgba(74,222,128,0.1)', border: '1px solid rgba(74,222,128,0.25)', borderRadius: '20px', padding: '0.1rem 0.5rem' }}>
            ● Live
          </span>
        </h3>
        <span style={{ fontSize: '0.75rem', opacity: 0.55 }}>
          Active model assignments across all roles.
        </span>
      </div>

      <div style={{ display: 'flex', flexDirection: 'column', gap: '0.5rem' }}>
        {/* LLM roles */}
        {llmRoles.map(role => {
          const { provider, model } = getLlmAssignment(role)
          const isSet = !!(provider || model)
          const isUpdated = justUpdated === role
          return (
            <div key={role} className="db-role-row" style={{
              display: 'flex', alignItems: 'center', gap: '0.75rem',
              padding: '0.6rem 0.75rem', borderRadius: '8px',
              border: `1px solid ${isSet ? 'rgba(169,111,245,0.25)' : 'rgba(128,128,128,0.12)'}`,
              background: isSet ? 'rgba(169,111,245,0.04)' : 'rgba(128,128,128,0.03)',
            }}>
              <div style={{ width: '4.5rem', flexShrink: 0 }}>
                <div style={{ fontSize: '0.85rem', fontWeight: 700 }}>{ALL_ROLE_ICONS[role]} {ALL_ROLE_LABELS[role]}</div>
                <div style={{ fontSize: '0.65rem', opacity: 0.45, marginTop: '0.1rem' }}>{ROLE_DESC[role]}</div>
              </div>
              <div style={{ flex: 1, minWidth: 0 }}>
                {isSet ? (
                  <>
                    <div style={{ fontSize: '0.82rem', fontWeight: 600, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                      {model ?? <span style={{ opacity: 0.5 }}>any model</span>}
                    </div>
                    <div style={{ fontSize: '0.7rem', opacity: 0.5 }}>{provider}</div>
                  </>
                ) : (
                  <span style={{ fontSize: '0.78rem', opacity: 0.35, fontStyle: 'italic' }}>
                    {role === 'chat' ? 'Not configured' : 'Falls back to Chat'}
                  </span>
                )}
              </div>
              <div style={{ flexShrink: 0, display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
                {isUpdated && <span style={{ fontSize: '0.7rem', color: '#4ade80' }}>Updated ✓</span>}
                {isSet && !isUpdated && (
                  <button className="db-btn-sm" style={{ fontSize: '0.68rem', padding: '0.15rem 0.5rem' }} onClick={() => handleClear(role)}>
                    Clear
                  </button>
                )}
              </div>
            </div>
          )
        })}

        {/* Media roles: ASR + TTS */}
        {(['asr', 'tts'] as const).map(role => {
          const modelId = activeRoles?.[role]?.model_id
            ?? (role === 'asr' ? settings?.active_whisper_model : settings?.active_tts_model)
            ?? null
          const name = modelName(modelId)
          const isSet = !!name
          const isUpdated = justUpdated === role
          return (
            <div key={role} className="db-role-row" style={{
              display: 'flex', alignItems: 'center', gap: '0.75rem',
              padding: '0.6rem 0.75rem', borderRadius: '8px',
              border: `1px solid ${isSet ? 'rgba(74,222,128,0.2)' : 'rgba(128,128,128,0.12)'}`,
              background: isSet ? 'rgba(74,222,128,0.03)' : 'rgba(128,128,128,0.03)',
            }}>
              <div style={{ width: '4.5rem', flexShrink: 0 }}>
                <div style={{ fontSize: '0.85rem', fontWeight: 700 }}>{ALL_ROLE_ICONS[role]} {ALL_ROLE_LABELS[role]}</div>
                <div style={{ fontSize: '0.65rem', opacity: 0.45, marginTop: '0.1rem' }}>{ROLE_DESC[role]}</div>
              </div>
              <div style={{ flex: 1, minWidth: 0 }}>
                {isSet ? (
                  <div style={{ fontSize: '0.82rem', fontWeight: 600, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {name}
                  </div>
                ) : (
                  <span style={{ fontSize: '0.78rem', opacity: 0.35, fontStyle: 'italic' }}>Not configured</span>
                )}
              </div>
              <div style={{ flexShrink: 0, display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
                {isUpdated && <span style={{ fontSize: '0.7rem', color: '#4ade80' }}>Updated ✓</span>}
                {isSet && !isUpdated && (
                  <button className="db-btn-sm" style={{ fontSize: '0.68rem', padding: '0.15rem 0.5rem' }} onClick={() => handleClearMediaRole(role)}>
                    Clear
                  </button>
                )}
              </div>
            </div>
          )
        })}
      </div>

      <MemoryBar status={memory} />

      {/* Active model capabilities */}
      {caps && (caps.thinking || caps.vision || caps.audio_input || caps.context_window_tokens > 4096) && (
        <div style={{
          display: 'flex', gap: '0.35rem', flexWrap: 'wrap', alignItems: 'center',
          marginTop: '0.6rem', padding: '0.5rem 0.6rem',
          borderRadius: '6px', background: 'rgba(99,179,237,0.06)', border: '1px solid rgba(99,179,237,0.15)',
        }}>
          <span style={{ fontSize: '0.7rem', opacity: 0.55, marginRight: '0.25rem' }}>Active model features:</span>
          {caps.thinking && <span style={{ fontSize: '0.62rem', fontWeight: 600, padding: '0.08rem 0.4rem', borderRadius: '10px', background: 'rgba(99,179,237,0.15)', color: 'rgba(99,179,237,0.9)' }}>Thinking</span>}
          {caps.vision && <span style={{ fontSize: '0.62rem', fontWeight: 600, padding: '0.08rem 0.4rem', borderRadius: '10px', background: 'rgba(99,179,237,0.15)', color: 'rgba(99,179,237,0.9)' }}>Vision</span>}
          {caps.audio_input && <span style={{ fontSize: '0.62rem', fontWeight: 600, padding: '0.08rem 0.4rem', borderRadius: '10px', background: 'rgba(99,179,237,0.15)', color: 'rgba(99,179,237,0.9)' }}>Audio</span>}
          {caps.structured_output && <span style={{ fontSize: '0.62rem', fontWeight: 600, padding: '0.08rem 0.4rem', borderRadius: '10px', background: 'rgba(99,179,237,0.15)', color: 'rgba(99,179,237,0.9)' }}>Structured Output</span>}
          {caps.context_window_tokens > 4096 && <span style={{ fontSize: '0.62rem', fontWeight: 600, padding: '0.08rem 0.4rem', borderRadius: '10px', background: 'rgba(99,179,237,0.15)', color: 'rgba(99,179,237,0.9)' }}>{Math.round(caps.context_window_tokens / 1000)}k context</span>}
        </div>
      )}

      <p style={{ fontSize: '0.72rem', opacity: 0.4, marginTop: '0.6rem' }}>
        Assign models using the {ROLE_ICONS['chat']}{ROLE_ICONS['think']}{ROLE_ICONS['task']} buttons on installed model cards below. Changes take effect immediately — no restart needed.
      </p>
    </div>
  )
}

// ── Model Card ────────────────────────────────────────────────────────────────

function ModelCard({
  entry,
  onDownload,
  onDelete,
  downloading,
  deleting,
  currentRole,
  onAssignRole,
  onActivateMediaRole,
  progressEntry,
}: {
  entry: ModelStatusEntry
  onDownload: (category: string, name: string) => void
  onDelete: (category: string, name: string) => void
  downloading: boolean
  deleting: boolean
  currentRole: string | null
  onAssignRole: ((role: 'chat' | 'think' | 'task') => void) | null
  onActivateMediaRole: ((role: 'asr' | 'tts') => void) | null
  progressEntry?: DownloadEntry
}) {
  const roles: Array<'chat' | 'think' | 'task'> = ['chat', 'think', 'task']

  return (
    <div
      className="db-model-card"
      data-downloaded={entry.downloaded ? 'true' : 'false'}
      data-active={entry.active ? 'true' : 'false'}
    >
      <div className="db-model-card-info">
        <div className="db-model-card-name">
          {entry.name}
          {currentRole && (
            <span
              className="db-badge"
              style={{ background: 'rgba(169,111,245,0.18)', color: '#a96ff5', border: '1px solid rgba(169,111,245,0.3)' }}
            >
              {ROLE_ICONS[currentRole]} {ROLE_LABELS[currentRole]}
            </span>
          )}
        </div>
        {entry.hf_id && <div className="db-model-card-hf">{entry.hf_id}</div>}
        <div className="db-model-card-desc">{entry.description}</div>
        <div className="db-model-card-size" style={{ display: 'flex', gap: '0.75rem', flexWrap: 'wrap' }}>
          <span>{formatSize(entry.size_mb)}</span>
          {entry.ram_estimate_mb && <span style={{ opacity: 0.65 }}>~{formatSize(entry.ram_estimate_mb)} RAM</span>}
          {entry.recommended_role && (
            <span style={{ opacity: 0.65 }}>
              Recommended: {ROLE_ICONS[entry.recommended_role]} {ROLE_LABELS[entry.recommended_role] ?? entry.recommended_role}
            </span>
          )}
        </div>
        <WebCapabilityBadges name={entry.name} />
      </div>
      <div className="db-model-card-actions">
        {/* Status indicator */}
        {entry.active ? (
          <span className="db-model-card-active">Active</span>
        ) : entry.downloaded ? (
          <span className="db-model-card-installed">Installed</span>
        ) : entry.url ? (
          <button
            className="db-btn-sm"
            disabled={downloading || progressEntry?.status === 'downloading'}
            onClick={() => onDownload(entry.category, entry.name)}
          >
            {(downloading || progressEntry?.status === 'downloading') ? 'Downloading…' : 'Download'}
          </button>
        ) : (
          <span className="db-model-card-no-dl">Server-based</span>
        )}

        {/* Role assignment pills — only for installed LLM models */}
        {onAssignRole && entry.downloaded && (
          <div style={{ display: 'flex', gap: '0.3rem', flexWrap: 'wrap', justifyContent: 'flex-end' }}>
            {roles.map(role => (
              <button
                key={role}
                className="db-btn-sm"
                title={`Assign to ${ROLE_LABELS[role]}`}
                onClick={() => onAssignRole(role)}
                style={{
                  fontSize: '0.7rem',
                  padding: '0.15rem 0.45rem',
                  background: currentRole === role ? 'rgba(169,111,245,0.25)' : undefined,
                  border: currentRole === role ? '1px solid rgba(169,111,245,0.5)' : undefined,
                }}
              >
                {ROLE_ICONS[role]}
              </button>
            ))}
          </div>
        )}

        {/* Media role activate button — whisper → asr, tts → tts */}
        {onActivateMediaRole && entry.downloaded && (
          <button
            className="db-btn-sm"
            title={`Set as active ${entry.category === 'whisper' ? 'ASR' : 'TTS'} model`}
            onClick={() => onActivateMediaRole(entry.category === 'whisper' ? 'asr' : 'tts')}
            style={{
              fontSize: '0.7rem', padding: '0.15rem 0.45rem',
              background: entry.active ? 'rgba(74,222,128,0.2)' : undefined,
              border: entry.active ? '1px solid rgba(74,222,128,0.4)' : undefined,
            }}
          >
            {entry.category === 'whisper' ? '🎙' : '🔊'}
          </button>
        )}

        {/* Delete button — shown for downloaded models only */}
        {entry.downloaded && (
          <button
            className="db-btn-sm"
            title={entry.active ? 'Deactivate this model before deleting' : 'Delete model file from disk'}
            disabled={entry.active || deleting}
            onClick={() => {
              if (!window.confirm(`Delete "${entry.filename ?? entry.name}" from disk?\nThe catalog entry will be kept.`)) return
              onDelete(entry.category, entry.name)
            }}
            style={{ fontSize: '0.72rem', opacity: entry.active ? 0.4 : 1, color: deleting ? undefined : '#e55' }}
          >
            {deleting ? '…' : '🗑'}
          </button>
        )}

        {progressEntry && <DownloadBar entry={progressEntry} />}
      </div>
    </div>
  )
}

// ── Page ───────────────────────────────────────────────────────────────────────

export default function Models({ token }: Props) {
  const [models, setModels] = useState<{
    whisper: ModelStatusEntry[]
    llamafile: ModelStatusEntry[]
    gguf: ModelStatusEntry[]
    tts: ModelStatusEntry[]
  } | null>(null)

  const [ollama, setOllama]             = useState<OllamaModel[] | null>(null)
  const [ollamaError, setOllamaError]   = useState<string | null>(null)
  const [loading, setLoading]           = useState(true)
  const [error, setError]               = useState<string | null>(null)
  const [downloading, setDownloading]   = useState<Set<string>>(new Set())
  const [deleting, setDeleting]         = useState<Set<string>>(new Set())
  const [refreshing, setRefreshing]     = useState(false)
  const [refreshMsg, setRefreshMsg]     = useState<string | null>(null)
  const [scanning, setScanning]         = useState(false)
  const [settings, setSettings]         = useState<Settings | null>(null)
  const [memory, setMemory]             = useState<MemoryStatus | null>(null)
  const [dlProgress, setDlProgress]     = useState<Record<string, DownloadEntry>>({})
  const memoryTimerRef                  = useRef<ReturnType<typeof setInterval> | null>(null)
  const progressTimerRef                = useRef<ReturnType<typeof setInterval> | null>(null)

  const load = useCallback(async () => {
    if (isPreviewMode(token)) {
      setModels({ whisper: [], llamafile: [], gguf: [], tts: [] })
      setLoading(false)
      return
    }
    try {
      const m = await api.listModels(token)
      setModels(m)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load models.')
    } finally {
      setLoading(false)
    }
  }, [token])

  useEffect(() => {
    load()
    if (isPreviewMode(token)) return

    // Load settings for role assignments
    api.getSettings(token).then(setSettings).catch(() => {})

    // Probe Ollama
    api.listOllamaModels(token)
      .then(r => {
        if (r.error) setOllamaError(r.error)
        else setOllama(r.models)
      })
      .catch(() => setOllamaError('Could not reach backend'))

    // Poll memory status every 5s
    const fetchMemory = () => {
      api.getMemoryStatus(token).then(setMemory).catch(() => {})
    }
    fetchMemory()
    memoryTimerRef.current = setInterval(fetchMemory, 5000)

    // Poll download progress every second; stop when none are active
    const fetchProgress = async () => {
      try {
        const r = await api.getDownloadProgress(token)
        const map: Record<string, DownloadEntry> = {}
        for (const d of r.downloads) map[d.filename] = d
        setDlProgress(map)
        // Refresh model list when a download just finished
        const anyDone = r.downloads.some(d => d.status === 'done')
        if (anyDone) load()
      } catch { /* ignore */ }
    }
    progressTimerRef.current = setInterval(fetchProgress, 1000)

    return () => {
      if (memoryTimerRef.current)   clearInterval(memoryTimerRef.current)
      if (progressTimerRef.current) clearInterval(progressTimerRef.current)
    }
  }, [load, token])

  async function handleAssignRole(
    role: 'chat' | 'think' | 'task',
    provider: string | null,
    model: string | null,
  ) {
    // Prefer the new activate endpoint (persists to join table + hot-reloads ModelRouter).
    // Fall back to settings PUT when provider/model is null (clearing a role).
    if (provider !== null && model !== null) {
      try {
        await api.activateModel(provider === 'local' ? 'gguf' : provider, model, role, token)
        // Refresh settings to reflect the new assignment
        const fresh = await api.getSettings(token)
        setSettings(fresh)
        return
      } catch {
        // fall through to saveSettings
      }
    }
    const update: Record<string, string | null> = {
      [`${role}_provider`]: provider,
      [`${role}_model`]:    model,
    }
    try {
      await api.saveSettings(update, token)
      setSettings(prev => prev ? { ...prev, ...update } as Settings : prev)
    } catch {
      // silently ignore; the role panel just won't update
    }
  }

  async function handleActivateMediaRole(category: string, name: string, role: 'asr' | 'tts') {
    try {
      await api.activateModel(category, name, role, token)
      await load()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Activate failed.')
    }
  }

  async function handleDelete(category: string, name: string) {
    const key = `${category}/${name}`
    setDeleting(prev => new Set(prev).add(key))
    try {
      await api.deleteModel(category, name, token)
      await load()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Delete failed.')
    } finally {
      setDeleting(prev => { const next = new Set(prev); next.delete(key); return next })
    }
  }

  async function handleDownload(category: string, name: string) {
    const key = `${category}/${name}`
    setDownloading(prev => new Set(prev).add(key))
    try {
      await api.downloadModel(category, name, token)
      let attempts = 0
      const interval = setInterval(async () => {
        attempts++
        const refreshed = await api.listModels(token)
        const catList = (refreshed as unknown as Record<string, ModelStatusEntry[]>)[category]
        const entry = catList?.find(e => e.name === name)
        if (entry?.downloaded || attempts > 100) {
          clearInterval(interval)
          setModels(refreshed)
          setDownloading(prev => { const next = new Set(prev); next.delete(key); return next })
        }
      }, 3000)
    } catch (e) {
      setDownloading(prev => { const next = new Set(prev); next.delete(key); return next })
      setError(e instanceof Error ? e.message : 'Download failed.')
    }
  }

  async function handleRefresh() {
    setRefreshing(true)
    setRefreshMsg(null)
    try {
      await api.refreshModelRegistry(token)
      await load()
      setRefreshMsg('Registry refreshed.')
      setTimeout(() => setRefreshMsg(null), 3000)
    } catch (e) {
      setRefreshMsg(e instanceof Error ? e.message : 'Refresh failed.')
    } finally {
      setRefreshing(false)
    }
  }

  async function handleScan() {
    setScanning(true)
    setRefreshMsg(null)
    try {
      const r = await api.scanModels(token)
      await load()
      setRefreshMsg(r.found > 0 ? `Found ${r.found} new file${r.found === 1 ? '' : 's'}.` : 'No new files found.')
      setTimeout(() => setRefreshMsg(null), 4000)
    } catch (e) {
      setRefreshMsg(e instanceof Error ? e.message : 'Scan failed.')
    } finally {
      setScanning(false)
    }
  }

  const categories = ['gguf', 'llamafile', 'whisper', 'tts'] as const

  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">Models</h1>
        <p className="db-page-subtitle">Download and manage on-device AI models.</p>
      </div>

      <div className="db-page-content">
        {/* Role assignment panel */}
        <RolePanel
          token={token}
          settings={settings}
          memory={memory}
          onAssignRole={handleAssignRole}
          onRefresh={load}
        />

        <div className="db-models-toolbar" style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: '0.75rem', gap: '0.5rem', alignItems: 'center' }}>
          {refreshMsg && <span style={{ fontSize: '0.8rem', opacity: 0.7 }}>{refreshMsg}</span>}
          <button
            className="db-btn-sm"
            onClick={handleScan}
            disabled={scanning || refreshing}
            title="Scan model directories for files not yet in the registry"
          >
            {scanning ? 'Scanning…' : 'Scan Files'}
          </button>
          <button className="db-btn-sm" onClick={handleRefresh} disabled={refreshing || scanning}>
            {refreshing ? 'Refreshing…' : 'Refresh Registry'}
          </button>
        </div>

        {error && <p className="db-error">{error}</p>}

        {loading ? (
          <p style={{ opacity: 0.6, textAlign: 'center', paddingTop: '2rem' }}>Loading models…</p>
        ) : (
          <>
            {categories.map(cat => {
              const raw    = models?.[cat] ?? []
              const isLlm  = cat === 'llamafile' || cat === 'gguf'
              // Sort: active first, then installed, then available
              const sorted = [...raw].sort((a, b) => {
                const rank = (e: ModelStatusEntry) => e.active ? 0 : e.downloaded ? 1 : 2
                return rank(a) - rank(b)
              })
              const installed  = sorted.filter(e => e.downloaded)
              const available  = sorted.filter(e => !e.downloaded)

              return (
                <div className="db-card" key={cat} style={{ marginBottom: '1.25rem' }}>
                  <div className="db-card-header">
                    <h3>
                      {CATEGORY_LABELS[cat]}
                      {installed.length > 0 && (
                        <span style={{ marginLeft: '0.5rem', fontSize: '0.72rem', fontWeight: 600, color: '#4ade80', background: 'rgba(74,222,128,0.12)', border: '1px solid rgba(74,222,128,0.3)', borderRadius: '20px', padding: '0.1rem 0.5rem' }}>
                          {installed.length} installed
                        </span>
                      )}
                    </h3>
                    <span style={{ fontSize: '0.75rem', opacity: 0.55 }}>{CATEGORY_DESC[cat]}</span>
                  </div>

                  {/* Search/discover panel for GGUF — shown at top */}
                  {cat === 'gguf' && (
                    <div style={{ marginBottom: '1rem' }}>
                      <p style={{ fontSize: '0.75rem', fontWeight: 600, marginBottom: '0.5rem', opacity: 0.7 }}>Search HuggingFace</p>
                      <GgufSearchPanel token={token} onDownloaded={load} dlProgress={dlProgress} />
                    </div>
                  )}

                  {raw.length === 0 && cat !== 'gguf' && (
                    <p style={{ fontSize: '0.8rem', opacity: 0.55, padding: '0.75rem 0' }}>No models in registry.</p>
                  )}

                  {/* Installed section */}
                  {installed.length > 0 && (
                    <>
                      <div className="db-model-section-label">Installed</div>
                      <div className="db-model-list" style={{ marginBottom: available.length > 0 ? '0.75rem' : 0 }}>
                        {installed.map(entry => {
                          const curRole = isLlm ? roleForEntry(entry, settings) : null
                          return (
                            <ModelCard
                              key={entry.name}
                              entry={entry}
                              downloading={downloading.has(`${entry.category}/${entry.name}`)}
                              deleting={deleting.has(`${entry.category}/${entry.name}`)}
                              onDownload={handleDownload}
                              onDelete={handleDelete}
                              currentRole={curRole}
                              onAssignRole={isLlm ? (role) => handleAssignRole(role, providerForEntry(entry), entry.name) : null}
                              onActivateMediaRole={
                                (cat === 'whisper' || cat === 'tts')
                                  ? (role) => handleActivateMediaRole(entry.category, entry.name, role)
                                  : null
                              }
                              progressEntry={entry.filename ? dlProgress[entry.filename] : undefined}
                            />
                          )
                        })}
                      </div>
                    </>
                  )}

                  {/* Available / not-yet-downloaded section */}
                  {available.length > 0 && (
                    <>
                      <div className="db-model-section-label">Available</div>
                      <div className="db-model-list">
                        {available.map(entry => {
                          const curRole = isLlm ? roleForEntry(entry, settings) : null
                          return (
                            <ModelCard
                              key={entry.name}
                              entry={entry}
                              downloading={downloading.has(`${entry.category}/${entry.name}`)}
                              deleting={deleting.has(`${entry.category}/${entry.name}`)}
                              onDownload={handleDownload}
                              onDelete={handleDelete}
                              currentRole={curRole}
                              onAssignRole={isLlm ? (role) => handleAssignRole(role, providerForEntry(entry), entry.name) : null}
                              onActivateMediaRole={
                                (cat === 'whisper' || cat === 'tts')
                                  ? (role) => handleActivateMediaRole(entry.category, entry.name, role)
                                  : null
                              }
                              progressEntry={entry.filename ? dlProgress[entry.filename] : undefined}
                            />
                          )
                        })}
                      </div>
                    </>
                  )}

                  {cat === 'llamafile' && (
                    <div style={{ borderTop: raw.length > 0 ? '1px solid rgba(128,128,128,0.12)' : 'none', paddingTop: '0.85rem', marginTop: raw.length > 0 ? '0.5rem' : 0 }}>
                      <p style={{ fontSize: '0.75rem', fontWeight: 600, marginBottom: '0.5rem', opacity: 0.7 }}>Browse llamafile releases from GitHub</p>
                      <LlamafileSearchPanel token={token} />
                    </div>
                  )}
                </div>
              )
            })}

            {/* Ollama section */}
            <div className="db-card">
              <div className="db-card-header">
                <h3>Ollama</h3>
                <span style={{ fontSize: '0.75rem', opacity: 0.55 }}>
                  Models installed in a local Ollama instance.
                </span>
              </div>
              {/* Pull new model */}
              <div style={{ marginBottom: '0.75rem' }}>
                <OllamaSearchPanel token={token} onPull={() => {
                  api.listOllamaModels(token)
                    .then(r => { if (!r.error) setOllama(r.models) })
                    .catch(() => {})
                }} />
              </div>
              {/* Installed models */}
              {ollamaError ? (
                <p style={{ fontSize: '0.8rem', opacity: 0.55, padding: '0.75rem 0' }}>{ollamaError}</p>
              ) : ollama === null ? (
                <p style={{ fontSize: '0.8rem', opacity: 0.55, padding: '0.75rem 0' }}>Probing Ollama…</p>
              ) : ollama.length === 0 ? (
                <p style={{ fontSize: '0.8rem', opacity: 0.55, padding: '0.75rem 0' }}>No models installed. Pull one above.</p>
              ) : (
                <div className="db-model-list">
                  {ollama.map(m => {
                    const curRole = roleForEntry({ name: m.name } as ModelStatusEntry, settings, m.name)
                    return (
                      <div className="db-model-card" key={m.name} data-downloaded="true">
                        <div className="db-model-card-info">
                          <div className="db-model-card-name">
                            {m.name}
                            <span className="db-badge db-badge-green">ready</span>
                            {curRole && (
                              <span
                                className="db-badge"
                                style={{ background: 'rgba(169,111,245,0.18)', color: '#a96ff5', border: '1px solid rgba(169,111,245,0.3)' }}
                              >
                                {ROLE_ICONS[curRole]} {ROLE_LABELS[curRole]}
                              </span>
                            )}
                          </div>
                          <div className="db-model-card-size">{formatSize(Math.round(m.size / (1024 * 1024)))}</div>
                        </div>
                        <div className="db-model-card-actions">
                          <div style={{ display: 'flex', gap: '0.3rem', flexWrap: 'wrap', justifyContent: 'flex-end' }}>
                            {(['chat', 'think', 'task'] as const).map(role => (
                              <button
                                key={role}
                                className="db-btn-sm"
                                title={`Assign to ${ROLE_LABELS[role]}`}
                                onClick={() => handleAssignRole(role, 'ollama', m.name)}
                                style={{
                                  fontSize: '0.7rem',
                                  padding: '0.15rem 0.45rem',
                                  background: curRole === role ? 'rgba(169,111,245,0.25)' : undefined,
                                  border: curRole === role ? '1px solid rgba(169,111,245,0.5)' : undefined,
                                }}
                              >
                                {ROLE_ICONS[role]}
                              </button>
                            ))}
                          </div>
                        </div>
                      </div>
                    )
                  })}
                </div>
              )}
            </div>

            {/* Face Recognition (auto-managed; read-only status) */}
            <FaceModelsCard token={token} />
          </>
        )}
      </div>
    </div>
  )
}
