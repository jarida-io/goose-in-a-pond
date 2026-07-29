import {
  ApiError,
  type AddExtensionRequest,
  type AgentChatStreamRequest,
  type AgentRecipe,
  type AgentTool,
  type CalibrateResponse,
  type ChallengeResponse,
  type ChatEvent,
  type ChatStreamRequest,
  type CleanupResponse,
  type Device,
  type DiskUsage,
  type DownloadEntry,
  type Extension,
  type FaceModelsResponse,
  type HandshakeResponse,
  type HealthResponse,
  type HfModel,
  type HfModelFile,
  type LlamafileRelease,
  type LogEntry,
  type MarketplaceExtension,
  type MemoryFragment,
  type ModelActiveRoles,
  type ModelEntry,
  type ModelMemoryStatus,
  type OllamaModel,
  type PairingCodeResponse,
  type PromptExtra,
  type PromptTemplate,
  type Schedule,
  type ScheduleRun,
  type SecretRequirement,
  type SessionMessage,
  type SessionMessageToolCall,
  type SessionSummary,
  type MusicControlAction,
  type NowPlayingApiResponse,
  type Settings,
  type UsageSummary,
  type TranscribeResponse,
  type UserSkill,
  type WeatherApiResponse,
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

// Resolve the pond-server base URL for the current runtime.
//   1. Tauri desktop shell injects `window.__GIAP_SERVER_URL__` (a local server).
//   2. Dashboard served over HTTP by the single-executable → the API lives at
//      the SAME origin the page was loaded from. Using window.location.origin
//      makes remote/LAN access work (same-origin, no CORS) instead of every
//      request hitting the *viewer's* own 127.0.0.1.
//   3. Fallback (SSR / non-browser / tests): the conventional local server.
export function defaultServerUrl(): string {
  if (typeof window !== "undefined") {
    if (window.__GIAP_SERVER_URL__) return window.__GIAP_SERVER_URL__;
    const isTauri = "__TAURI_INTERNALS__" in window;
    if (!isTauri && window.location?.origin?.startsWith("http")) {
      return window.location.origin;
    }
  }
  return "http://127.0.0.1:4000";
}

export class PondApiClient {
  private readonly base: string;
  private token: string | null;
  private refreshToken: string | null = null;
  private tokenExpiresAt: number | null = null;
  private refreshPromise: Promise<void> | null = null;

  private static readonly LS_SESSION = "giap-session-token";
  private static readonly LS_REFRESH = "giap-refresh-token";
  private static readonly LS_EXPIRES = "giap-token-expires-at";

  constructor(base?: string, token?: string | null) {
    this.base = (base ?? defaultServerUrl()).replace(/\/$/, "");
    this.token = token ?? null;
    // Hydrate persisted tokens so the desktop survives restarts without
    // re-pairing. An explicit constructor token takes precedence.
    if (!this.token) this.loadPersistedTokens();
  }

  /** Load session/refresh/expiry from localStorage (no-op if unavailable). */
  private loadPersistedTokens(): void {
    try {
      this.token = localStorage.getItem(PondApiClient.LS_SESSION);
      this.refreshToken = localStorage.getItem(PondApiClient.LS_REFRESH);
      const exp = localStorage.getItem(PondApiClient.LS_EXPIRES);
      this.tokenExpiresAt = exp ? Number(exp) || null : null;
    } catch { /* no localStorage (tests / SSR) */ }
  }

  /** Persist the current token triple (no-op if localStorage is unavailable). */
  private persistTokens(): void {
    try {
      const set = (k: string, v: string | null) =>
        v ? localStorage.setItem(k, v) : localStorage.removeItem(k);
      set(PondApiClient.LS_SESSION, this.token);
      set(PondApiClient.LS_REFRESH, this.refreshToken);
      set(PondApiClient.LS_EXPIRES, this.tokenExpiresAt ? String(this.tokenExpiresAt) : null);
    } catch { /* ignore */ }
  }

  /**
   * Set the active session token. Pass `expiresAt` (RFC3339, the server's
   * `expires_at`) to (re)arm the proactive refresh timer; omit it to update
   * only the bearer token while preserving the known expiry (used by callers
   * that just need the header). Pass `null` to clear the expiry.
   */
  setToken(token: string | null, expiresAt?: string | null): void {
    this.token = token;
    if (expiresAt !== undefined) {
      this.tokenExpiresAt = token && expiresAt ? Date.parse(expiresAt) || null : null;
    }
    this.persistTokens();
  }

  private async ensureTokenFresh(): Promise<void> {
    if (!this.token || !this.tokenExpiresAt) return;
    if (Date.now() < this.tokenExpiresAt - 60_000) return;
    if (!this.refreshToken) return;
    if (this.refreshPromise) return this.refreshPromise;
    // Use a token-less fetch (handshakeFetch) so this can't recurse back into
    // ensureTokenFresh via request().
    this.refreshPromise = this.handshakeFetch<HandshakeResponse>("POST", "/api/v1/handshake/refresh", {
      refresh_token: this.refreshToken,
    })
      .then((res) => {
        if (res.accepted && res.session_token) {
          this.refreshToken = res.refresh_token ?? this.refreshToken;
          this.setToken(res.session_token, res.expires_at ?? null); // persists all three
        }
      })
      .catch(() => { /* refresh failed — continue with current token */ })
      .finally(() => { this.refreshPromise = null; });
    return this.refreshPromise;
  }

  // ── Internal helpers ───────────────────────────────────────

  private headers(extra?: Record<string, string>): Record<string, string> {
    const h: Record<string, string> = { "Content-Type": "application/json", ...extra };
    if (this.token) h["Authorization"] = `Bearer ${this.token}`;
    return h;
  }

  private async request<T>(method: string, path: string, body?: unknown, timeout?: number, _retry = false): Promise<T> {
    await this.ensureTokenFresh();
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), timeout ?? 30_000);
    try {
      const res = await fetch(`${this.base}${path}`, {
        method,
        headers: this.headers(),
        body: body !== undefined ? JSON.stringify(body) : undefined,
        signal: controller.signal,
      });
      clearTimeout(timeoutId);
      if (res.status === 401 && !_retry) {
        // Server restarted — in-memory session token was cleared. Re-handshake
        // using the persisted refresh token, then retry once.
        this.setToken(null);
        await this.connect();
        return this.request<T>(method, path, body, timeout, true);
      }
      if (!res.ok) {
        let msg = res.statusText;
        try { msg = (await res.json()).error ?? msg; } catch { /* ignore */ }
        throw new ApiError(res.status, msg);
      }
      // 204 No Content and any other empty body — return undefined cast to T
      const ct = res.headers.get("content-type") ?? "";
      if (res.status === 204 || !ct.includes("json")) return undefined as unknown as T;
      return res.json() as Promise<T>;
    } catch (e) {
      clearTimeout(timeoutId);
      if (e instanceof DOMException && e.name === "AbortError") {
        throw new ApiError(408, "Request timed out");
      }
      throw e;
    }
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

  getSystemInfo(): Promise<{ hostname: string; port: number; version: string; platform: string; arch: string }> {
    return this.get("/api/v1/system/info");
  }

  // ── Onboarding ────────────────────────────────────────────

  getOnboardingStatus(): Promise<{ onboarded: boolean; current_step: string; steps_completed: number; total_steps: number }> {
    return this.get("/api/v1/onboard/status");
  }

  /**
   * Record that the wizard has reached an onboarding step. `step` is a backend
   * `OnboardingStep` variant name (e.g. "Basics", "WakeWord"). Progress is
   * monotonic server-side, so re-reporting an earlier step is a safe no-op.
   */
  recordOnboardingStep(
    step: string,
  ): Promise<{ onboarded: boolean; current_step: string; steps_completed: number; total_steps: number }> {
    return this.post(`/api/v1/onboard/step/${encodeURIComponent(step)}`);
  }

  completeOnboarding(): Promise<{ status: string }> {
    return this.post("/api/v1/onboard/complete");
  }

  /**
   * Reset onboarding back to the first step ("Start over"). Clears persisted
   * progress and re-arms the onboarding guard so the wizard shows again.
   */
  resetOnboarding(): Promise<{ onboarded: boolean; current_step: string; steps_completed: number; total_steps: number }> {
    return this.post("/api/v1/onboard/reset");
  }

  /**
   * Synthesize `text` to speech and return the raw audio bytes (WAV) for
   * client-side playback. Throws {@link ApiError} (e.g. 503) when no TTS
   * backend is running — callers should degrade gracefully.
   */
  async synthesizeSpeech(text: string): Promise<ArrayBuffer> {
    await this.ensureTokenFresh();
    const res = await fetch(`${this.base}/api/v1/tts`, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify({ text }),
    });
    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).error ?? msg; } catch { /* ignore */ }
      throw new ApiError(res.status, msg);
    }
    return res.arrayBuffer();
  }

  // ── Settings ──────────────────────────────────────────────

  getSettings(): Promise<Settings> {
    return this.get("/api/v1/settings");
  }

  updateSettings(patch: Partial<Settings>): Promise<Settings> {
    return this.put("/api/v1/settings", patch);
  }

  // ── Weather ───────────────────────────────────────────────

  getWeather(): Promise<WeatherApiResponse> {
    return this.get("/api/v1/weather");
  }

  // ── Music ─────────────────────────────────────────────────

  getNowPlaying(): Promise<NowPlayingApiResponse> {
    return this.get("/api/v1/music/now-playing");
  }

  controlMusic(action: MusicControlAction): Promise<{ ok: boolean }> {
    return this.post("/api/v1/music/control", { action });
  }

  // ── Devices ───────────────────────────────────────────────

  listDevices(): Promise<Device[]> {
    return this.get<{ devices: Device[] } | Device[]>("/api/v1/devices").then((r) =>
      Array.isArray(r) ? r : (r as { devices: Device[] }).devices ?? [],
    );
  }

  registerDevice(req: {
    name: string;
    device_type: string;
    hostname?: string;
    capabilities: string[];
    room?: string;
  }): Promise<Device> {
    return this.post<Device>("/api/v1/devices", req);
  }

  unregisterDevice(id: string): Promise<void> {
    return this.del(`/api/v1/devices/${encodeURIComponent(id)}`);
  }

  /** Mark a device online ("Turn on" in the Devices UI) — refreshes its heartbeat. */
  markDeviceOnline(id: string): Promise<void> {
    return this.post(`/api/v1/devices/${encodeURIComponent(id)}/heartbeat`, {});
  }

  /** Mark a device offline ("Turn off" in the Devices UI). */
  markDeviceOffline(id: string): Promise<void> {
    return this.post(`/api/v1/devices/${encodeURIComponent(id)}/offline`, {});
  }

  /** Save edits from the Devices "Configure" panel (name, hostname, room). */
  updateDevice(
    id: string,
    req: { name: string; hostname?: string; room?: string },
  ): Promise<Device> {
    return this.put<Device>(`/api/v1/devices/${encodeURIComponent(id)}`, req);
  }

  // ── Schedules ─────────────────────────────────────────────

  listSchedules(): Promise<Schedule[]> {
    return this.get<Array<Record<string, unknown>>>("/api/v1/schedules").then((items) =>
      (Array.isArray(items) ? items : []).map((t) => {
        // Extract prompt from kind.prompt or legacy payload.prompt
        const kind = t.kind as Record<string, unknown> | undefined;
        const payload = t.payload as Record<string, unknown> | undefined;
        const prompt = (kind?.prompt as string) ?? (payload?.prompt as string) ?? "";
        return {
          id: t.id as string,
          name: (t.label ?? t.name ?? "") as string,
          cron: t.cron as string,
          prompt,
          enabled: t.paused !== undefined ? !(t.paused as boolean) : (t.enabled as boolean ?? true),
          timezone: (t.timezone as string) ?? "UTC",
          kind: t.kind as Schedule["kind"],
          last_run: t.last_run as string | undefined,
          next_run: t.next_run as string | undefined,
          created_at: t.created_at as string | undefined,
        };
      }),
    );
  }

  createSchedule(body: Omit<Schedule, "id" | "created_at">): Promise<Schedule> {
    return this.post<Record<string, unknown>>("/api/v1/schedules", {
      name: body.name,
      cron: body.cron,
      prompt: body.prompt,
      timezone: body.timezone ?? "UTC",
    }).then((t) => {
      const kind = t.kind as Record<string, unknown> | undefined;
      const prompt = (kind?.prompt as string) ?? body.prompt;
      return {
        id: t.id as string,
        name: (t.label ?? t.name ?? body.name) as string,
        cron: t.cron as string,
        prompt,
        enabled: t.paused !== undefined ? !(t.paused as boolean) : true,
        timezone: (t.timezone as string) ?? body.timezone ?? "UTC",
        kind: t.kind as Schedule["kind"],
        created_at: t.created_at as string | undefined,
      };
    });
  }

  deleteSchedule(id: string): Promise<void> {
    return this.del(`/api/v1/schedules/${encodeURIComponent(id)}`);
  }

  async updateSchedule(
    id: string,
    patch: { name?: string; cron?: string; prompt?: string; timezone?: string },
  ): Promise<Schedule> {
    const t = await this.put<Record<string, unknown>>(
      `/api/v1/schedules/${encodeURIComponent(id)}`,
      patch,
    );
    const kind = t.kind as Record<string, unknown> | undefined;
    const prompt = (kind?.prompt as string) ?? patch.prompt ?? "";
    return {
      id: t.id as string,
      name: ((t.label ?? t.name) as string) || "",
      cron: t.cron as string,
      prompt,
      enabled: t.paused !== undefined ? !(t.paused as boolean) : true,
      timezone: (t.timezone as string) ?? "UTC",
      kind: t.kind as Schedule["kind"],
      last_run: t.last_run as string | undefined,
      next_run: t.next_run as string | undefined,
      created_at: t.created_at as string | undefined,
    };
  }

  getScheduleRuns(id: string, limit = 10): Promise<ScheduleRun[]> {
    return this.get<ScheduleRun[]>(`/api/v1/schedules/${encodeURIComponent(id)}/runs?limit=${limit}`);
  }

  getUpcomingSchedules(limit = 10): Promise<Schedule[]> {
    return this.get<Schedule[]>(`/api/v1/schedules/upcoming?limit=${limit}`);
  }

  /** Fetch recent runs across all schedules, merged and sorted by start time. */
  async getAllRecentRuns(perScheduleLimit = 5): Promise<Array<ScheduleRun & { schedule_name: string }>> {
    const schedules = await this.listSchedules();
    const runSets = await Promise.all(
      schedules.map(async (s) => {
        try {
          const runs = await this.getScheduleRuns(s.id, perScheduleLimit);
          return runs.map((r) => ({ ...r, schedule_name: s.name || s.label || s.id }));
        } catch {
          return [];
        }
      }),
    );
    return runSets
      .flat()
      .sort((a, b) => (a.started_at < b.started_at ? 1 : -1));
  }

  // ── MCP Apps ──────────────────────────────────────────────

  /** Fetch an MCP App resource by its ui:// URI. Returns the HTML content. */
  async getMcpResource(uri: string): Promise<string> {
    const res = await this.get<{ contents: Array<{ text?: string }> }>(
      `/api/v1/mcp/resources?uri=${encodeURIComponent(uri)}`,
    );
    return res.contents?.[0]?.text ?? "";
  }

  /** Execute an MCP tool by name with arguments. Returns the tool result. */
  async callTool(name: string, args: Record<string, unknown>): Promise<unknown> {
    return this.post<unknown>("/api/v1/mcp/tools/call", { name, arguments: args });
  }

  /**
   * Invoke an MCP tool directly (bypasses the LLM) via `POST /api/v1/tools/invoke`.
   * Used by the Hub to actuate devices without a chat turn.
   */
  async invokeTool(req: {
    server: string;
    tool: string;
    args: Record<string, unknown>;
  }): Promise<{ tool: string; success: boolean; content: string }> {
    return this.post("/api/v1/tools/invoke", {
      server: req.server,
      tool: req.tool,
      args: req.args,
    });
  }

  // ── Usage ─────────────────────────────────────────────────

  getUsageSummary(): Promise<UsageSummary> {
    return this.get<UsageSummary>("/api/v1/usage/summary");
  }

  // ── Memory ────────────────────────────────────────────────

  listMemories(limit = 20): Promise<MemoryFragment[]> {
    return this.get(`/api/v1/memories?limit=${limit}`);
  }

  addMemory(
    content: string,
    tags?: string[],
    segment?: import("./types").MemorySegment,
    importance?: number,
    tier?: import("./types").MemoryTier,
  ): Promise<MemoryFragment> {
    return this.post("/api/v1/memories", {
      content,
      ...(tags !== undefined && { tags }),
      ...(segment !== undefined && { segment }),
      ...(importance !== undefined && { importance }),
      ...(tier !== undefined && { tier }),
    });
  }

  deleteMemory(id: string): Promise<void> {
    return this.del(`/api/v1/memories/${id}`);
  }

  // ── Consolidation ─────────────────────────────────────────

  /** Start manual consolidation. Returns an SSE stream of ConsolidationEvent. */
  async *streamConsolidation(): AsyncGenerator<import("./types").ConsolidationEvent> {
    const res = await fetch(`${this.base}/api/v1/memory/consolidate`, {
      method: "POST",
      headers: this.headers(),
    });
    if (!res.ok || !res.body) return;
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";
      for (const line of lines) {
        if (!line.startsWith("data: ")) continue;
        const data = line.slice(6).trim();
        if (!data) continue;
        try {
          yield JSON.parse(data) as import("./types").ConsolidationEvent;
        } catch { /* skip malformed */ }
      }
    }
  }

  /** Stop an in-progress consolidation. */
  stopConsolidation(): Promise<void> {
    return this.post("/api/v1/memory/consolidate/stop", {});
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

  /**
   * Token-less fetch for the public handshake endpoints. Deliberately bypasses
   * `request()`/`ensureTokenFresh()` so refresh/pairing can't recurse.
   */
  private async handshakeFetch<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await fetch(`${this.base}${path}`, {
      method,
      headers: { "Content-Type": "application/json" },
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (!res.ok) throw new ApiError(res.status, res.statusText);
    return res.json() as Promise<T>;
  }

  /** HMAC-SHA256(pairingCode, challenge ‖ clientId) → lowercase hex (Web Crypto). */
  private async computeMac(code: string, challengeB64: string, clientId: string): Promise<string> {
    const enc = new TextEncoder();
    const challenge = Uint8Array.from(atob(challengeB64), (c) => c.charCodeAt(0));
    const idBytes = enc.encode(clientId);
    const msg = new Uint8Array(challenge.length + idBytes.length);
    msg.set(challenge);
    msg.set(idBytes, challenge.length);
    const key = await crypto.subtle.importKey(
      "raw",
      enc.encode(code),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["sign"],
    );
    const sig = await crypto.subtle.sign("HMAC", key, msg);
    return Array.from(new Uint8Array(sig))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
  }

  /**
   * Auto-pair this desktop install with the local server. Because the desktop
   * and server share a machine, we read the pairing code off the loopback-only
   * endpoint and run the full two-phase handshake — no operator typing needed.
   * On success the session+refresh tokens are stored on this client.
   */
  async pair(clientId = "pond-desktop"): Promise<HandshakeResponse> {
    // Read the current code; if none is active (e.g. the startup code expired
    // after 10 min), ISSUE a fresh one. Both endpoints are loopback-only, so the
    // same-host desktop is trusted to mint its own code — this is what makes
    // silent auto-pair actually reliable instead of failing once the operator's
    // startup code lapses.
    let pc = await this.handshakeFetch<PairingCodeResponse>("GET", "/api/v1/handshake/pairing-code");
    if (!pc.code) {
      pc = await this.handshakeFetch<PairingCodeResponse>("POST", "/api/v1/handshake/pairing-code");
    }
    if (!pc.code) {
      throw new ApiError(409, "could not obtain a pairing code from the local server");
    }
    const init = await this.handshakeFetch<ChallengeResponse>("POST", "/api/v1/handshake/init", {
      client_id: clientId,
      client_type: "desktop",
      client_version: "1.0.0",
    });
    const mac = await this.computeMac(pc.code, init.challenge, clientId);
    const res = await this.handshakeFetch<HandshakeResponse>("POST", "/api/v1/handshake/verify", {
      challenge_id: init.challenge_id,
      mac,
      device_name: "Pond Desktop",
    });
    if (res.accepted && res.session_token) {
      this.refreshToken = res.refresh_token ?? null;
      this.setToken(res.session_token, res.expires_at ?? null); // persists all three
    }
    return res;
  }

  /**
   * Establish an authenticated session, reusing a persisted token across app
   * restarts. Tries, in order: a still-valid stored session token → a refresh
   * with the stored refresh token (no pairing code needed) → a fresh pair
   * (needs the server's current pairing code). Returns the active session
   * token, or `null` if none could be established.
   */
  async connect(clientId = "pond-desktop"): Promise<string | null> {
    // 1. Stored session token still comfortably valid.
    if (this.token && this.tokenExpiresAt && Date.now() < this.tokenExpiresAt - 60_000) {
      return this.token;
    }
    // 2. Refresh with a stored refresh token — survives restarts for 30 days
    //    without ever needing the pairing code again.
    if (this.refreshToken) {
      try {
        const r = await this.handshakeFetch<HandshakeResponse>("POST", "/api/v1/handshake/refresh", {
          refresh_token: this.refreshToken,
        });
        if (r.accepted && r.session_token) {
          this.refreshToken = r.refresh_token ?? this.refreshToken;
          this.setToken(r.session_token, r.expires_at ?? null);
          return r.session_token;
        }
      } catch { /* fall through to a fresh pair */ }
    }
    // 3. Fresh pairing.
    const res = await this.pair(clientId);
    return res.accepted ? res.session_token : null;
  }

  // ── Pairing ───────────────────────────────────────────────

  /** Return the current unexpired pairing code, or null if none is active. Loopback-only. */
  getPairingCode(): Promise<PairingCodeResponse> {
    return this.handshakeFetch<PairingCodeResponse>("GET", "/api/v1/handshake/pairing-code");
  }

  /** Issue a fresh pairing code, replacing any existing one. Loopback-only. */
  issuePairingCode(): Promise<PairingCodeResponse> {
    return this.handshakeFetch<PairingCodeResponse>("POST", "/api/v1/handshake/pairing-code");
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
            downloaded: item.downloaded as boolean | undefined,
            description: item.description as string | undefined,
            size_mb: item.size_mb as number | undefined,
            category: (item.category as string | undefined) ?? category,
            filename: item.filename as string | undefined,
            url: item.url as string | undefined,
            asr_language: item.asr_language as string | undefined,
            asr_size: item.asr_size as string | undefined,
            tts_engine: item.tts_engine as string | undefined,
            config_filename: item.config_filename as string | undefined,
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
        chat:      normalize(raw.chat),
        tool:      raw.tool ?? null,
        asr:       normalize(raw.asr),
        tts:       normalize(raw.tts),
        embedding: normalize(raw.embedding),
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
    return this.get<{ messages: Array<Record<string, unknown>> } | Array<Record<string, unknown>>>(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/messages`,
    ).then((r) => {
      const raw = Array.isArray(r) ? r : (r as { messages: Array<Record<string, unknown>> }).messages ?? [];
      return raw.map((m): SessionMessage => ({
        id: m.id as string,
        session_id: m.session_id as string,
        role: m.role as SessionMessage["role"],
        content: (m.content as string) ?? "",
        created_at: m.created_at as string,
        tool_calls: m.tool_calls as SessionMessageToolCall[] | undefined,
        tool_call_id: m.tool_call_id as string | undefined,
      }));
    });
  }

  renameSession(sessionId: string, title: string): Promise<void> {
    return this.patch(`/api/v1/sessions/${encodeURIComponent(sessionId)}`, { title });
  }

  deleteSession(sessionId: string): Promise<void> {
    return this.del(`/api/v1/sessions/${encodeURIComponent(sessionId)}`);
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

  createRecipe(req: {
    name: string;
    description?: string;
    yaml: string;
  }): Promise<AgentRecipe> {
    return this.post("/api/v1/recipes", {
      name: req.name,
      description: req.description ?? "",
      yaml: req.yaml,
    });
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
    canvasMode?: boolean,
  ): AsyncGenerator<ChatEvent> {
    const reqBody: ChatStreamRequest = {
      message,
      session_id: sessionId,
      canvas_mode: canvasMode ?? false,
    };
    yield* this.streamSse("/api/v1/chat/stream", reqBody, token);
  }

  /**
   * Execute a recipe by name. Server resolves the recipe's prompt and streams
   * the agent's response using the same SSE event shape as `chatStream`.
   */
  async *runRecipe(
    name: string,
    opts?: { sessionId?: string; voiceMode?: boolean; canvasMode?: boolean; token?: string },
  ): AsyncGenerator<ChatEvent> {
    const reqBody = {
      session_id: opts?.sessionId,
      voice_mode: opts?.voiceMode ?? false,
      canvas_mode: opts?.canvasMode ?? false,
    };
    yield* this.streamSse(
      `/api/v1/recipes/${encodeURIComponent(name)}/run`,
      reqBody,
      opts?.token,
    );
  }

  /**
   * POST a JSON body to an SSE endpoint and yield each parsed event.
   * Shared by chatStream and runRecipe.
   */
  private async *streamSse(
    path: string,
    body: unknown,
    token?: string,
  ): AsyncGenerator<ChatEvent> {
    await this.ensureTokenFresh();
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    const tok = token ?? this.token;
    if (tok) headers["Authorization"] = `Bearer ${tok}`;

    // Retry initial connection on network-level failures (not HTTP errors).
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 120_000);
    let res!: Response;
    for (let attempt = 0; attempt <= 2; attempt++) {
      try {
        res = await fetch(`${this.base}${path}`, {
          method: "POST",
          headers,
          body: JSON.stringify(body),
          signal: controller.signal,
        });
        clearTimeout(timeoutId);
        break;
      } catch (e) {
        clearTimeout(timeoutId);
        if (e instanceof DOMException && e.name === "AbortError") {
          throw new ApiError(408, "Request timed out");
        }
        if (attempt === 2) throw e;
        await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
      }
    }

    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).error ?? msg; } catch { /* ignore */ }
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

  // Sweep unreferenced HF-cache blobs and stale resume files.
  cleanupModels(): Promise<CleanupResponse> {
    return this.post("/api/v1/models/cleanup");
  }

  // Per-category disk usage report (model dirs + hf_cache totals).
  getDiskUsage(): Promise<DiskUsage> {
    return this.get("/api/v1/models/disk-usage");
  }

  // Delete model file from disk (409 ApiError if model is active in a role)
  deleteModel(category: string, name: string): Promise<void> {
    return this.del(`/api/v1/models/${encodeURIComponent(category)}/${encodeURIComponent(name)}`);
  }

  // Trigger async download of a catalog model by category and name
  downloadModel(category: string, name: string): Promise<{ status: string }> {
    return this.post(`/api/v1/models/${encodeURIComponent(category)}/${encodeURIComponent(name)}/download`);
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
    await this.ensureTokenFresh();
    const reqBody: AgentChatStreamRequest = { message };
    if (sessionId) reqBody.session_id = sessionId;

    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (this.token) headers["Authorization"] = `Bearer ${this.token}`;

    // Retry initial connection on network-level failures (not HTTP errors).
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 120_000);
    let res!: Response;
    for (let attempt = 0; attempt <= 2; attempt++) {
      try {
        res = await fetch(`${this.base}/api/v1/agent/chat/stream`, {
          method: "POST",
          headers,
          body: JSON.stringify(reqBody),
          signal: controller.signal,
        });
        clearTimeout(timeoutId);
        break;
      } catch (e) {
        clearTimeout(timeoutId);
        if (e instanceof DOMException && e.name === "AbortError") {
          throw new ApiError(408, "Request timed out");
        }
        if (attempt === 2) throw e;
        await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
      }
    }

    if (!res.ok || !res.body) {
      let msg = res.statusText;
      try { msg = (await res.json()).error ?? msg; } catch { /* ignore */ }
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
          if (!trimmed || trimmed === "data: [DONE]") {
            if (trimmed === "data: [DONE]") yield { type: "done", done: true };
            continue;
          }
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

  // ── Marketplace ─────────────────────────────────────────

  async listMarketplace(): Promise<MarketplaceExtension[]> {
    const res = await this.get<{ extensions: MarketplaceExtension[] }>("/api/v1/marketplace");
    return res.extensions;
  }

  async installMarketplaceExtension(id: string, secrets?: Record<string, string>): Promise<Extension> {
    const body = secrets ? { secrets } : undefined;
    return this.post<Extension>(`/api/v1/marketplace/${encodeURIComponent(id)}/install`, body);
  }

  async getExtensionSecrets(name: string): Promise<{ requirements: SecretRequirement[]; fulfilled: Record<string, boolean> }> {
    return this.get(`/api/v1/extensions/${encodeURIComponent(name)}/secrets`);
  }

  async setExtensionSecrets(name: string, secrets: Record<string, string>): Promise<void> {
    await this.post(`/api/v1/extensions/${encodeURIComponent(name)}/secrets`, secrets);
  }

  // ── Secrets ──────────────────────────────────────────────

  async listSecretKeys(): Promise<string[]> {
    const res = await this.get<{ keys: string[] }>("/api/v1/secrets");
    return res.keys;
  }

  async checkSecret(key: string): Promise<boolean> {
    const res = await this.get<{ exists: boolean }>(`/api/v1/secrets/${encodeURIComponent(key)}/exists`);
    return res.exists;
  }

  async setSecret(key: string, value: string): Promise<void> {
    await this.put(`/api/v1/secrets/${encodeURIComponent(key)}`, { value });
  }

  async deleteSecret(key: string): Promise<void> {
    await this.del(`/api/v1/secrets/${encodeURIComponent(key)}`);
  }

  // ── Activity ──────────────────────────────────────────────

  listActivity(params?: import("./types").ActivityQueryParams): Promise<import("./types").ActivityResponse> {
    const qs = new URLSearchParams();
    if (params?.limit !== undefined) qs.set("limit", String(params.limit));
    if (params?.since) qs.set("since", params.since);
    if (params?.category) qs.set("category", params.category);
    if (params?.session_id) qs.set("session_id", params.session_id);
    const q = qs.toString();
    return this.get(`/api/v1/activity${q ? `?${q}` : ""}`);
  }

  getActivitySummary(window?: "hour" | "day" | "week"): Promise<import("./types").ActivitySummary> {
    const q = window ? `?window=${window}` : "";
    return this.get(`/api/v1/activity/summary${q}`);
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
      try { msg = (await res.json()).error ?? msg; } catch { /* ignore */ }
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
      try { msg = (await res.json()).error ?? msg; } catch { /* ignore */ }
      throw new ApiError(res.status, msg);
    }

    return res.json() as Promise<CalibrateResponse>;
  }

  /** Clear all calibration data. Detector reverts to raw wake-word phrase. */
  async resetWakeWordCalibration(): Promise<void> {
    await this.del("/api/v1/voice/calibrate");
  }

  // ── OAuth PKCE ──────────────────────────────────────────────

  /** Start an OAuth PKCE flow. Returns the authorization URL to open in a browser. */
  async initiateOAuth(provider: string, extensionId?: string): Promise<{ auth_url: string; state: string }> {
    return this.post("/api/v1/oauth/authorize", { provider, extension_id: extensionId });
  }

  /** Refresh an expired OAuth access token. */
  async refreshOAuth(provider: string): Promise<void> {
    await this.post("/api/v1/oauth/refresh", { provider });
  }

  /** List supported OAuth providers. */
  async listOAuthProviders(): Promise<{ id: string; display_name: string; scopes: string[] }[]> {
    const res = await this.get<{ providers: { id: string; display_name: string; scopes: string[] }[] }>("/api/v1/oauth/providers");
    return res.providers;
  }
}

// Singleton — the Tauri backend injects window.__GIAP_SERVER_URL__
export const api = new PondApiClient();
