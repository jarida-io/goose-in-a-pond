// Support Tauri desktop app: main.rs injects window.__GIAP_SERVER_URL__
// before the React app loads, allowing the desktop app to connect to any
// pond-server (local or remote Jetson). The web dashboard uses relative paths.
declare global {
  interface Window {
    __GIAP_SERVER_URL__?: string
  }
}

const BASE = window.__GIAP_SERVER_URL__
  ? `${window.__GIAP_SERVER_URL__.replace(/\/$/, '')}/api/v1`
  : '/api/v1'

export const DEV_MOCK_TOKEN = 'dev-mock-token'
export const isPreviewMode = (token: string) => token === DEV_MOCK_TOKEN

// ── Types matching pond-core domain ──────────────────────────────────────────

export interface HandshakeRequest {
  client_id: string
  client_type: string
  client_version: string
  pairing_code?: string
}

export interface HandshakeResponse {
  accepted: boolean
  session_token: string | null
  hostname: string
  server_version: string
  capabilities: string[]
  rejection_reason: string | null
}

export interface OnboardingStatus {
  status: string
  onboarded: boolean
  steps_completed: number
  total_steps: number
}

export interface SessionSummary {
  id: string
  title: string | null
  created_at: string
  updated_at: string
}

export interface SessionMessage {
  id: string
  session_id: string
  role: 'user' | 'assistant' | 'system'
  content: string
  created_at: string
}

export interface ScheduledTask {
  id: string
  label: string
  cron: string
  last_run: string | null
  next_run: string | null
  paused: boolean
  currently_running: boolean
}

export interface CreateScheduleRequest {
  id: string
  label: string
  cron: string
  payload: Record<string, unknown>
}

export interface SensorReading {
  device_id: string
  sensor_type: string
  value: number
  unit: string
  recorded_at: string
}

export interface CameraEvent {
  id: number
  camera_id: string
  event_type: string
  confidence: number | null
  snapshot_path: string | null
  acknowledged: boolean
  created_at: string
}

export interface ModelStatusEntry {
  category:         string
  name:             string
  description:      string
  size_mb:          number
  downloaded:       boolean
  active:           boolean
  url:              string | null
  hf_id:            string | null
  filename:         string | null
  ram_estimate_mb:  number | null
  recommended_role: string | null
}

export interface MemoryStatus {
  total_mb:             number
  available_for_llm_mb: number
  loaded_model:         string | null
}

export interface ModelsResponse {
  whisper:   ModelStatusEntry[]
  llamafile: ModelStatusEntry[]
  tts:       ModelStatusEntry[]
  gguf:      ModelStatusEntry[]
}

export interface OllamaModel {
  name:        string
  model:       string
  size:        number
  modified_at: string
}

export interface HuggingFaceModel {
  id:        string
  downloads: number
  likes:     number
  tags:      string[]
  url:       string
}

export interface HuggingFaceFile {
  filename: string
  size_mb:  number | null
  url:      string
}

export interface DownloadEntry {
  filename:          string
  category:          string
  downloaded_bytes:  number
  total_bytes:       number | null
  status:            'downloading' | 'done' | 'error'
}

export interface LlamafileAsset {
  name:        string
  version:     string
  size_mb:     number
  url:         string
  release_url: string
}

export interface Settings {
  primary_profile_id: string | null
  assistant_name: string
  assistant_personality: string
  user_name: string
  timezone: string
  llm_max_tokens: number
  llm_temperature: number
  llm_provider: string
  voice_wake_word: string
  voice_tts_voice: string
  voice_recording_duration_secs: number
  voice_whisper_url: string
  active_llm_model: string
  active_whisper_model: string
  active_tts_model: string
  weather_enabled: boolean
  weather_latitude: number
  weather_longitude: number
  weather_location_name: string
  retention_event_log_days: number
  retention_sensor_days: number
  retention_session_messages_keep: number
  prompt_style: string
  custom_system_prompt: string | null
  prompt_addendum: string
  // Model role assignments
  chat_provider:   string
  chat_model:      string
  think_provider:  string | null
  think_model:     string | null
  task_provider:   string | null
  task_model:      string | null
  tool_model:      string | null
}

// ── Agent data types ──────────────────────────────────────────────────────────

export interface PromptTemplate {
  name: string
  content: string
  description: string
  is_system: boolean
  updated_at: string
}

export interface PromptExtra {
  key: string
  instruction: string
  active: boolean
  sort_order: number
}

export interface UserSkill {
  id: string
  name: string
  content: string
  active: boolean
  created_at: string
}

export interface AgentRecipe {
  id: string
  name: string
  description: string
  yaml: string
  active: boolean
  created_at: string
}

export interface MemoryFragment {
  id: string
  profile_id: string | null
  session_id: string | null
  content: string
  source: string
  tags: string[]
  created_at: string
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function handleUnauthorized() {
  window.dispatchEvent(new CustomEvent('pond-unauthorized'))
}

async function post<T>(path: string, body: unknown, token?: string): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (token) headers['Authorization'] = `Bearer ${token}`

  const res = await fetch(`${BASE}${path}`, {
    method: 'POST',
    headers,
    body: JSON.stringify(body),
  })

  if (res.status === 401) { handleUnauthorized(); throw new Error('Unauthorized') }
  if (!res.ok) {
    const text = await res.text()
    throw new Error(text || `HTTP ${res.status}`)
  }

  return res.json()
}

async function putReq<T>(path: string, body: unknown, token: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method: 'PUT',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${token}`,
    },
    body: JSON.stringify(body),
  })

  if (res.status === 401) { handleUnauthorized(); throw new Error('Unauthorized') }
  if (!res.ok) {
    const text = await res.text()
    throw new Error(text || `HTTP ${res.status}`)
  }

  return res.json()
}

async function getReq<T>(path: string, token: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    headers: token ? { 'Authorization': `Bearer ${token}` } : {},
  })

  if (!res.ok) {
    const text = await res.text()
    throw new Error(text || `HTTP ${res.status}`)
  }

  return res.json()
}

async function deleteReq<T>(path: string, token: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method: 'DELETE',
    headers: { 'Authorization': `Bearer ${token}` },
  })

  if (res.status === 401) { handleUnauthorized(); throw new Error('Unauthorized') }
  if (!res.ok) {
    const text = await res.text()
    throw new Error(text || `HTTP ${res.status}`)
  }

  return res.json()
}

async function deleteVoidReq(path: string, token: string): Promise<void> {
  const res = await fetch(`${BASE}${path}`, {
    method: 'DELETE',
    headers: { 'Authorization': `Bearer ${token}` },
  })

  if (!res.ok) {
    const text = await res.text()
    throw new Error(text || `HTTP ${res.status}`)
  }
}

// ── API calls ─────────────────────────────────────────────────────────────────

export const api = {
  // ── Auth & onboarding ────────────────────────────────────────────────────

  /** Step 1: verify this device and get a session token */
  handshake: (req: HandshakeRequest) =>
    post<HandshakeResponse>('/handshake', req),

  /** Step 2: start onboarding tracking on the backend */
  startOnboarding: () =>
    post<{ status: string }>('/onboard', {}),

  /** Check whether this device has completed onboarding */
  onboardingStatus: () =>
    getReq<OnboardingStatus>('/onboard/status', ''),

  /** Mark onboarding as complete on the backend, unlocking protected routes */
  completeOnboarding: () =>
    post<{ status: string }>('/onboard/complete', {}),

  // ── Settings ─────────────────────────────────────────────────────────────

  /** Load all settings from the backend */
  getSettings: (token: string) =>
    getReq<Settings>('/settings', token),

  /** Save settings (partial update — only provided keys are merged) */
  saveSettings: (settings: Record<string, unknown>, token: string) =>
    putReq<{ status: string }>('/settings', settings, token),

  // ── System ───────────────────────────────────────────────────────────────

  /** System info — hostname, version, platform */
  systemInfo: (token: string) =>
    getReq<{ hostname: string; version: string; platform: string; arch: string }>('/system/info', token),

  /** Health check */
  health: () =>
    getReq<{ status: string; version: string }>('/health', ''),

  // ── Chat & sessions ──────────────────────────────────────────────────────

  /** Send a chat message */
  chat: (message: string, token: string, sessionId?: string) =>
    post<{ session_id: string; response: string; model_role?: string }>('/chat', { message, session_id: sessionId }, token),

  /** List all sessions */
  listSessions: (token: string) =>
    getReq<{ sessions: SessionSummary[] }>('/sessions', token),

  /** Load all messages for a session */
  getSessionMessages: (sessionId: string, token: string) =>
    getReq<{ messages: SessionMessage[] }>(`/sessions/${sessionId}/messages`, token),

  /** Rename a session */
  renameSession: (sessionId: string, title: string, token: string) =>
    fetch(`${BASE}/sessions/${sessionId}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${token}` },
      body: JSON.stringify({ title }),
    }).then(r => r.json()),

  // ── Profiles ─────────────────────────────────────────────────────────────

  /** List all household profiles */
  listProfiles: (token: string) =>
    getReq<{ profiles: { id: string; display_name: string; avatar_emoji: string; preferences: Record<string, string> }[] }>('/profiles', token),

  /** Create a new household profile (public — callable during onboarding) */
  createProfile: (req: { display_name: string; avatar_emoji: string }, token: string) =>
    post<{ id: string; display_name: string; avatar_emoji: string; preferences: Record<string, string> }>('/profiles', req, token),

  /** Update a profile's preferences (public — callable during onboarding) */
  updateProfilePreferences: (id: string, preferences: Record<string, string>, token: string) =>
    fetch(`${BASE}/profiles/${id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${token}` },
      body: JSON.stringify({ preferences }),
    }).then(async r => {
      if (!r.ok) { const t = await r.text(); throw new Error(t || `HTTP ${r.status}`) }
      return r.json()
    }),

  // ── Devices ──────────────────────────────────────────────────────────────

  /** List already-registered devices */
  listDevices: (token: string) =>
    getReq<{ devices: { id: string; name: string; device_type: string }[] }>('/devices', token),

  /** Register a new device */
  registerDevice: (device: { name: string; device_type: string; hostname?: string; capabilities: string[] }, token: string) =>
    post<{ status: string }>('/devices', device, token),

  // ── Sensors ──────────────────────────────────────────────────────────────

  /** Get recent sensor readings for a device */
  getSensors: (deviceId: string, token: string, limit = 5) =>
    getReq<{ readings: SensorReading[] }>(`/sensors/${deviceId}?limit=${limit}`, token),

  // ── Camera ───────────────────────────────────────────────────────────────

  /** List recent camera events */
  listCameraEvents: (token: string, limit = 20) =>
    getReq<{ events: CameraEvent[] }>(`/camera/events?limit=${limit}`, token),

  // ── Scheduler ────────────────────────────────────────────────────────────

  /** List all scheduled tasks */
  listSchedules: (token: string) =>
    getReq<ScheduledTask[]>('/schedules', token),

  /** Create a new scheduled task */
  createSchedule: (req: CreateScheduleRequest, token: string) =>
    post<ScheduledTask>('/schedules', req, token),

  /** Delete a scheduled task */
  deleteSchedule: (id: string, token: string) =>
    deleteReq<{ status: string }>(`/schedules/${id}`, token),

  /** Pause a scheduled task */
  pauseSchedule: (id: string, token: string) =>
    post<{ status: string }>(`/schedules/${id}/pause`, {}, token),

  /** Resume a paused task */
  resumeSchedule: (id: string, token: string) =>
    post<{ status: string }>(`/schedules/${id}/resume`, {}, token),

  /** Trigger a task to run immediately */
  runScheduleNow: (id: string, token: string) =>
    post<{ status: string }>(`/schedules/${id}/run-now`, {}, token),

  // ── Models ───────────────────────────────────────────────────────────────

  /** List all models with download/active status */
  listModels: (token: string) =>
    getReq<ModelsResponse>('/models', token),

  /** Trigger an async download for a model */
  downloadModel: (category: string, name: string, token: string) =>
    post<{ status: string; name: string; category: string }>(`/models/${category}/${name}/download`, {}, token),

  /** Refresh the model registry from the online URL */
  refreshModelRegistry: (token: string) =>
    post<{ status: string }>('/models/registry/refresh', {}, token),

  /** Scan model directories for files not yet in the registry */
  scanModels: (token: string) =>
    post<{ found: number; entries: ModelStatusEntry[] }>('/models/scan', {}, token),

  /** Get the currently wired provider+model for each role */
  getActiveRoles: (token: string) =>
    getReq<{
      chat:  { provider: string; model: string }
      think: { provider: string | null; model: string | null }
      task:  { provider: string | null; model: string | null }
      asr:   { model_id: string | null }
      tts:   { model_id: string | null }
      router_name: string
    }>('/models/active-roles', token),

  /** List models available in a running Ollama instance */
  listOllamaModels: (token: string) =>
    getReq<{ models: OllamaModel[]; error?: string }>('/models/ollama', token),

  /** Pull an Ollama model by name (non-blocking on the server) */
  pullOllamaModel: (model: string, token: string) =>
    post<{ status: string; model: string }>('/models/ollama/pull', { model }, token),

  /** Search HuggingFace for GGUF models */
  searchGgufModels: (q: string, token: string) =>
    getReq<{ models: HuggingFaceModel[]; error?: string }>(`/models/search/gguf?q=${encodeURIComponent(q)}`, token),

  /** List .gguf files inside a specific HuggingFace repo */
  listHfModelFiles: (repo: string, token: string) =>
    getReq<{ files: HuggingFaceFile[]; error?: string }>(`/models/search/gguf/files?repo=${encodeURIComponent(repo)}`, token),

  /** Download a model file by URL into the server's models folder */
  downloadModelFromUrl: (url: string, category: string, filename: string, token: string) =>
    post<{ status: string; filename: string; category: string }>('/models/download/url', { url, category, filename }, token),

  /** Get progress for all active/recent downloads */
  getDownloadProgress: (token: string) =>
    getReq<{ downloads: DownloadEntry[] }>('/models/download/progress', token),

  /** List llamafile releases from GitHub */
  searchLlamafileModels: (q: string, token: string) =>
    getReq<{ models: LlamafileAsset[]; error?: string }>(`/models/search/llamafile?q=${encodeURIComponent(q)}`, token),

  /** Get runtime capabilities of the active model */
  getModelCapabilities: (token: string) =>
    getReq<{ thinking: boolean; vision: boolean; audio_input: boolean; context_window_tokens: number; structured_output: boolean }>('/models/capabilities', token),

  /** Get current RAM usage and loaded model info */
  getMemoryStatus: (token: string) =>
    getReq<MemoryStatus>('/models/memory-status', token),

  /** Delete a model file from disk (catalog record kept). Returns 204 No Content. */
  deleteModel: async (category: string, name: string, token: string): Promise<void> => {
    const res = await fetch(`${BASE}/models/${category}/${name}`, {
      method: 'DELETE',
      headers: { 'Authorization': `Bearer ${token}` },
    })
    if (!res.ok) {
      const text = await res.text()
      throw new Error(text || `HTTP ${res.status}`)
    }
  },

  /** Assign a model to a role, rebuilding the ModelRouter live. */
  activateModel: (category: string, name: string, role: string, token: string) =>
    post<{ role: string; model_id: string }>(`/models/${category}/${name}/activate`, { role }, token),

  // ── Streaming chat ────────────────────────────────────────────────────────

  /**
   * Stream chat tokens via Server-Sent Events.
   *
   * Calls `onToken` for each incremental token, `onDone` when complete,
   * and `onError` on failure. Uses `fetch` + `ReadableStream` because SSE
   * requires a POST body which `EventSource` does not support.
   */
  chatStream: async (
    message: string,
    token: string,
    sessionId: string | undefined,
    onToken: (token: string) => void,
    onDone: (sessionId: string, modelRole?: string) => void,
    onError: (err: string) => void,
  ): Promise<void> => {
    let res: Response
    try {
      res = await fetch(`${BASE}/chat/stream`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${token}`,
        },
        body: JSON.stringify({ message, session_id: sessionId }),
      })
    } catch (e) {
      onError(String(e))
      return
    }

    if (!res.ok) {
      onError(`HTTP ${res.status}`)
      return
    }

    const reader = res.body?.getReader()
    if (!reader) { onError('No response body'); return }

    const decoder = new TextDecoder()
    let buffer = ''

    try {
      while (true) {
        const { done, value } = await reader.read()
        if (done) break
        buffer += decoder.decode(value, { stream: true })

        // Process all complete SSE lines in the buffer
        let newlinePos: number
        while ((newlinePos = buffer.indexOf('\n')) !== -1) {
          const line = buffer.slice(0, newlinePos).trimEnd()
          buffer = buffer.slice(newlinePos + 1)

          if (!line.startsWith('data: ')) continue
          const raw = line.slice('data: '.length).trim()
          if (!raw) continue

          try {
            const payload = JSON.parse(raw) as {
              type?: string
              token?: string
              content?: string
              done?: boolean
              session_id?: string
              model_role?: string
              error?: string
            }
            if (payload.error) {
              onError(payload.error)
              return
            }
            if (payload.done) {
              onDone(payload.session_id ?? '', payload.model_role)
              return
            }
            const text = (payload.type === 'text' ? payload.content : undefined) ?? payload.token
            if (text) {
              onToken(text)
            }
          } catch {
            // Ignore malformed SSE lines
          }
        }
      }
    } finally {
      reader.releaseLock()
    }
  },

  // ── Prompt Templates ─────────────────────────────────────────────────────

  listPromptTemplates: (token: string) =>
    getReq<PromptTemplate[]>('/prompts', token),

  getPromptTemplate: (name: string, token: string) =>
    getReq<PromptTemplate>(`/prompts/${encodeURIComponent(name)}`, token),

  updatePromptTemplate: (name: string, body: { content: string; description?: string }, token: string) =>
    putReq<{ name: string; status: string }>(`/prompts/${encodeURIComponent(name)}`, body, token),

  deletePromptTemplate: (name: string, token: string) =>
    deleteVoidReq(`/prompts/${encodeURIComponent(name)}`, token),

  // ── Prompt Extras ─────────────────────────────────────────────────────────

  listPromptExtras: (token: string) =>
    getReq<PromptExtra[]>('/agent/extras', token),

  upsertPromptExtra: (body: { key: string; instruction: string; active?: boolean; sort_order?: number }, token: string) =>
    post<{ key: string; status: string }>('/agent/extras', body, token),

  deletePromptExtra: (key: string, token: string) =>
    deleteVoidReq(`/agent/extras/${encodeURIComponent(key)}`, token),

  // ── Agent Tools ───────────────────────────────────────────────────────────

  listAgentTools: (token: string) =>
    getReq<{ extensions: { name: string; tools: string[] }[] }>('/agent/tools', token),

  // ── Skills ────────────────────────────────────────────────────────────────

  listSkills: (token: string) =>
    getReq<UserSkill[]>('/skills', token),

  createSkill: (body: { name: string; content: string }, token: string) =>
    post<UserSkill>('/skills', body, token),

  updateSkill: (id: string, body: { content?: string; active?: boolean }, token: string) =>
    putReq<UserSkill>(`/skills/${encodeURIComponent(id)}`, body, token),

  deleteSkill: (id: string, token: string) =>
    deleteVoidReq(`/skills/${encodeURIComponent(id)}`, token),

  // ── Recipes ───────────────────────────────────────────────────────────────

  listRecipes: (token: string) =>
    getReq<AgentRecipe[]>('/recipes', token),

  createRecipe: (body: { name: string; description?: string; yaml: string }, token: string) =>
    post<AgentRecipe>('/recipes', body, token),

  updateRecipe: (id: string, body: { description?: string; yaml?: string; active?: boolean }, token: string) =>
    putReq<AgentRecipe>(`/recipes/${encodeURIComponent(id)}`, body, token),

  deleteRecipe: (id: string, token: string) =>
    deleteVoidReq(`/recipes/${encodeURIComponent(id)}`, token),

  // Run a named recipe via the agent (uses POST /agent/chat with recipe YAML context)
  runRecipe: (name: string, token: string) =>
    post<{ session_id: string; response: string }>('/chat', { message: `Run recipe: ${name}` }, token),

  // ── Memories ──────────────────────────────────────────────────────────────

  listMemories: (token: string) =>
    getReq<MemoryFragment[]>('/memories', token),

  createMemory: async (body: { content: string; tags?: string[]; source?: string }, token: string): Promise<void> => {
    const res = await fetch(`${BASE}/memories`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${token}` },
      body: JSON.stringify(body),
    })
    if (!res.ok) {
      const text = await res.text()
      throw new Error(text || `HTTP ${res.status}`)
    }
  },

  deleteMemory: (id: string, token: string) =>
    deleteVoidReq(`/memories/${encodeURIComponent(id)}`, token),

  // ── Server-side TTS ───────────────────────────────────────────────────────

  /**
   * Synthesise speech server-side and return a blob URL for playback.
   *
    * The server routes to Piper based on its configuration.
   * Returns `null` if no TTS backend is running (caller should skip audio).
   * Caller is responsible for calling `URL.revokeObjectURL()` after playback.
   */
  speak: async (text: string, token: string): Promise<string | null> => {
    try {
      const res = await fetch(`${BASE}/tts`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${token}`,
        },
        body: JSON.stringify({ text }),
      })
      if (!res.ok) return null
      const blob = await res.blob()
      return URL.createObjectURL(blob)
    } catch {
      return null
    }
  },

  // ── Face Biometrics (Phase 2) ────────────────────────────────────────────
  //
  // Backed by `pond-server` compiled with `--features face-onnx`.  When the
  // feature is disabled these endpoints return 503 — the UI should surface a
  // "face recognition unavailable" state instead of erroring out.

  /** Register a face enrollment for the given profile. */
  registerFace: async (
    profileId: string,
    imageBlob: Blob,
    token: string,
    bbox?: { x: number; y: number; width: number; height: number },
  ): Promise<{ id: string; profile_id: string; model_dims: number; created_at: string }> => {
    const form = new FormData()
    form.append('profile_id', profileId)
    form.append('image', imageBlob, 'face.jpg')
    if (bbox) form.append('bbox', `${bbox.x},${bbox.y},${bbox.width},${bbox.height}`)
    const res = await fetch(`${BASE}/faces/register`, {
      method: 'POST',
      headers: { 'Authorization': `Bearer ${token}` },
      body: form,
    })
    if (!res.ok) {
      const text = await res.text()
      throw new Error(text || `HTTP ${res.status}`)
    }
    return res.json()
  },

  /** Legacy single-frame identify — retained for API callers but
   *  vulnerable to photo attacks (no liveness signal across frames).
   *  Prefer `identifyFaceBurst` for anything user-facing. */
  identifyFace: async (
    imageBlob: Blob,
    token: string,
    bbox?: { x: number; y: number; width: number; height: number },
  ): Promise<{ identified: boolean; profile_id: string | null; confidence: number | null; threshold: number }> => {
    const form = new FormData()
    form.append('image', imageBlob, 'face.jpg')
    if (bbox) form.append('bbox', `${bbox.x},${bbox.y},${bbox.width},${bbox.height}`)
    const res = await fetch(`${BASE}/faces/identify`, {
      method: 'POST',
      headers: { 'Authorization': `Bearer ${token}` },
      body: form,
    })
    if (!res.ok) {
      const text = await res.text()
      throw new Error(text || `HTTP ${res.status}`)
    }
    return res.json()
  },

  /** Production-style identify: 5-frame burst with multi-frame liveness
   *  gates.  Inter-frame embedding sameness + landmark pixel-motion
   *  std-dev catch photo/phone-screen attacks that single-frame can't see.
   *  Response adds `reason: "liveness_failed"` when a still-image
   *  presentation attack is detected. */
  identifyFaceBurst: async (
    frames: Blob[],
    token: string,
    bbox?: { x: number; y: number; width: number; height: number },
  ): Promise<{
    identified: boolean
    profile_id: string | null
    confidence: number | null
    threshold: number
    reason?: string
    liveness?: {
      hard_reject: boolean
      suspicious: boolean
      mean_inter_cos: number
      landmark_motion: number
      eye_ratio_spread: number
    }
  }> => {
    const form = new FormData()
    frames.forEach((f, i) => form.append('image', f, `frame${i}.jpg`))
    if (bbox) form.append('bbox', `${bbox.x},${bbox.y},${bbox.width},${bbox.height}`)
    const res = await fetch(`${BASE}/faces/identify-burst`, {
      method: 'POST',
      headers: { 'Authorization': `Bearer ${token}` },
      body: form,
    })
    if (!res.ok) {
      const text = await res.text()
      throw new Error(text || `HTTP ${res.status}`)
    }
    return res.json()
  },

  /** Status of the three on-disk face models — used by Models pages to render
   *  a parity card for ArcFace + SCRFD + Silent-Face PAD. Returns
   *  `feature_enabled: false` when pond-server was built without
   *  `--features face-onnx`. */
  listFaceModels: (token: string) =>
    getReq<{
      feature_enabled: boolean
      models_dir: string | null
      models: Array<{
        name: string
        label: string
        role: 'embedding' | 'detector' | 'antispoof'
        expected_mb: number
        size_mb: number | null
        downloaded: boolean
        path: string | null
      }>
    }>(`/faces/models`, token),

  /** List all face enrollments for a profile (metadata only — embeddings stay server-side). */
  listFaceEnrollments: (profileId: string, token: string) =>
    getReq<{ profile_id: string; enrollments: { id: string; profile_id: string; model_dims: number; created_at: string }[]; count: number }>(
      `/faces/profile/${profileId}`,
      token,
    ),

  /** Delete every biometric record for a profile (face embeddings today; voice prints once Phase 1 lands). */
  deleteUserBiometrics: async (
    profileId: string,
    token: string,
  ): Promise<{ profile_id: string; face_embeddings_deleted: number }> => {
    const res = await fetch(`${BASE}/users/${profileId}/biometrics`, {
      method: 'DELETE',
      headers: { 'Authorization': `Bearer ${token}` },
    })
    if (!res.ok) {
      const text = await res.text()
      throw new Error(text || `HTTP ${res.status}`)
    }
    return res.json()
  },
}
