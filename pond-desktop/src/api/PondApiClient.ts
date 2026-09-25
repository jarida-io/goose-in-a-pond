import {
  ApiError,
  type AddExtensionRequest,
  type AgentRecipe,
  type AgentTool,
  type CalibrateResponse,
  type ChallengeResponse,
  type ChatEvent,
  type ChatStreamRequest,
  type CleanupResponse,
  type CompactionReport,
  type ContextIndexHealth,
  type ContextIndexRebuild,
  type AccountSyncSummary,
  type ContextItem,
  type ContextSource,
  type Device,
  type DiskUsage,
  type DownloadEntry,
  type Extension,
  type FaceModelsResponse,
  type HandshakeResponse,
  type HealthResponse,
  type HfModel,
  type HfModelFile,
  type ImageAttachment,
  type ActiveRun,
  type LlamafileRelease,
  type LogEntry,
  type MarketplaceExtension,
  type MatterStatus,
  type MemoryFragment,
  type MeshPeer,
  type MeshPeerCapabilities,
  type MeshSettlementStatus,
  type MeshSelf,
  type ModelActiveRoles,
  type ModelEntry,
  type ModelMemoryStatus,
  type OllamaModel,
  type PairingCodeResponse,
  type PromptTemplate,
  type RetitleOneResult,
  type RetitleResult,
  type Schedule,
  type ScheduleRun,
  type SecretRequirement,
  type SessionMessage,
  type SessionMessageToolCall,
  type ProposalDecision,
  type ProposalList,
  type SessionSummary,
  type MusicControlAction,
  type NowPlayingApiResponse,
  type Settings,
  type UsageSummary,
  type TranscribeResponse,
  type UserSkill,
  type WeatherApiResponse,
  type ZoneChoice,
  type DetectedPlace,
} from "./types";
import { voiceTitle } from "../voice/voiceCatalogue";

// All REST calls MUST go through PondApiClient; no fetch() elsewhere.

declare global {
  interface Window {
    __GIAP_SERVER_URL__?: string;
  }
}

/** Shell-injected URL, else the serving page's origin (LAN access, no CORS), else localhost. */
export function defaultServerUrl(): string {
  if (typeof window !== "undefined") {
    if (window.__GIAP_SERVER_URL__) return window.__GIAP_SERVER_URL__;
    // The shell's app://giap origin is not a server; only an http(s) page origin is.
    if (window.location?.origin?.startsWith("http")) {
      return window.location.origin;
    }
  }
  return "http://127.0.0.1:4000";
}

/** Just above the server's 180s pairing timeout, so the server's error surfaces, not an abort. */
const COMMISSION_TIMEOUT_MS = 190_000;

export class PondApiClient {
  /** Mutable: the shell can correct this when the sidecar binds a fallback port. */
  private base: string;
  private token: string | null;
  private refreshToken: string | null = null;
  private tokenExpiresAt: number | null = null;
  private refreshPromise: Promise<void> | null = null;
  // In-flight re-auth after a 401, shared so a burst of 401s re-pairs once.
  private reauthPromise: Promise<string | null> | null = null;

  private static readonly LS_SESSION = "giap-session-token";
  private static readonly LS_REFRESH = "giap-refresh-token";
  private static readonly LS_EXPIRES = "giap-token-expires-at";
  private static readonly LS_CLIENT_ID = "giap-client-id";

  /**
   * Persisted per-install client id. Pairing revokes earlier sessions for the same id, so separate
   * clients need distinct ids; tabs in one profile share one (and its session).
   */
  private cachedClientId: string | null = null;
  private clientId(): string {
    if (this.cachedClientId) return this.cachedClientId;
    let id: string | null = null;
    try {
      id = localStorage.getItem(PondApiClient.LS_CLIENT_ID);
      if (!id) {
        id = `pond-desktop-${PondApiClient.randomId()}`;
        localStorage.setItem(PondApiClient.LS_CLIENT_ID, id);
      }
    } catch {
      // No localStorage (tests / SSR): fall back to a fresh id for this process.
      id = `pond-desktop-${PondApiClient.randomId()}`;
    }
    this.cachedClientId = id;
    return id;
  }

  private static randomId(): string {
    try {
      if (typeof crypto !== "undefined" && crypto.randomUUID) {
        return crypto.randomUUID().slice(0, 8);
      }
    } catch {
      /* fall through */
    }
    return Math.random().toString(36).slice(2, 10);
  }

  constructor(base?: string, token?: string | null) {
    this.base = (base ?? defaultServerUrl()).replace(/\/$/, "");
    this.token = token ?? null;
    // Persisted tokens survive restarts without re-pairing; an explicit token wins.
    if (!this.token) this.loadPersistedTokens();
  }

  /** Load session/refresh/expiry from localStorage (no-op if unavailable). */
  private loadPersistedTokens(): void {
    try {
      this.token = localStorage.getItem(PondApiClient.LS_SESSION);
      this.refreshToken = localStorage.getItem(PondApiClient.LS_REFRESH);
      const exp = localStorage.getItem(PondApiClient.LS_EXPIRES);
      this.tokenExpiresAt = exp ? Number(exp) || null : null;
    } catch {
      /* no localStorage (tests / SSR) */
    }
  }

  /** Persist the current token triple (no-op if localStorage is unavailable). */
  private persistTokens(): void {
    try {
      const set = (k: string, v: string | null) =>
        v ? localStorage.setItem(k, v) : localStorage.removeItem(k);
      set(PondApiClient.LS_SESSION, this.token);
      set(PondApiClient.LS_REFRESH, this.refreshToken);
      set(
        PondApiClient.LS_EXPIRES,
        this.tokenExpiresAt ? String(this.tokenExpiresAt) : null,
      );
    } catch {
      /* ignore */
    }
  }

  /** Requests read `this.base` per call, so this retargets the whole singleton. */
  setBase(url: string): void {
    this.base = url.replace(/\/$/, "");
  }

  /** `expiresAt` (RFC3339) arms proactive refresh; omit to keep the expiry, `null` to clear it. */
  setToken(token: string | null, expiresAt?: string | null): void {
    this.token = token;
    if (expiresAt !== undefined) {
      this.tokenExpiresAt =
        token && expiresAt ? Date.parse(expiresAt) || null : null;
    }
    this.persistTokens();
  }

  private async ensureTokenFresh(): Promise<void> {
    if (!this.token || !this.tokenExpiresAt) return;
    if (Date.now() < this.tokenExpiresAt - 60_000) return;
    if (!this.refreshToken) return;
    if (this.refreshPromise) return this.refreshPromise;
    // handshakeFetch is token-less, so this cannot recurse into ensureTokenFresh via request().
    this.refreshPromise = this.handshakeFetch<HandshakeResponse>(
      "POST",
      "/api/v1/handshake/refresh",
      {
        refresh_token: this.refreshToken,
      },
    )
      .then((res) => {
        if (res.accepted && res.session_token) {
          this.refreshToken = res.refresh_token ?? this.refreshToken;
          this.setToken(res.session_token, res.expires_at ?? null); // persists all three
        }
      })
      .catch(() => {
        /* refresh failed — continue with current token */
      })
      .finally(() => {
        this.refreshPromise = null;
      });
    return this.refreshPromise;
  }

  // ── Internal helpers ───────────────────────────────────────

  private headers(
    method: string,
    extra?: Record<string, string>,
  ): Record<string, string> {
    // Content-Type on a bodyless GET forces a CORS preflight; only bodies need it.
    const h: Record<string, string> =
      method === "GET"
        ? { ...extra }
        : { "Content-Type": "application/json", ...extra };
    if (this.token) h["Authorization"] = `Bearer ${this.token}`;
    return h;
  }

  private async request<T>(
    method: string,
    path: string,
    body?: unknown,
    timeout?: number,
    _retry = false,
  ): Promise<T> {
    await this.ensureTokenFresh();
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), timeout ?? 30_000);
    try {
      const res = await fetch(`${this.base}${path}`, {
        method,
        headers: this.headers(method),
        body: body !== undefined ? JSON.stringify(body) : undefined,
        signal: controller.signal,
      });
      clearTimeout(timeoutId);
      if (res.status === 401 && !_retry) {
        // Token rejected (e.g. rotated): re-pair once, coalesced, then retry.
        await this.reauthenticate();
        return this.request<T>(method, path, body, timeout, true);
      }
      if (!res.ok) {
        let msg = res.statusText;
        try {
          msg = (await res.json()).error ?? msg;
        } catch {
          /* ignore */
        }
        throw new ApiError(res.status, msg);
      }
      const ct = res.headers.get("content-type") ?? "";
      if (res.status === 204 || !ct.includes("json"))
        return undefined as unknown as T;
      return res.json() as Promise<T>;
    } catch (e) {
      clearTimeout(timeoutId);
      if (e instanceof DOMException && e.name === "AbortError") {
        throw new ApiError(408, "Request timed out");
      }
      throw e;
    }
  }

  private get<T>(path: string): Promise<T> {
    return this.request<T>("GET", path);
  }
  private post<T>(path: string, body?: unknown, timeout?: number): Promise<T> {
    return this.request<T>("POST", path, body, timeout);
  }
  private put<T>(path: string, body?: unknown): Promise<T> {
    return this.request<T>("PUT", path, body);
  }
  private patch<T = void>(path: string, body?: unknown): Promise<T> {
    return this.request<T>("PATCH", path, body);
  }
  private del<T = void>(path: string): Promise<T> {
    return this.request<T>("DELETE", path);
  }

  // ── Health ────────────────────────────────────────────────

  health(): Promise<HealthResponse> {
    return this.get("/api/v1/health");
  }

  getSystemInfo(): Promise<{
    hostname: string;
    /** LAN IPv4 for phones that cannot resolve `<hostname>.local`; null without a LAN route. */
    lan_address: string | null;
    /** Tailscale IPv4, reachable from outside the house. Null unless this Pond is on a tailnet. */
    tailnet_address: string | null;
    port: number;
    version: string;
    platform: string;
    arch: string;
  }> {
    return this.get("/api/v1/system/info");
  }

  // ── Onboarding ────────────────────────────────────────────

  getOnboardingStatus(): Promise<{
    onboarded: boolean;
    current_step: string;
    steps_completed: number;
    total_steps: number;
  }> {
    return this.get("/api/v1/onboard/status");
  }

  /** `step`: an `OnboardingStep` variant name. Progress is monotonic; an earlier step is a no-op. */
  recordOnboardingStep(
    step: string,
  ): Promise<{
    onboarded: boolean;
    current_step: string;
    steps_completed: number;
    total_steps: number;
  }> {
    return this.post(`/api/v1/onboard/step/${encodeURIComponent(step)}`);
  }

  completeOnboarding(): Promise<{ status: string }> {
    return this.post("/api/v1/onboard/complete");
  }

  /** "Start over": clears progress and re-arms the guard so the wizard shows again. */
  resetOnboarding(): Promise<{
    onboarded: boolean;
    current_step: string;
    steps_completed: number;
    total_steps: number;
  }> {
    return this.post("/api/v1/onboard/reset");
  }

  /** WAV bytes for `text`; throws {@link ApiError} (e.g. 503) when no TTS backend runs. */
  async synthesizeSpeech(text: string): Promise<ArrayBuffer> {
    await this.ensureTokenFresh();
    const res = await fetch(`${this.base}/api/v1/tts`, {
      method: "POST",
      headers: this.headers("POST"),
      body: JSON.stringify({ text }),
    });
    if (!res.ok) {
      let msg = res.statusText;
      try {
        msg = (await res.json()).error ?? msg;
      } catch {
        /* ignore */
      }
      throw new ApiError(res.status, msg);
    }
    return res.arrayBuffer();
  }

  // ── Settings ──────────────────────────────────────────────

  getSettings(): Promise<Settings> {
    return this.get("/api/v1/settings");
  }

  /**
   * Send ONLY changed keys (`diffSettings`): each is marked `is_user_set` for good and clobbers
   * other surfaces' writes (docs/developer/settings-defaults-and-user-intent.md). Floats return
   * f32-rounded: fold the sent patch into any baseline; epsilon compares break integers > 2^24.
   */
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
    return this.get<{ devices: Device[] } | Device[]>("/api/v1/devices").then(
      (r) =>
        Array.isArray(r) ? r : ((r as { devices: Device[] }).devices ?? []),
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

  /** `name` is also written to the device. Own timeout: the server allows 180s to pair. */
  commissionDevice(
    code: string,
    name?: string,
  ): Promise<{ id: string; name: string; node_id: number }> {
    return this.request(
      "POST",
      "/api/v1/devices/commission",
      name ? { code, name } : { code },
      COMMISSION_TIMEOUT_MS,
    );
  }

  /** Polled while the Matter controller starts up; the settings save does not wait for it. */
  getMatterStatus(): Promise<MatterStatus> {
    return this.get<MatterStatus>("/api/v1/matter/status");
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

  // ── Mesh ──────────────────────────────────────────────────

  listMeshPeers(): Promise<MeshPeer[]> {
    return this.get<{ peers: MeshPeer[] }>("/api/v1/mesh/peers").then(
      (r) => r?.peers ?? [],
    );
  }

  addMeshPeer(req: {
    peer_id: string;
    trust_scope: "self_owned" | "circle";
    address?: string;
  }): Promise<{ peer_id: string; trust_scope: string }> {
    return this.post("/api/v1/mesh/peers", req);
  }

  removeMeshPeer(peerId: string): Promise<void> {
    return this.del(`/api/v1/mesh/peers/${encodeURIComponent(peerId)}`);
  }

  /** Manual top-up standing in for real Lightning settlement — not built yet. */
  topUpMeshPeer(
    peerId: string,
    amountMillisats: number,
  ): Promise<{ peer_id: string; credit_balance_millisats: number }> {
    return this.post(
      `/api/v1/mesh/peers/${encodeURIComponent(peerId)}/credit`,
      {
        amount_millisats: amountMillisats,
      },
    );
  }

  getMeshSelf(): Promise<MeshSelf> {
    return this.get("/api/v1/mesh/self");
  }

  /** Queried live, never cached (offers can flip between calls); 503 when mesh is off. */
  getMeshPeerCapabilities(peerId: string): Promise<MeshPeerCapabilities> {
    return this.get(
      `/api/v1/mesh/peers/${encodeURIComponent(peerId)}/capabilities`,
    );
  }

  /** Read-only; `MeshSettlementStatus` explains why there is no setter. */
  getMeshSettlementStatus(): Promise<MeshSettlementStatus> {
    return this.get("/api/v1/mesh/settlement");
  }

  // ── Schedules ─────────────────────────────────────────────

  listSchedules(): Promise<Schedule[]> {
    return this.get<Array<Record<string, unknown>>>("/api/v1/schedules").then(
      (items) =>
        (Array.isArray(items) ? items : []).map((t) => {
          // Extract prompt from kind.prompt or legacy payload.prompt
          const kind = t.kind as Record<string, unknown> | undefined;
          const payload = t.payload as Record<string, unknown> | undefined;
          const prompt =
            (kind?.prompt as string) ?? (payload?.prompt as string) ?? "";
          return {
            id: t.id as string,
            name: (t.label ?? t.name ?? "") as string,
            cron: t.cron as string,
            fire_at: t.fire_at as string | null | undefined,
            prompt,
            enabled:
              t.paused !== undefined
                ? !(t.paused as boolean)
                : ((t.enabled as boolean) ?? true),
            timezone: (t.timezone as string) ?? "UTC",
            kind: t.kind as Schedule["kind"],
            last_run: t.last_run as string | undefined,
            next_run: t.next_run as string | undefined,
            created_at: t.created_at as string | undefined,
          };
        }),
    );
  }

  createSchedule(
    body: Omit<Schedule, "id" | "created_at"> & {
      /** Fire once at `cron`'s next occurrence instead of recurring. */
      once?: boolean;
    },
  ): Promise<Schedule> {
    return this.post<Record<string, unknown>>("/api/v1/schedules", {
      name: body.name,
      cron: body.cron,
      prompt: body.prompt,
      timezone: body.timezone ?? "UTC",
      once: body.once ?? false,
    }).then((t) => {
      const kind = t.kind as Record<string, unknown> | undefined;
      const prompt = (kind?.prompt as string) ?? body.prompt;
      return {
        id: t.id as string,
        name: (t.label ?? t.name ?? body.name) as string,
        cron: t.cron as string,
        fire_at: t.fire_at as string | null | undefined,
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
    patch: {
      name?: string;
      cron?: string;
      prompt?: string;
      timezone?: string;
      once?: boolean;
    },
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
      fire_at: t.fire_at as string | null | undefined,
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
    return this.get<ScheduleRun[]>(
      `/api/v1/schedules/${encodeURIComponent(id)}/runs?limit=${limit}`,
    );
  }

  getUpcomingSchedules(limit = 10): Promise<Schedule[]> {
    return this.get<Schedule[]>(`/api/v1/schedules/upcoming?limit=${limit}`);
  }

  /** Fetch recent runs across all schedules, merged and sorted by start time. */
  async getAllRecentRuns(
    perScheduleLimit = 5,
  ): Promise<Array<ScheduleRun & { schedule_name: string }>> {
    const schedules = await this.listSchedules();
    const runSets = await Promise.all(
      schedules.map(async (s) => {
        try {
          const runs = await this.getScheduleRuns(s.id, perScheduleLimit);
          return runs.map((r) => ({
            ...r,
            schedule_name: s.name || s.label || s.id,
          }));
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

  async callTool(
    name: string,
    args: Record<string, unknown>,
  ): Promise<unknown> {
    return this.post<unknown>("/api/v1/mcp/tools/call", {
      name,
      arguments: args,
    });
  }

  /** Runs an MCP tool directly, bypassing the LLM (the Hub actuates devices without a chat turn). */
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
  async *streamConsolidation(): AsyncGenerator<
    import("./types").ConsolidationEvent
  > {
    const res = await fetch(`${this.base}/api/v1/memory/consolidate`, {
      method: "POST",
      headers: this.headers("POST"),
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
        } catch {
          /* skip malformed */
        }
      }
    }
  }

  stopConsolidation(): Promise<void> {
    return this.post("/api/v1/memory/consolidate/stop", {});
  }

  // ── Semantic index ────────────────────────────────────────

  /** No index or embedder answers `indexed: false`, not an error; counts are then absent, not 0. */
  getContextIndexHealth(): Promise<ContextIndexHealth> {
    return this.get<ContextIndexHealth>("/api/v1/context/index/health");
  }

  /**
   * Empties the index for the sweep to re-embed (the fix after an embedder, width or prefix change).
   * Answers once cleared, not when re-embedded: health reads near zero and climbs afterwards.
   */
  rebuildContextIndex(): Promise<ContextIndexRebuild> {
    return this.post<ContextIndexRebuild>("/api/v1/context/index/rebuild", {});
  }

  // ── Time and place ────────────────────────────────────────

  /** Every IANA zone with today's offset. Public: the wizard needs it. */
  listTimeZones(): Promise<{ zones: ZoneChoice[] }> {
    return this.get<{ zones: ZoneChoice[] }>("/api/v1/time/zones");
  }

  /** Locates the pond, cheapest source first; server-side so onboarding and Settings share it. */
  detectLocation(hints: {
    system_zone?: string;
    typed_name?: string;
    latitude?: number;
    longitude?: number;
  }): Promise<DetectedPlace> {
    return this.post<DetectedPlace>("/api/v1/location/detect", hints);
  }

  // ── Connected accounts ────────────────────────────────────

  /** Sources this session's speaker may see; scoped server-side, never filtered here. */
  listContextSources(sessionId: string): Promise<{ sources: ContextSource[] }> {
    return this.get(
      `/api/v1/context/sources?session_id=${encodeURIComponent(sessionId)}`,
    );
  }

  /** Sends no owner: the server derives it from session and device; a client-set owner is a hole. */
  connectContextSource(input: {
    kind: string;
    provider: string;
    sessionId: string;
    credentials?: { username: string; password: string; baseUrl?: string };
  }): Promise<{ id: string; kind: string; profile_id: string }> {
    return this.post("/api/v1/context/sources", {
      kind: input.kind,
      provider: input.provider,
      session_id: input.sessionId,
      ...(input.credentials
        ? {
            credentials: {
              username: input.credentials.username,
              password: input.credentials.password,
              ...(input.credentials.baseUrl
                ? { base_url: input.credentials.baseUrl }
                : {}),
            },
          }
        : {}),
    });
  }

  /** Items read from connected sources, newest first. Keyword search: people recall exact words. */
  listContextItems(
    sessionId: string,
    query?: string,
    limit = 200,
  ): Promise<{ items: ContextItem[] }> {
    const q = query?.trim() ? `&q=${encodeURIComponent(query.trim())}` : "";
    return this.get(
      `/api/v1/context/items?session_id=${encodeURIComponent(sessionId)}&limit=${limit}${q}`,
    );
  }

  /** Correct a memory's wording, keeping its identity. */
  updateMemory(id: string, content: string): Promise<void> {
    return this.put(`/api/v1/memories/${encodeURIComponent(id)}`, { content });
  }

  /** Syncs all accounts now rather than at the half-hourly sweep, reporting what the pass did. */
  syncContextSources(): Promise<AccountSyncSummary> {
    // Several round trips to third-party servers; the 30s default would fail a pass still running.
    return this.post("/api/v1/context/sync", {}, 120_000);
  }

  /** Disconnect a source. Its items go with it, and the reply says how many. */
  disconnectContextSource(
    id: string,
    sessionId: string,
  ): Promise<{ removed: number }> {
    return this.del(
      `/api/v1/context/sources/${encodeURIComponent(id)}?session_id=${encodeURIComponent(sessionId)}`,
    );
  }

  // ── Conversation titles ───────────────────────────────────

  /** Retitles now; slow (one model call per rename before it answers). Hand-typed names are kept. */
  retitleSessions(): Promise<RetitleResult> {
    return this.post("/api/v1/sessions/retitle", {});
  }

  /** Unlike the sweep, replaces even a fitting or hand-typed name: asking is consent. */
  retitleSession(sessionId: string): Promise<RetitleOneResult> {
    return this.post(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/retitle`,
      {},
    );
  }

  // ── Skills ────────────────────────────────────────────────

  listSkills(all = false): Promise<UserSkill[]> {
    return this.get(`/api/v1/skills${all ? "?all=true" : ""}`);
  }

  addSkill(
    name: string,
    description: string,
    content: string,
    icon = "sparkles",
  ): Promise<UserSkill> {
    return this.post("/api/v1/skills", { name, description, content, icon });
  }

  toggleSkill(id: string, currentEnabled: boolean): Promise<UserSkill> {
    return this.put(`/api/v1/skills/${id}`, { active: !currentEnabled });
  }

  updateSkill(
    id: string,
    patch: {
      name?: string;
      description?: string;
      content?: string;
      icon?: string;
    },
  ): Promise<UserSkill> {
    return this.put(`/api/v1/skills/${id}`, patch);
  }

  removeSkill(id: string): Promise<void> {
    return this.del(`/api/v1/skills/${id}`);
  }

  // ── Auth / Handshake ─────────────────────────────────────────

  /** Token-less, bypassing `request()`, so refresh and pairing cannot recurse. */
  private async handshakeFetch<T>(
    method: string,
    path: string,
    body?: unknown,
  ): Promise<T> {
    const res = await fetch(`${this.base}${path}`, {
      method,
      headers: { "Content-Type": "application/json" },
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (!res.ok) throw new ApiError(res.status, res.statusText);
    return res.json() as Promise<T>;
  }

  /** HMAC-SHA256(pairingCode, challenge ‖ clientId) → lowercase hex (Web Crypto). */
  private async computeMac(
    code: string,
    challengeB64: string,
    clientId: string,
  ): Promise<string> {
    const enc = new TextEncoder();
    const challenge = Uint8Array.from(atob(challengeB64), (c) =>
      c.charCodeAt(0),
    );
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

  /** Auto-pairs via the loopback-only pairing-code endpoint (same machine), storing the tokens. */
  async pair(clientId?: string): Promise<HandshakeResponse> {
    clientId = clientId ?? this.clientId();
    // If the startup code (10 min) lapsed, mint one; both endpoints are loopback-only.
    let pc = await this.handshakeFetch<PairingCodeResponse>(
      "GET",
      "/api/v1/handshake/pairing-code",
    );
    if (!pc.code) {
      pc = await this.handshakeFetch<PairingCodeResponse>(
        "POST",
        "/api/v1/handshake/pairing-code",
      );
    }
    if (!pc.code) {
      throw new ApiError(
        409,
        "could not obtain a pairing code from the local server",
      );
    }
    const init = await this.handshakeFetch<ChallengeResponse>(
      "POST",
      "/api/v1/handshake/init",
      {
        client_id: clientId,
        client_type: "desktop",
        client_version: "1.0.0",
      },
    );
    const mac = await this.computeMac(pc.code, init.challenge, clientId);
    const res = await this.handshakeFetch<HandshakeResponse>(
      "POST",
      "/api/v1/handshake/verify",
      {
        challenge_id: init.challenge_id,
        mac,
        device_name: "Pond Desktop",
      },
    );
    if (res.accepted && res.session_token) {
      this.refreshToken = res.refresh_token ?? null;
      this.setToken(res.session_token, res.expires_at ?? null); // persists all three
    }
    return res;
  }

  /**
   * Re-auth after a rejected token, coalesced: concurrent 401s share one re-pair. The token is
   * cleared inside the shared body, so a late caller cannot null a freshly obtained one.
   */
  private reauthenticate(clientId?: string): Promise<string | null> {
    if (this.reauthPromise) return this.reauthPromise;
    this.reauthPromise = (async () => {
      this.setToken(null); // force connect() past its "still-valid token" path
      return this.connect(clientId); // connect() defaults to the per-instance id
    })().finally(() => {
      this.reauthPromise = null;
    });
    return this.reauthPromise;
  }

  /** Valid stored token, else a refresh, else a fresh pair. Resolves to the session token or null. */
  async connect(clientId?: string): Promise<string | null> {
    clientId = clientId ?? this.clientId();
    if (
      this.token &&
      this.tokenExpiresAt &&
      Date.now() < this.tokenExpiresAt - 60_000
    ) {
      return this.token;
    }
    // A refresh token lasts 30 days and needs no pairing code.
    if (this.refreshToken) {
      try {
        const r = await this.handshakeFetch<HandshakeResponse>(
          "POST",
          "/api/v1/handshake/refresh",
          {
            refresh_token: this.refreshToken,
          },
        );
        if (r.accepted && r.session_token) {
          this.refreshToken = r.refresh_token ?? this.refreshToken;
          this.setToken(r.session_token, r.expires_at ?? null);
          return r.session_token;
        }
      } catch {
        /* fall through to a fresh pair */
      }
    }
    const res = await this.pair(clientId);
    return res.accepted ? res.session_token : null;
  }

  // ── Pairing ───────────────────────────────────────────────

  /** Return the current unexpired pairing code, or null if none is active. Loopback-only. */
  getPairingCode(): Promise<PairingCodeResponse> {
    return this.handshakeFetch<PairingCodeResponse>(
      "GET",
      "/api/v1/handshake/pairing-code",
    );
  }

  /** Issue a fresh pairing code, replacing any existing one. Loopback-only. */
  issuePairingCode(): Promise<PairingCodeResponse> {
    return this.handshakeFetch<PairingCodeResponse>(
      "POST",
      "/api/v1/handshake/pairing-code",
    );
  }

  // ── Models ────────────────────────────────────────────────

  listModels(): Promise<ModelEntry[]> {
    // The backend groups entries by category: { gguf: [...], llamafile: [...], tts: [...], ... }.
    return this.get<ModelEntry[] | Record<string, unknown[]>>(
      "/api/v1/models",
    ).then((r) => {
      if (Array.isArray(r)) return r;
      const entries: ModelEntry[] = [];
      for (const [category, items] of Object.entries(r)) {
        for (const item of items as Record<string, unknown>[]) {
          entries.push({
            id: `${category}/${item.name as string}`,
            provider: category,
            name: item.name as string,
            // Kokoro descriptions are sentences, so title from the id (af_heart -> Af_Heart).
            display_name:
              ((item.category as string | undefined) ?? category) ===
              "tts_kokoro"
                ? voiceTitle(item.name as string)
                : ((item.description as string | undefined) ??
                  (item.name as string)),
            is_active: (item.active as boolean | undefined) ?? false,
            ram_estimate_mb: item.ram_estimate_mb as number | undefined,
            recommended_role: item.recommended_role as string | undefined,
            context_length:
              (item.context_length as number | null | undefined) ?? undefined,
            quantization:
              (item.quantization as string | null | undefined) ?? undefined,
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

  /** Applies saved voice settings to the running engine (built at boot), fetching missing voices. */
  applyTtsSettings(patch?: {
    voice?: string;
    speed?: number;
    quality?: string;
  }): Promise<{
    voice: string;
    speed: number;
    quality: string;
    downloaded_voice: boolean;
    downloaded_weights: boolean;
    engine_reloaded: boolean;
    installed_voices: string[];
  }> {
    return this.post("/api/v1/voice/tts/apply", patch ?? {});
  }

  getModelCapabilities(): Promise<import("./types").ModelCapabilities> {
    return this.get("/api/v1/models/capabilities");
  }

  getMemoryStatus(): Promise<ModelMemoryStatus> {
    return this.get("/api/v1/models/memory-status");
  }

  /** Prefix warm-up status — is the pond ready for a first message yet. */
  getWarmupStatus(): Promise<import("./types").WarmupStatus> {
    return this.get("/api/v1/warmup");
  }

  getActiveRoles(): Promise<ModelActiveRoles> {
    // ASR/TTS roles may come as { model_id: "provider/name" } instead of { provider, model }.
    return this.get<Record<string, unknown>>(
      "/api/v1/models/active-roles",
    ).then((raw) => {
      function normalize(
        r: unknown,
      ): { provider: string; model: string } | null {
        if (!r || typeof r !== "object") return null;
        const obj = r as Record<string, unknown>;
        if (obj.provider && obj.model)
          return {
            provider: obj.provider as string,
            model: obj.model as string,
          };
        if (typeof obj.model_id === "string" && obj.model_id.includes("/")) {
          const [provider, ...rest] = obj.model_id.split("/");
          return { provider, model: rest.join("/") };
        }
        return null;
      }
      return {
        chat: normalize(raw.chat),
        tool: raw.tool ?? null,
        asr: normalize(raw.asr),
        tts: normalize(raw.tts),
        embedding: normalize(raw.embedding),
      } as ModelActiveRoles;
    });
  }

  activateModel(provider: string, name: string, role: string): Promise<void> {
    return this.post(
      `/api/v1/models/${encodeURIComponent(provider)}/${encodeURIComponent(name)}/activate`,
      { role },
    );
  }

  // ── Suggestions (proactive proposals) ─────────────────────
  // Session id is required, never defaulted: a default resolves to the whole household.

  listProposals(sessionId: string): Promise<ProposalList> {
    return this.get<ProposalList>(
      `/api/v1/proposals?session_id=${encodeURIComponent(sessionId)}`,
    );
  }

  decideProposal(
    id: string,
    sessionId: string,
    decision: ProposalDecision,
  ): Promise<unknown> {
    return this.post(`/api/v1/proposals/${encodeURIComponent(id)}/decide`, {
      session_id: sessionId,
      decision,
    });
  }

  // ── Sessions ──────────────────────────────────────────────

  listSessions(): Promise<SessionSummary[]> {
    return this.get<{ sessions: SessionSummary[] } | SessionSummary[]>(
      "/api/v1/sessions",
    ).then((r) =>
      Array.isArray(r)
        ? r
        : ((r as { sessions: SessionSummary[] }).sessions ?? []),
    );
  }

  /** `limit` without `offset` returns the N most recent messages, not the oldest page. */
  getSessionMessages(
    sessionId: string,
    limit?: number,
  ): Promise<SessionMessage[]> {
    const qs =
      limit != null ? `?limit=${encodeURIComponent(String(limit))}` : "";
    return this.get<
      | { messages: Array<Record<string, unknown>> }
      | Array<Record<string, unknown>>
    >(`/api/v1/sessions/${encodeURIComponent(sessionId)}/messages${qs}`).then(
      (r) => {
        const raw = Array.isArray(r)
          ? r
          : ((r as { messages: Array<Record<string, unknown>> }).messages ??
            []);
        return raw.map((m): SessionMessage => ({
          id: m.id as string,
          session_id: m.session_id as string,
          role: m.role as SessionMessage["role"],
          content: (m.content as string) ?? "",
          created_at: m.created_at as string,
          tool_calls: m.tool_calls as SessionMessageToolCall[] | undefined,
          tool_call_id: m.tool_call_id as string | undefined,
          images: m.images as SessionMessage["images"],
          liked: m.liked as boolean | null | undefined,
        }));
      },
    );
  }

  renameSession(sessionId: string, title: string): Promise<void> {
    return this.patch(`/api/v1/sessions/${encodeURIComponent(sessionId)}`, {
      title,
    });
  }

  deleteSession(sessionId: string): Promise<void> {
    return this.del(`/api/v1/sessions/${encodeURIComponent(sessionId)}`);
  }

  /** Refusals (e.g. "cooling_down") resolve normally and are common: render the reason. */
  compactSession(sessionId: string): Promise<CompactionReport> {
    return this.post(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/compact`,
    );
  }

  /** Deletes this message and all later ones: the primitive behind "edit" and "refresh". */
  deleteMessagesFrom(sessionId: string, messageId: string): Promise<void> {
    return this.del(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/messages/${encodeURIComponent(messageId)}`,
    );
  }

  /** Like (`true`), dislike (`false`) or clear (`null`) a message's training feedback. */
  setMessageFeedback(
    sessionId: string,
    messageId: string,
    liked: boolean | null,
  ): Promise<void> {
    return this.put(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/messages/${encodeURIComponent(messageId)}/feedback`,
      { liked },
    );
  }

  // ── Prompts ───────────────────────────────────────────────

  listPrompts(): Promise<PromptTemplate[]> {
    return this.get("/api/v1/prompts");
  }

  getPrompt(name: string): Promise<PromptTemplate> {
    return this.get(`/api/v1/prompts/${name}`);
  }

  /** Omitting `description` keeps the stored one; pass it only to change it. */
  updatePrompt(
    name: string,
    content: string,
    description?: string,
  ): Promise<PromptTemplate> {
    return this.put(`/api/v1/prompts/${name}`, {
      content,
      ...(description === undefined ? {} : { description }),
    });
  }

  resetPrompt(name: string): Promise<PromptTemplate> {
    return this.post(`/api/v1/prompts/${name}/reset`);
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

  updateRecipe(
    id: string,
    patch: { description?: string; yaml?: string; active?: boolean },
  ): Promise<AgentRecipe> {
    return this.put(`/api/v1/recipes/${encodeURIComponent(id)}`, patch);
  }

  removeRecipe(id: string): Promise<void> {
    return this.del(`/api/v1/recipes/${encodeURIComponent(id)}`);
  }

  // ── Chat streaming ────────────────────────────────────────

  async *chatStream(
    message: string,
    sessionId?: string,
    token?: string,
    canvasMode?: boolean,
    images?: ImageAttachment[],
    resumable?: boolean,
  ): AsyncGenerator<ChatEvent> {
    const reqBody: ChatStreamRequest = {
      message,
      session_id: sessionId,
      canvas_mode: canvasMode ?? false,
      // Only when non-empty, so text-only bodies stay unchanged.
      ...(images && images.length > 0 ? { images } : {}),
      // Only when asked, so other bodies stay unchanged.
      ...(resumable ? { resumable: true } : {}),
    };
    yield* this.streamSse("/api/v1/chat/stream", reqBody, token);
  }

  /** Live run to reattach to after a reload; `null` if none, as after a server restart. */
  async getActiveRun(sessionId: string): Promise<ActiveRun | null> {
    try {
      return await this.request<ActiveRun>(
        "GET",
        `/api/v1/sessions/${encodeURIComponent(sessionId)}/active-run`,
      );
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return null;
      throw e;
    }
  }

  /** Replays from `afterSeq` then follows; with `epoch`, a restarted server answers 410, not 404. */
  async *reattachRun(
    runId: string,
    afterSeq: number,
    epoch?: string,
    token?: string,
  ): AsyncGenerator<ChatEvent> {
    const q = new URLSearchParams({ after_seq: String(afterSeq) });
    if (epoch) q.set("epoch", epoch);
    yield* this.streamSse(
      `/api/v1/chat/runs/${encodeURIComponent(runId)}/events?${q}`,
      undefined,
      token,
      { method: "GET" },
    );
  }

  /** The only way to stop a run: hanging up does not. */
  async cancelRun(runId: string): Promise<void> {
    await this.request(
      "POST",
      `/api/v1/chat/runs/${encodeURIComponent(runId)}/cancel`,
    );
  }

  /** Stop whatever run this session is driving, without knowing its id. */
  async cancelSessionRun(sessionId: string): Promise<void> {
    await this.request(
      "DELETE",
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/active-run`,
    );
  }

  /** Absolute URL for a persisted chat-image attachment (see SessionMessageImage.url). */
  sessionAttachmentUrl(sessionId: string, attachmentId: string): string {
    return `${this.base}/api/v1/sessions/${encodeURIComponent(sessionId)}/attachments/${encodeURIComponent(attachmentId)}`;
  }

  /** Runs a recipe server-side, streaming the same SSE events as `chatStream`. */
  async *runRecipe(
    name: string,
    opts?: {
      sessionId?: string;
      voiceMode?: boolean;
      canvasMode?: boolean;
      token?: string;
      parameters?: Record<string, string>;
    },
  ): AsyncGenerator<ChatEvent> {
    const reqBody = {
      session_id: opts?.sessionId,
      voice_mode: opts?.voiceMode ?? false,
      canvas_mode: opts?.canvasMode ?? false,
      ...(opts?.parameters ? { parameters: opts.parameters } : {}),
    };
    yield* this.streamSse(
      `/api/v1/recipes/${encodeURIComponent(name)}/run`,
      reqBody,
      opts?.token,
    );
  }

  /** POSTs JSON to an SSE endpoint and yields each parsed event. */
  private async *streamSse(
    path: string,
    body: unknown,
    token?: string,
    opts?: { method?: "GET" | "POST" },
  ): AsyncGenerator<ChatEvent> {
    const method = opts?.method ?? "POST";
    await this.ensureTokenFresh();
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
    };
    const tok = token ?? this.token;
    if (tok) headers["Authorization"] = `Bearer ${tok}`;

    // Retry initial connection on network-level failures (not HTTP errors).
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 120_000);
    let res!: Response;
    for (let attempt = 0; attempt <= 2; attempt++) {
      try {
        res = await fetch(`${this.base}${path}`, {
          method,
          headers,
          ...(method === "POST" ? { body: JSON.stringify(body) } : {}),
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

    // A 401 must not surface as a chat error: re-auth once (coalesced) and reconnect.
    if (res.status === 401) {
      const fresh = await this.reauthenticate();
      if (fresh) {
        headers["Authorization"] = `Bearer ${fresh}`;
        const retryController = new AbortController();
        const retryTimeout = setTimeout(() => retryController.abort(), 120_000);
        try {
          res = await fetch(`${this.base}${path}`, {
            method,
            headers,
            ...(method === "POST" ? { body: JSON.stringify(body) } : {}),
            signal: retryController.signal,
          });
        } finally {
          clearTimeout(retryTimeout);
        }
      }
    }

    if (!res.ok) {
      let msg = res.statusText;
      try {
        msg = (await res.json()).error ?? msg;
      } catch {
        /* ignore */
      }
      throw new ApiError(res.status, msg);
    }

    const reader = res.body!.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    // The frame sequence rides in the SSE `id:` field, not the JSON; reattach resumes from it.
    let lastSeq: number | undefined;

    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() ?? "";

        for (const line of lines) {
          const trimmed = line.trim();
          if (trimmed.startsWith("id: ")) {
            const parsed = Number(trimmed.slice(4));
            if (Number.isFinite(parsed)) lastSeq = parsed;
            continue;
          }
          if (!trimmed || trimmed === "data: [DONE]") {
            if (trimmed === "data: [DONE]") yield { type: "done", done: true };
            continue;
          }
          const data = trimmed.startsWith("data: ")
            ? trimmed.slice(6)
            : trimmed;
          try {
            const event = JSON.parse(data) as ChatEvent;
            yield lastSeq === undefined ? event : { ...event, seq: lastSeq };
          } catch {
            // Malformed SSE line — skip
          }
        }
      }
    } finally {
      // Abandoning the generator must cancel the body: releaseLock() alone keeps the server's SSE
      // permit (one of four) held. cancel() after EOF is a no-op.
      try {
        await reader.cancel();
      } catch {
        // Already closed, or errored on the way down: nothing left to release.
      }
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
    return this.get(
      `/api/v1/models/search/gguf/files?repo=${encodeURIComponent(repo)}`,
    );
  }

  downloadModelFromUrl(
    url: string,
    category: string,
    filename: string,
  ): Promise<{ status: string }> {
    return this.post("/api/v1/models/download/url", {
      url,
      category,
      filename,
    });
  }

  /** Pause keeps the partial file (resume re-requests with a range header); cancel deletes it. */
  controlDownload(
    filename: string,
    action: "pause" | "resume" | "cancel",
  ): Promise<{ status: string }> {
    return this.post("/api/v1/models/download/control", { filename, action });
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
    return this.del(
      `/api/v1/models/${encodeURIComponent(category)}/${encodeURIComponent(name)}`,
    );
  }

  // Trigger async download of a catalog model by category and name
  downloadModel(category: string, name: string): Promise<{ status: string }> {
    return this.post(
      `/api/v1/models/${encodeURIComponent(category)}/${encodeURIComponent(name)}/download`,
    );
  }

  // ── Ollama ────────────────────────────────────────────────

  listOllamaModels(): Promise<{ models: OllamaModel[]; error?: string }> {
    return this.get("/api/v1/models/ollama");
  }

  pullOllamaModel(model: string): Promise<{ status: string }> {
    return this.post("/api/v1/models/ollama/pull", { model });
  }

  // ── Face Recognition (auto-managed; status only) ──────────
  // pond-server downloads these models on first boot when built with `--features face-onnx`.
  listFaceModels(): Promise<FaceModelsResponse> {
    return this.get("/api/v1/faces/models");
  }

  // ── Face enrollment / identification (Faces section) ──────
  // Face calls are multipart (postMultipart); without `face-onnx` the server answers 503.

  listProfiles(): Promise<{
    profiles: Array<{ id: string; display_name: string; avatar_emoji: string }>;
  }> {
    return this.get("/api/v1/profiles");
  }

  /** A member's stored preferences, in the server's own snake_case spelling. */
  async getProfilePrefs(profileId: string): Promise<Record<string, string>> {
    const p = await this.get<{ preferences?: Record<string, string> }>(
      `/api/v1/profiles/${encodeURIComponent(profileId)}`,
    );
    return p.preferences ?? {};
  }

  createProfile(
    displayName: string,
    avatarEmoji?: string,
  ): Promise<{
    id: string;
    display_name: string;
    avatar_emoji: string;
    preferences: Record<string, string>;
  }> {
    return this.post("/api/v1/profiles", {
      display_name: displayName,
      ...(avatarEmoji ? { avatar_emoji: avatarEmoji } : {}),
    });
  }

  /**
   * Keys must be the snake_case names `particulars_for` (routes.rs) reads, from `PROFILE_PREF_KEYS`:
   * a camelCase key returns 200 yet never reaches the model.
   */
  updateProfilePrefs(
    profileId: string,
    preferences: Record<string, string>,
  ): Promise<{
    id: string;
    display_name: string;
    preferences: Record<string, string>;
  }> {
    return this.patch(`/api/v1/profiles/${encodeURIComponent(profileId)}`, {
      preferences,
    });
  }

  async registerFace(
    profileId: string,
    frame: Blob,
  ): Promise<{
    id: string;
    profile_id: string;
    model_dims: number;
    created_at: string;
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
    enrollments: Array<{
      id: string;
      profile_id: string;
      model_dims: number;
      created_at: string;
    }>;
    count: number;
  }> {
    return this.get(`/api/v1/faces/profile/${encodeURIComponent(profileId)}`);
  }

  deleteUserBiometrics(
    profileId: string,
  ): Promise<{ profile_id: string; face_embeddings_deleted: number }> {
    return this.del(
      `/api/v1/users/${encodeURIComponent(profileId)}/biometrics`,
    );
  }

  private async postMultipart<T>(path: string, form: FormData): Promise<T> {
    const headers: Record<string, string> = {};
    if (this.token) headers["Authorization"] = `Bearer ${this.token}`;
    const res = await fetch(`${this.base}${path}`, {
      method: "POST",
      headers,
      body: form,
    });
    if (!res.ok) {
      let message = `HTTP ${res.status}`;
      try {
        message = (await res.text()) || message;
      } catch {
        /* ignore */
      }
      throw new ApiError(res.status, message);
    }
    return res.json() as Promise<T>;
  }

  // ── Llamafile GitHub releases ─────────────────────────────

  searchLlamafileModels(q?: string): Promise<{ models: LlamafileRelease[] }> {
    const qs = q ? `?q=${encodeURIComponent(q)}` : "";
    return this.get(`/api/v1/models/search/llamafile${qs}`);
  }

  // ── Extensions ───────────────────────────────────────────

  listExtensions(): Promise<{ extensions: Extension[] }> {
    return this.get("/api/v1/extensions");
  }

  toggleExtension(name: string, enabled: boolean): Promise<void> {
    return this.patch(`/api/v1/extensions/${encodeURIComponent(name)}`, {
      enabled,
    });
  }

  addExtension(req: AddExtensionRequest): Promise<Extension> {
    return this.post("/api/v1/extensions", req);
  }

  removeExtension(name: string): Promise<void> {
    return this.del(`/api/v1/extensions/${encodeURIComponent(name)}`);
  }

  // ── Marketplace ─────────────────────────────────────────

  async listMarketplace(): Promise<MarketplaceExtension[]> {
    const res = await this.get<{ extensions: MarketplaceExtension[] }>(
      "/api/v1/marketplace",
    );
    return res.extensions;
  }

  async installMarketplaceExtension(
    id: string,
    secrets?: Record<string, string>,
  ): Promise<Extension> {
    const body = secrets ? { secrets } : undefined;
    return this.post<Extension>(
      `/api/v1/marketplace/${encodeURIComponent(id)}/install`,
      body,
    );
  }

  async getExtensionSecrets(
    name: string,
  ): Promise<{
    requirements: SecretRequirement[];
    fulfilled: Record<string, boolean>;
  }> {
    return this.get(`/api/v1/extensions/${encodeURIComponent(name)}/secrets`);
  }

  /**
   * Stores credentials and restarts the extension. `restarted: false` with no error means nothing to
   * restart (not installed, or disabled); a `restart_error` means stored but not running: surface it.
   */
  async setExtensionSecrets(
    name: string,
    secrets: Record<string, string>,
  ): Promise<{ stored: number; restarted: boolean; restart_error: string | null }> {
    return await this.post<{ stored: number; restarted: boolean; restart_error: string | null }>(
      `/api/v1/extensions/${encodeURIComponent(name)}/secrets`,
      secrets,
    );
  }

  // ── Secrets ──────────────────────────────────────────────

  async listSecretKeys(): Promise<string[]> {
    const res = await this.get<{ keys: string[] }>("/api/v1/secrets");
    return res.keys;
  }

  async checkSecret(key: string): Promise<boolean> {
    const res = await this.get<{ exists: boolean }>(
      `/api/v1/secrets/${encodeURIComponent(key)}/exists`,
    );
    return res.exists;
  }

  async setSecret(key: string, value: string): Promise<void> {
    await this.put(`/api/v1/secrets/${encodeURIComponent(key)}`, { value });
  }

  async deleteSecret(key: string): Promise<void> {
    await this.del(`/api/v1/secrets/${encodeURIComponent(key)}`);
  }

  // ── Activity ──────────────────────────────────────────────

  listActivity(
    params?: import("./types").ActivityQueryParams,
  ): Promise<import("./types").ActivityResponse> {
    const qs = new URLSearchParams();
    if (params?.limit !== undefined) qs.set("limit", String(params.limit));
    if (params?.since) qs.set("since", params.since);
    if (params?.category) qs.set("category", params.category);
    if (params?.session_id) qs.set("session_id", params.session_id);
    const q = qs.toString();
    return this.get(`/api/v1/activity${q ? `?${q}` : ""}`);
  }

  getActivitySummary(
    window?: "hour" | "day" | "week",
  ): Promise<import("./types").ActivitySummary> {
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

  async transcribe(
    wav: ArrayBuffer,
    token?: string,
  ): Promise<TranscribeResponse> {
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
      try {
        msg = (await res.json()).error ?? msg;
      } catch {
        /* ignore */
      }
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
      try {
        msg = (await res.json()).error ?? msg;
      } catch {
        /* ignore */
      }
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
  async initiateOAuth(
    provider: string,
    extensionId?: string,
  ): Promise<{ auth_url: string; state: string }> {
    return this.post("/api/v1/oauth/authorize", {
      provider,
      extension_id: extensionId,
    });
  }

  /** `unknown`: this server never issued `state` (it restarted) or the outcome aged out. */
  async getOAuthStatus(
    state: string,
  ): Promise<import("./types").OAuthFlowStatus> {
    return this.get(`/api/v1/oauth/status/${encodeURIComponent(state)}`);
  }

  async refreshOAuth(provider: string): Promise<void> {
    await this.post("/api/v1/oauth/refresh", { provider });
  }

  async listOAuthProviders(): Promise<
    { id: string; display_name: string; scopes: string[] }[]
  > {
    const res = await this.get<{
      providers: { id: string; display_name: string; scopes: string[] }[];
    }>("/api/v1/oauth/providers");
    return res.providers;
  }
}

export const api = new PondApiClient();
