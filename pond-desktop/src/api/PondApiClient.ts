import {
  ApiError,
  type AddExtensionRequest,
  type AgentRecipe,
  type AgentTool,
  type CalibrateResponse,
  type ChatEvent,
  type Device,
  type DownloadEntry,
  type Extension,
  type FaceModelsResponse,
  type HandshakeResponse,
  type HealthResponse,
  type HfModel,
  type HfModelFile,
  type LlamafileRelease,
  type LogEntry,
  type MemoryFragment,
  type ModelActiveRoles,
  type ModelEntry,
  type ModelMemoryStatus,
  type OllamaModel,
  type PromptExtra,
  type PromptTemplate,
  type Schedule,
  type SessionMessage,
  type SessionSummary,
  type Settings,
  type TranscribeResponse,
  type UserSkill,
} from "./types";

// ────────────────────────────────────────────────────────────
// PondApiClient — single API port for the pond-desktop app.
// All REST calls MUST go through this class. No fetch() elsewhere.
// ────────────────────────────────────────────────────────────

declare global {
  interface Window {
    __GIAP_SERVER_URL__?: string;
  }
}

export class PondApiClient {
  private readonly base: string;
  private token: string | null;

  constructor(base?: string, token?: string | null) {
    this.base = (base ?? window.__GIAP_SERVER_URL__ ?? "http://127.0.0.1:4000").replace(/\/$/, "");
    this.token = token ?? null;
  }

  setToken(token: string | null): void {
    this.token = token;
  }

  // ── Internal helpers ───────────────────────────────────────

  private headers(extra?: Record<string, string>): Record<string, string> {
    const h: Record<string, string> = { "Content-Type": "application/json", ...extra };
    if (this.token) h["Authorization"] = `Bearer ${this.token}`;
    return h;
  }

  private async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await fetch(`${this.base}${path}`, {
      method,
      headers: this.headers(),
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).message ?? msg; } catch { /* ignore */ }
      throw new ApiError(res.status, msg);
    }
    // 204 No Content and any other empty body — return undefined cast to T
    const ct = res.headers.get("content-type") ?? "";
    if (res.status === 204 || !ct.includes("json")) return undefined as unknown as T;
    return res.json() as Promise<T>;
  }

  private get<T>(path: string): Promise<T>                       { return this.request<T>("GET", path); }
  private post<T>(path: string, body?: unknown): Promise<T>       { return this.request<T>("POST", path, body); }
  private put<T>(path: string, body?: unknown): Promise<T>        { return this.request<T>("PUT", path, body); }
  private patch<T = void>(path: string, body?: unknown): Promise<T> { return this.request<T>("PATCH", path, body); }
  private del<T = void>(path: string): Promise<T>                 { return this.request<T>("DELETE", path); }

  // ── Health ────────────────────────────────────────────────

  health(): Promise<HealthResponse> {
    return this.get("/api/v1/health");
  }

  // ── Onboarding ────────────────────────────────────────────

  getOnboardingStatus(): Promise<{ onboarded: boolean; current_step: string; steps_completed: number; total_steps: number }> {
    return this.get("/api/v1/onboard/status");
  }

  completeOnboarding(): Promise<{ status: string }> {
    return this.post("/api/v1/onboard/complete");
  }

  // ── Settings ──────────────────────────────────────────────

  getSettings(): Promise<Settings> {
    return this.get("/api/v1/settings");
  }

  updateSettings(patch: Partial<Settings>): Promise<Settings> {
    return this.put("/api/v1/settings", patch);
  }

  // ── Devices ───────────────────────────────────────────────

  listDevices(): Promise<Device[]> {
    return this.get<{ devices: Device[] } | Device[]>("/api/v1/devices").then((r) =>
      Array.isArray(r) ? r : (r as { devices: Device[] }).devices ?? [],
    );
  }

  // ── Schedules ─────────────────────────────────────────────
  // Backend field mapping: label ↔ name, payload.prompt ↔ prompt, paused ↔ !enabled

  listSchedules(): Promise<Schedule[]> {
    return this.get<Array<Record<string, unknown>>>("/api/v1/schedules").then((items) =>
      (Array.isArray(items) ? items : []).map((t) => ({
        id: t.id as string,
        name: (t.label ?? t.name ?? "") as string,
        cron: t.cron as string,
        prompt: ((t.payload as Record<string, unknown> | undefined)?.prompt as string | undefined) ?? "",
        enabled: t.paused !== undefined ? !(t.paused as boolean) : (t.enabled as boolean ?? true),
        created_at: t.created_at as string | undefined,
      })),
    );
  }

  createSchedule(body: Omit<Schedule, "id" | "created_at">): Promise<Schedule> {
    const id = crypto.randomUUID();
    return this.post<Record<string, unknown>>("/api/v1/schedules", {
      id,
      label: body.name,
      cron: body.cron,
      payload: { prompt: body.prompt },
    }).then((t) => ({
      id: t.id as string,
      name: (t.label ?? t.name ?? body.name) as string,
      cron: t.cron as string,
      prompt: ((t.payload as Record<string, unknown> | undefined)?.prompt as string | undefined) ?? body.prompt,
      enabled: t.paused !== undefined ? !(t.paused as boolean) : true,
    }));
  }

  deleteSchedule(id: string): Promise<void> {
    return this.del(`/api/v1/schedules/${id}`);
  }

  // ── Memory ────────────────────────────────────────────────

  listMemories(limit = 20): Promise<MemoryFragment[]> {
    return this.get(`/api/v1/memories?limit=${limit}`);
  }

  addMemory(content: string, tags?: string[]): Promise<MemoryFragment> {
    return this.post("/api/v1/memories", { content, tags });
  }

  deleteMemory(id: string): Promise<void> {
    return this.del(`/api/v1/memories/${id}`);
  }

  // ── Skills ────────────────────────────────────────────────

  listSkills(all = false): Promise<UserSkill[]> {
    return this.get(`/api/v1/skills${all ? "?all=true" : ""}`);
  }

  addSkill(name: string, content: string): Promise<UserSkill> {
    return this.post("/api/v1/skills", { name, content });
  }

  toggleSkill(id: string, currentEnabled: boolean): Promise<UserSkill> {
    return this.put(`/api/v1/skills/${id}`, { active: !currentEnabled });
  }

  removeSkill(id: string): Promise<void> {
    return this.del(`/api/v1/skills/${id}`);
  }

  // ── Auth / Handshake ─────────────────────────────────────────

  handshake(clientId: string): Promise<HandshakeResponse> {
    return this.post("/api/v1/handshake", { client_id: clientId });
  }

  // ── Models ────────────────────────────────────────────────

  listModels(): Promise<ModelEntry[]> {
    // Backend returns { gguf: [...], llamafile: [...], tts: [...], whisper: [...] }
    // each entry has: name, category, active, ram_estimate_mb, recommended_role, description
    return this.get<ModelEntry[] | Record<string, unknown[]>>("/api/v1/models").then((r) => {
      if (Array.isArray(r)) return r;
      // Flatten grouped object into ModelEntry[]
      const entries: ModelEntry[] = [];
      for (const [category, items] of Object.entries(r)) {
        for (const item of items as Record<string, unknown>[]) {
          entries.push({
            id: `${category}/${item.name as string}`,
            provider: category,
            name: item.name as string,
            display_name: (item.description as string | undefined) ?? (item.name as string),
            is_active: (item.active as boolean | undefined) ?? false,
            ram_estimate_mb: item.ram_estimate_mb as number | undefined,
            recommended_role: item.recommended_role as string | undefined,
          });
        }
      }
      return entries;
    });
  }

  getModelCapabilities(): Promise<import("./types").ModelCapabilities> {
    return this.get("/api/v1/models/capabilities");
  }

  getMemoryStatus(): Promise<ModelMemoryStatus> {
    return this.get("/api/v1/models/memory-status");
  }

  getActiveRoles(): Promise<ModelActiveRoles> {
    // Backend may return { model_id: "provider/name" } for ASR/TTS instead of { provider, model }.
    // Normalize all roles to { provider, model } | null.
    return this.get<Record<string, unknown>>("/api/v1/models/active-roles").then((raw) => {
      function normalize(r: unknown): { provider: string; model: string } | null {
        if (!r || typeof r !== "object") return null;
        const obj = r as Record<string, unknown>;
        // Already has provider + model
        if (obj.provider && obj.model) return { provider: obj.provider as string, model: obj.model as string };
        // Has model_id like "whisper/base.en" or "gguf/gemma-2b"
        if (typeof obj.model_id === "string" && obj.model_id.includes("/")) {
          const [provider, ...rest] = obj.model_id.split("/");
          return { provider, model: rest.join("/") };
        }
        return null;
      }
      return {
        chat:  normalize(raw.chat),
        tool:  raw.tool ?? null,
        asr:   normalize(raw.asr),
        tts:   normalize(raw.tts),
      } as ModelActiveRoles;
    });
  }

  activateModel(provider: string, name: string, role: string): Promise<void> {
    return this.post(`/api/v1/models/${encodeURIComponent(provider)}/${encodeURIComponent(name)}/activate`, { role });
  }

  // ── Sessions ──────────────────────────────────────────────

  listSessions(): Promise<SessionSummary[]> {
    return this.get<{ sessions: SessionSummary[] } | SessionSummary[]>("/api/v1/sessions").then((r) =>
      Array.isArray(r) ? r : (r as { sessions: SessionSummary[] }).sessions ?? [],
    );
  }

  getSessionMessages(sessionId: string): Promise<SessionMessage[]> {
    return this.get<{ messages: SessionMessage[] } | SessionMessage[]>(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/messages`,
    ).then((r) =>
      Array.isArray(r) ? r : (r as { messages: SessionMessage[] }).messages ?? [],
    );
  }

  // ── Prompts ───────────────────────────────────────────────

  listPrompts(): Promise<PromptTemplate[]> {
    return this.get("/api/v1/prompts");
  }

  getPrompt(name: string): Promise<PromptTemplate> {
    return this.get(`/api/v1/prompts/${name}`);
  }

  updatePrompt(name: string, content: string): Promise<PromptTemplate> {
    return this.put(`/api/v1/prompts/${name}`, { content });
  }

  resetPrompt(name: string): Promise<PromptTemplate> {
    return this.post(`/api/v1/prompts/${name}/reset`);
  }

  // ── Agent extras ──────────────────────────────────────────

  listExtras(): Promise<PromptExtra[]> {
    // Backend uses field name "instruction"; normalize to "content" and "enabled"
    return this.get<Array<Record<string, unknown>>>("/api/v1/agent/extras").then((items) =>
      items.map((e) => ({
        key: e.key as string,
        content: (e.content ?? e.instruction ?? "") as string,
        enabled: (e.enabled ?? e.active ?? true) as boolean,
      })),
    );
  }

  addExtra(key: string, content: string): Promise<PromptExtra> {
    // Backend expects field name "instruction" not "content"
    return this.post<Record<string, unknown>>("/api/v1/agent/extras", { key, instruction: content, active: true })
      .then(() => ({ key, content, enabled: true }));
  }

  deleteExtra(key: string): Promise<void> {
    return this.del(`/api/v1/agent/extras/${encodeURIComponent(key)}`);
  }

  listTools(): Promise<AgentTool[]> {
    return this.get("/api/v1/agent/tools");
  }

  // ── Recipes ───────────────────────────────────────────────

  listRecipes(): Promise<AgentRecipe[]> {
    return this.get("/api/v1/recipes");
  }

  getRecipe(name: string): Promise<AgentRecipe> {
    return this.get(`/api/v1/recipes/${name}`);
  }

  // ── Chat streaming ────────────────────────────────────────
  //
  // Returns an AsyncGenerator that yields ChatEvent objects.
  // Usage:
  //   for await (const event of client.chatStream("hello")) {
  //     if (event.type === "text") appendToken(event.content ?? "");
  //   }

  async *chatStream(
    message: string,
    sessionId?: string,
    token?: string,
  ): AsyncGenerator<ChatEvent> {
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    const tok = token ?? this.token;
    if (tok) headers["Authorization"] = `Bearer ${tok}`;

    const res = await fetch(`${this.base}/api/v1/chat/stream`, {
      method: "POST",
      headers,
      body: JSON.stringify({ message, session_id: sessionId }),
    });

    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).message ?? msg; } catch { /* ignore */ }
      throw new ApiError(res.status, msg);
    }

    const reader = res.body!.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() ?? "";

        for (const line of lines) {
          const trimmed = line.trim();
          if (!trimmed || trimmed === "data: [DONE]") {
            if (trimmed === "data: [DONE]") yield { type: "done", done: true };
            continue;
          }
          const data = trimmed.startsWith("data: ") ? trimmed.slice(6) : trimmed;
          try {
            const event = JSON.parse(data) as ChatEvent;
            yield event;
          } catch {
            // Malformed SSE line — skip
          }
        }
      }
    } finally {
      reader.releaseLock();
    }
  }

  // ── Schedule actions ──────────────────────────────────────────

  pauseSchedule(id: string): Promise<void> {
    return this.post(`/api/v1/schedules/${encodeURIComponent(id)}/pause`);
  }

  resumeSchedule(id: string): Promise<void> {
    return this.post(`/api/v1/schedules/${encodeURIComponent(id)}/resume`);
  }

  runScheduleNow(id: string): Promise<void> {
    return this.post(`/api/v1/schedules/${encodeURIComponent(id)}/run-now`);
  }

  // ── GGUF / HuggingFace model search & download ────────────────

  searchGgufModels(q: string): Promise<{ models: HfModel[] }> {
    return this.get(`/api/v1/models/search/gguf?q=${encodeURIComponent(q)}`);
  }

  listHfModelFiles(repo: string): Promise<{ files: HfModelFile[] }> {
    return this.get(`/api/v1/models/search/gguf/files?repo=${encodeURIComponent(repo)}`);
  }

  downloadModelFromUrl(url: string, category: string, filename: string): Promise<{ status: string }> {
    return this.post("/api/v1/models/download/url", { url, category, filename });
  }

  getDownloadProgress(): Promise<{ downloads: DownloadEntry[] }> {
    return this.get("/api/v1/models/download/progress");
  }

  scanModels(): Promise<{ found: number }> {
    return this.post("/api/v1/models/scan");
  }

  // Delete model file from disk (409 ApiError if model is active in a role)
  deleteModel(category: string, name: string): Promise<void> {
    return this.del(`/api/v1/models/${encodeURIComponent(category)}/${encodeURIComponent(name)}`);
  }

  // ── Ollama ────────────────────────────────────────────────

  listOllamaModels(): Promise<{ models: OllamaModel[]; error?: string }> {
    return this.get("/api/v1/models/ollama");
  }

  pullOllamaModel(model: string): Promise<{ status: string }> {
    return this.post("/api/v1/models/ollama/pull", { model });
  }

  // ── Face Recognition (auto-managed; status only) ──────────
  //
  // The desktop Models section binds to this for read-only status of the
  // ArcFace + SCRFD + Silent-Face PAD models that pond-server downloads
  // automatically on first boot when built with `--features face-onnx`.
  listFaceModels(): Promise<FaceModelsResponse> {
    return this.get("/api/v1/faces/models");
  }

  // ── Face enrollment / identification (Faces section) ──────
  //
  // Multipart-form endpoints — bypass the JSON helpers and use fetch
  // directly so the request body stays a FormData. All calls require the
  // `face-onnx` cargo feature; without it pond-server returns 503 which
  // bubbles up as ApiError(503).

  listProfiles(): Promise<{ profiles: Array<{ id: string; display_name: string; avatar_emoji: string }> }> {
    return this.get("/api/v1/profiles");
  }

  async registerFace(profileId: string, frame: Blob): Promise<{
    id: string; profile_id: string; model_dims: number; created_at: string;
  }> {
    const form = new FormData();
    form.append("profile_id", profileId);
    form.append("image", frame, "face.jpg");
    return this.postMultipart("/api/v1/faces/register", form);
  }

  async identifyFaceBurst(frames: Blob[]): Promise<{
    identified: boolean;
    profile_id: string | null;
    confidence: number | null;
    threshold: number;
    reason?: string;
  }> {
    const form = new FormData();
    frames.forEach((f, i) => form.append("image", f, `frame${i}.jpg`));
    return this.postMultipart("/api/v1/faces/identify-burst", form);
  }

  listFaceEnrollments(profileId: string): Promise<{
    profile_id: string;
    enrollments: Array<{ id: string; profile_id: string; model_dims: number; created_at: string }>;
    count: number;
  }> {
    return this.get(`/api/v1/faces/profile/${encodeURIComponent(profileId)}`);
  }

  deleteUserBiometrics(profileId: string): Promise<{ profile_id: string; face_embeddings_deleted: number }> {
    return this.del(`/api/v1/users/${encodeURIComponent(profileId)}/biometrics`);
  }

  // ── Voice / Speaker biometrics ────────────────────────────────────────────────
  //
  // Enrollment triggers server-side mic recording (5 s by default). In the
  // Tauri desktop app the server runs on the same machine, so the server mic
  // IS the user's mic. Returns the new total enrollment count so the UI can
  // display progress without a separate listing endpoint.

  enrollSpeaker(profileId: string, durationSecs = 5): Promise<{
    embedding_id: string;
    profile_id: string;
    model: string;
    dims: number;
    enrolled_count: number;
    created_at: string;
  }> {
    return this.post(`/api/v1/profiles/${encodeURIComponent(profileId)}/enroll`, {
      duration_secs: durationSecs,
    });
  }

  deleteSpeakerBiometrics(profileId: string): Promise<void> {
    return this.del(`/api/v1/profiles/${encodeURIComponent(profileId)}/biometrics`);
  }

  listSpeakerEnrollments(profileId: string): Promise<{
    profile_id: string;
    enrollments: Array<{ id: string; profile_id: string; model: string; dims: number; created_at: string }>;
    count: number;
  }> {
    return this.get(`/api/v1/speaker/enrollments/${encodeURIComponent(profileId)}`);
  }

  identifyVoice(durationSecs = 5): Promise<{
    identified: boolean;
    profile_id: string | null;
    confidence: number | null;
  }> {
    return this.post("/api/v1/speaker/identify", { duration_secs: durationSecs });
  }

  private async postMultipart<T>(path: string, form: FormData): Promise<T> {
    const headers: Record<string, string> = {};
    if (this.token) headers["Authorization"] = `Bearer ${this.token}`;
    const res = await fetch(`${this.base}${path}`, { method: "POST", headers, body: form });
    if (!res.ok) {
      let message = `HTTP ${res.status}`;
      try { message = (await res.text()) || message; } catch { /* ignore */ }
      throw new ApiError(res.status, message);
    }
    return res.json() as Promise<T>;
  }

  // ── Llamafile GitHub releases ─────────────────────────────

  searchLlamafileModels(q?: string): Promise<{ models: LlamafileRelease[] }> {
    const qs = q ? `?q=${encodeURIComponent(q)}` : "";
    return this.get(`/api/v1/models/search/llamafile${qs}`);
  }

  // ── Agent chat (agentic mode with tool calls) ─────────────

  async *agentChatStream(message: string, sessionId?: string): AsyncGenerator<ChatEvent> {
    const body: Record<string, string> = { message };
    if (sessionId) body.session_id = sessionId;

    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (this.token) headers["Authorization"] = `Bearer ${this.token}`;

    const res = await fetch(`${this.base}/api/v1/agent/chat/stream`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
    });

    if (!res.ok || !res.body) {
      let msg = res.statusText;
      try { msg = (await res.json()).message ?? msg; } catch { /* ignore */ }
      throw new ApiError(res.status, msg);
    }

    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() ?? "";
        for (const line of lines) {
          const trimmed = line.trim();
          if (!trimmed || trimmed === "data: [DONE]") continue;
          const data = trimmed.startsWith("data: ") ? trimmed.slice(6) : trimmed;
          try { yield JSON.parse(data) as ChatEvent; } catch { /* malformed */ }
        }
      }
    } finally {
      reader.releaseLock();
    }
  }

  // ── Extensions ───────────────────────────────────────────

  listExtensions(): Promise<{ extensions: Extension[] }> {
    return this.get("/api/v1/extensions");
  }

  toggleExtension(name: string, enabled: boolean): Promise<void> {
    return this.patch(`/api/v1/extensions/${encodeURIComponent(name)}`, { enabled });
  }

  addExtension(req: AddExtensionRequest): Promise<Extension> {
    return this.post("/api/v1/extensions", req);
  }

  removeExtension(name: string): Promise<void> {
    return this.del(`/api/v1/extensions/${encodeURIComponent(name)}`);
  }

  // ── Logs ─────────────────────────────────────────────────

  listLogs(params?: { limit?: number; level?: string }): Promise<LogEntry[]> {
    const qs = new URLSearchParams();
    if (params?.limit !== undefined) qs.set("limit", String(params.limit));
    if (params?.level) qs.set("level", params.level);
    const q = qs.toString();
    return this.get(`/api/v1/logs${q ? `?${q}` : ""}`);
  }

  exportLogsUrl(): string {
    return `${this.base}/api/v1/logs/export`;
  }

  // ── Transcription ─────────────────────────────────────────

  async transcribe(wav: ArrayBuffer, token?: string): Promise<TranscribeResponse> {
    const headers: Record<string, string> = {};
    const tok = token ?? this.token;
    if (tok) headers["Authorization"] = `Bearer ${tok}`;

    const form = new FormData();
    form.append("audio", new Blob([wav], { type: "audio/wav" }), "audio.wav");

    const res = await fetch(`${this.base}/api/v1/transcribe`, {
      method: "POST",
      headers,
      body: form,
    });

    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).message ?? msg; } catch { /* ignore */ }
      throw new ApiError(res.status, msg);
    }

    return res.json() as Promise<TranscribeResponse>;
  }

  // ── Wake-word calibration ────────────────────────────────────

  /** Submit one WAV recording as a calibration sample. */
  async calibrateWakeWord(wav: ArrayBuffer): Promise<CalibrateResponse> {
    const headers: Record<string, string> = {};
    if (this.token) headers["Authorization"] = `Bearer ${this.token}`;

    const form = new FormData();
    form.append("audio", new Blob([wav], { type: "audio/wav" }), "sample.wav");

    const res = await fetch(`${this.base}/api/v1/voice/calibrate`, {
      method: "POST",
      headers,
      body: form,
    });

    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).message ?? msg; } catch { /* ignore */ }
      throw new ApiError(res.status, msg);
    }

    return res.json() as Promise<CalibrateResponse>;
  }

  /** Clear all calibration data. Detector reverts to raw wake-word phrase. */
  async resetWakeWordCalibration(): Promise<void> {
    await this.del("/api/v1/voice/calibrate");
  }
}

// Singleton — the Tauri backend injects window.__GIAP_SERVER_URL__
export const api = new PondApiClient();
