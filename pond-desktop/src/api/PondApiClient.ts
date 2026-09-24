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
  type LaneRunResult,
  type LaneStatus,
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
  type SuggestionList,
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
    // Only an http(s) page origin is a server worth talking to. The desktop
    // shell serves the renderer from app://giap, which is deliberately not
    // http -- and it always injects the URL above anyway, so this branch is
    // the browser's.
    if (window.location?.origin?.startsWith("http")) {
      return window.location.origin;
    }
  }
  return "http://127.0.0.1:4000";
}

/** Commissioning budget, kept just above the server's own 180s pairing timeout
 *  so the server's error is what surfaces, not a client-side abort. */
const COMMISSION_TIMEOUT_MS = 190_000;

export class PondApiClient {
  /** Mutable: the shell can correct this when the sidecar binds a fallback port. */
  private base: string;
  private token: string | null;
  private refreshToken: string | null = null;
  private tokenExpiresAt: number | null = null;
  private refreshPromise: Promise<void> | null = null;
  // In-flight reactive re-authentication (after a 401). Shared so a burst of
  // concurrent 401s re-pairs once and reuses the resulting token, instead of
  // each request running its own handshake (a self-inflicted pairing storm).
  private reauthPromise: Promise<string | null> | null = null;

  private static readonly LS_SESSION = "giap-session-token";
  private static readonly LS_REFRESH = "giap-refresh-token";
  private static readonly LS_EXPIRES = "giap-token-expires-at";
  private static readonly LS_CLIENT_ID = "giap-client-id";

  /**
   * A per-install client id, stable across restarts (persisted) but distinct
   * between separate clients — a browser profile, a second machine, the Tauri
   * app. Pairing revokes any earlier session for the *same* client id, so a
   * shared hardcoded id made two clients ping-pong: each pair revoked the
   * other's token, forcing an endless re-pair. Distinct ids keep distinct
   * sessions, so they coexist. Tabs in one profile share the id (and the
   * persisted token), so they reuse one session rather than fighting.
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

  /**
   * Set the active session token. Pass `expiresAt` (RFC3339, the server's
   * `expires_at`) to (re)arm the proactive refresh timer; omit it to update
   * only the bearer token while preserving the known expiry (used by callers
   * that just need the header). Pass `null` to clear the expiry.
   */
  /**
   * Point the client at a different server.
   *
   * Every request builder reads `this.base` at call time, so one call here
   * redirects the whole singleton -- which is why the shell can correct a
   * fallback port without reloading the renderer or rebuilding the client.
   */
  setBase(url: string): void {
    this.base = url.replace(/\/$/, "");
  }

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
    // Use a token-less fetch (handshakeFetch) so this can't recurse back into
    // ensureTokenFresh via request().
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
    // Content-Type on a bodyless GET forces an unnecessary CORS preflight on
    // every read call — omit it there; POST/PUT/PATCH bodies still need it.
    const h: Record<string, string> =
      method === "GET"
        ? { ...extra }
        : { "Content-Type": "application/json", ...extra };
    if (this.token) h["Authorization"] = `Bearer ${this.token}`;
    return h;
  }

  /**
   * Send one authenticated request and hand back the response unread.
   *
   * The half every JSON call and every bytes call share: the proactive token
   * refresh, the timeout, one coalesced re-pair on a 401, and a non-2xx mapped
   * to `ApiError`. Split out of `request()` so a caller that wants bytes
   * rather than JSON inherits all four instead of copying three of them.
   */
  private async send(
    method: string,
    path: string,
    body?: unknown,
    timeout?: number,
    _retry = false,
  ): Promise<Response> {
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
        // The stored token was rejected (e.g. the server rotated it). Re-pair
        // once, coalesced, then retry — see reauthenticate().
        await this.reauthenticate();
        return this.send(method, path, body, timeout, true);
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
      return res;
    } catch (e) {
      clearTimeout(timeoutId);
      if (e instanceof DOMException && e.name === "AbortError") {
        throw new ApiError(408, "Request timed out");
      }
      throw e;
    }
  }

  private async request<T>(
    method: string,
    path: string,
    body?: unknown,
    timeout?: number,
  ): Promise<T> {
    const res = await this.send(method, path, body, timeout);
    // 204 No Content and any other empty body — return undefined cast to T
    const ct = res.headers.get("content-type") ?? "";
    if (res.status === 204 || !ct.includes("json"))
      return undefined as unknown as T;
    return res.json() as Promise<T>;
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
    /** LAN IPv4 a phone should use when it cannot resolve `<hostname>.local`. Null when the host has no LAN route. */
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

  /**
   * Record that the wizard has reached an onboarding step. `step` is a backend
   * `OnboardingStep` variant name (e.g. "Basics", "WakeWord"). Progress is
   * monotonic server-side, so re-reporting an earlier step is a safe no-op.
   */
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

  /**
   * Reset onboarding back to the first step ("Start over"). Clears persisted
   * progress and re-arms the onboarding guard so the wizard shows again.
   */
  resetOnboarding(): Promise<{
    onboarded: boolean;
    current_step: string;
    steps_completed: number;
    total_steps: number;
  }> {
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
   * PATCH the settings the caller actually changed, and return the full merged
   * result.
   *
   * Send ONLY changed keys. The server writes exactly the keys the request
   * carries and treats that key set as a record of user intent: each one is
   * marked `is_user_set`, which permanently exempts it from future
   * default-adoption migrations. A caller that PUTs a whole settings object
   * therefore marks every setting as deliberately chosen — even on a Save that
   * changed nothing — and reverts any field another surface (the Models tab,
   * the phone, wake-word calibration) has written since it loaded.
   *
   * See docs/developer/settings-defaults-and-user-intent.md. Callers that
   * batch edits behind a Save button should diff against the last loaded
   * snapshot; `diffSettings` in `settings/state.ts` does exactly that.
   *
   * The returned object is NOT byte-identical to the patch for a float field.
   * `Settings` holds these as `f32` and the API serialises through `f64`, so a
   * sent `0.7` comes back as 0.699999988079071. A caller that keeps a baseline
   * must fold in the patch it sent alongside this response, or the float looks
   * permanently edited and every later save re-sends it — which re-marks it and
   * defeats default adoption for that key. Do not paper over it by comparing
   * numbers with an epsilon or `Math.fround`: that also collapses distinct
   * integers above 2^24 and would drop real edits to the count fields.
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

  /** Commission a Matter device onto the fabric with its setup code, optionally
   *  naming it (written to the device and used as its GIAP name).
   *
   *  Given its own timeout: pairing involves discovery, attestation, and fabric
   *  join, for which the server allows 180s. On the default 30s a real
   *  commission aborted here as "Request timed out" while it went on to succeed
   *  on the Pond. */
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

  /** What the Matter integration is actually doing. Polled by the Devices tab
   *  while the controller starts up, which the settings save does not wait for. */
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

  // ── Mesh (#132) ─────────────────────────────────────────────

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

  /** Live "what does this peer offer right now" — queried over the mesh on
   * every call, not cached (a peer's Lightning wallet or backing model can
   * flip between two calls). 503s when mesh isn't enabled on this Pond. */
  getMeshPeerCapabilities(peerId: string): Promise<MeshPeerCapabilities> {
    return this.get(
      `/api/v1/mesh/peers/${encodeURIComponent(peerId)}/capabilities`,
    );
  }

  /** Read-only settlement-job status — see MeshSettlementStatus's own docs
   * on why there's no matching setter here. */
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

  /** Execute an MCP tool by name with arguments. Returns the tool result. */
  async callTool(
    name: string,
    args: Record<string, unknown>,
  ): Promise<unknown> {
    return this.post<unknown>("/api/v1/mcp/tools/call", {
      name,
      arguments: args,
    });
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

  /** Stop an in-progress consolidation. */
  stopConsolidation(): Promise<void> {
    return this.post("/api/v1/memory/consolidate/stop", {});
  }

  // ── Semantic index ────────────────────────────────────────

  /**
   * How much of what the pond knows retrieval can actually reach.
   *
   * Never rejects for a pond with no index or no embedder — it answers with
   * `indexed: false` and a reason, because switching embeddings off is a
   * working configuration rather than a fault. Branch on `indexed` before
   * reading any count: they are absent, not zero, in that answer.
   */
  getContextIndexHealth(): Promise<ContextIndexHealth> {
    return this.get<ContextIndexHealth>("/api/v1/context/index/health");
  }

  /**
   * Empty the index so the maintenance sweep embeds every row again.
   *
   * Answers when the table is cleared, NOT when the re-embed finishes: health
   * read straight afterwards is near zero and climbs in the background, so a
   * caller that refreshes immediately has to say so or it looks like the
   * rebuild broke something. Nothing is lost either way — every vector is
   * recomputable from the store it was derived from.
   *
   * This is the repair for the one-way doors: a changed embedder, width or task
   * prefix leaves rows that score plausibly and are wrong, and the sweep cannot
   * notice them on its own because it is driven by a vector being ABSENT.
   */
  rebuildContextIndex(): Promise<ContextIndexRebuild> {
    return this.post<ContextIndexRebuild>("/api/v1/context/index/rebuild", {});
  }

  /**
   * Record that a composed suggestion was tapped.
   *
   * Only composed ones: a template suggestion is recomputed on every read and
   * has no row to settle. Without this the queue never drains and a household
   * reads the same composed questions forever, which is the complaint the whole
   * surface was built from, one tier up.
   */
  markSuggestionTaken(id: string): Promise<{ id: string; settled: boolean }> {
    return this.post(`/api/v1/suggestions/${encodeURIComponent(id)}/taken`, {});
  }

  // ── The inference lane ────────────────────────────────────

  /** What every background job is doing and waiting for. */
  laneStatus(): Promise<LaneStatus> {
    return this.get<LaneStatus>("/api/v1/lane");
  }

  /**
   * Ask one background job to take its next tick now.
   *
   * Wakes rather than runs: the work happens in the job's own loop under the
   * same single slot every scheduled pass takes, so this returns as soon as the
   * doorbell has been rung. What happened is read back from `laneStatus`.
   */
  runLaneJob(job: string): Promise<LaneRunResult> {
    return this.post<LaneRunResult>(
      `/api/v1/lane/jobs/${encodeURIComponent(job)}/run`,
      {},
    );
  }

  // ── Time and place ────────────────────────────────────────

  /** Every IANA zone with today's offset. Public: the wizard needs it. */
  listTimeZones(): Promise<{ zones: ZoneChoice[] }> {
    return this.get<{ zones: ZoneChoice[] }>("/api/v1/time/zones");
  }

  /**
   * Work out where this pond is, from several sources, cheapest first.
   *
   * Server-side so onboarding and Settings run the SAME cascade — they used to
   * have one each, and neither produced usable coordinates.
   */
  detectLocation(hints: {
    system_zone?: string;
    typed_name?: string;
    latitude?: number;
    longitude?: number;
  }): Promise<DetectedPlace> {
    return this.post<DetectedPlace>("/api/v1/location/detect", hints);
  }

  // ── Connected accounts ────────────────────────────────────

  /**
   * The sources this session's speaker may see.
   *
   * Scoped on the server from the session, not filtered here: one member never
   * sees another's accounts, and that is decided where the rows are.
   */
  listContextSources(sessionId: string): Promise<{ sources: ContextSource[] }> {
    return this.get(
      `/api/v1/context/sources?session_id=${encodeURIComponent(sessionId)}`,
    );
  }

  /**
   * Connect an account.
   *
   * The owner is NOT sent: the server resolves it from the session and the
   * paired device this request arrived on. A caller-supplied owner would be a
   * hole, and every item the source ever produces inherits it.
   */
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

  /** What the pond has read from connected sources, newest first.
   *
   * Keyword search, not semantic: somebody scanning this list is looking for a
   * message they remember the words of, and a cosine ranking would bury an
   * exact title match under things merely about the same subject. */
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

  /**
   * Pull every connected account now, instead of waiting for the half-hourly
   * sweep. Answers with what the pass did, so a person who just typed in a
   * password learns whether it worked.
   */
  syncContextSources(): Promise<AccountSyncSummary> {
    // Longer than the default: a sync is several HTTP round trips to somebody
    // else's server, and timing out at 30s would report a failure for a pass
    // that was still going.
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

  /**
   * Ask the pond to rename conversations now rather than waiting for it to be
   * idle.
   *
   * Answers as soon as the titling job has been asked, not when it has
   * finished. It used to do the work inline — one model call per conversation,
   * up to twenty — which on a small board took minutes and so reliably tripped
   * the 30 s default timeout below while the work carried on invisibly. Names
   * typed by hand are never touched.
   */
  retitleSessions(): Promise<RetitleResult> {
    return this.post("/api/v1/sessions/retitle", {});
  }

  /**
   * Rename one named conversation, now.
   *
   * Obeys rather than protects: unlike the sweep, this replaces a name that
   * still fits and one typed by hand, because asking for a specific
   * conversation is consent about that conversation.
   */
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

  /**
   * Token-less fetch for the public handshake endpoints. Deliberately bypasses
   * `request()`/`ensureTokenFresh()` so refresh/pairing can't recurse.
   */
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

  /**
   * Auto-pair this desktop install with the local server. Because the desktop
   * and server share a machine, we read the pairing code off the loopback-only
   * endpoint and run the full two-phase handshake — no operator typing needed.
   * On success the session+refresh tokens are stored on this client.
   */
  async pair(clientId?: string): Promise<HandshakeResponse> {
    clientId = clientId ?? this.clientId();
    // Read the current code; if none is active (e.g. the startup code expired
    // after 10 min), ISSUE a fresh one. Both endpoints are loopback-only, so the
    // same-host desktop is trusted to mint its own code — this is what makes
    // silent auto-pair actually reliable instead of failing once the operator's
    // startup code lapses.
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
   * Establish an authenticated session, reusing a persisted token across app
   * restarts. Tries, in order: a still-valid stored session token → a refresh
   * with the stored refresh token (no pairing code needed) → a fresh pair
   * (needs the server's current pairing code). Returns the active session
   * token, or `null` if none could be established.
   */
  /**
   * Re-authenticate after a rejected token, coalescing concurrent callers.
   *
   * A stale token fails every in-flight request at once (page load fires
   * several), and without this each one would independently drop the token and
   * run a full handshake — the burst of pairings seen in the server log. Here
   * the first caller drops the rejected token and re-pairs; everyone else
   * awaits the same promise and picks up the one fresh token. The token is
   * cleared inside the shared body (once), so a late caller can't null a token
   * a concurrent re-pair just obtained.
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

  async connect(clientId?: string): Promise<string | null> {
    clientId = clientId ?? this.clientId();
    // 1. Stored session token still comfortably valid.
    if (
      this.token &&
      this.tokenExpiresAt &&
      Date.now() < this.tokenExpiresAt - 60_000
    ) {
      return this.token;
    }
    // 2. Refresh with a stored refresh token — survives restarts for 30 days
    //    without ever needing the pairing code again.
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
    // 3. Fresh pairing.
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
    // Backend returns { gguf: [...], llamafile: [...], tts: [...], whisper: [...] }
    // each entry has: name, category, active, ram_estimate_mb, recommended_role, description
    return this.get<ModelEntry[] | Record<string, unknown[]>>(
      "/api/v1/models",
    ).then((r) => {
      if (Array.isArray(r)) return r;
      // Flatten grouped object into ModelEntry[]
      const entries: ModelEntry[] = [];
      for (const [category, items] of Object.entries(r)) {
        for (const item of items as Record<string, unknown>[]) {
          entries.push({
            id: `${category}/${item.name as string}`,
            provider: category,
            name: item.name as string,
            // A Kokoro voice's description is a sentence, not a name, so it
            // must not become the row title the way an LLM's short description
            // legitimately does. Title from the id instead: `af_heart` →
            // `Af_Heart`, with the description left for the row's subtitle.
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

  /**
   * Bring the RUNNING speech engine in line with the saved voice settings,
   * fetching anything missing first.
   *
   * Without this, every voice change waited for the next restart — the engine
   * is built once at boot. Selecting a voice the household does not have is a
   * download here, not an error telling them to install it elsewhere.
   */
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

  /** What the batch memory-extraction engine is doing, and why it is not. */
  getExtractionStatus(): Promise<import("./types").ExtractionStatus> {
    return this.get("/api/v1/memories/extraction-status");
  }

  /** Prefix warm-up status — is the pond ready for a first message yet. */
  getWarmupStatus(): Promise<import("./types").WarmupStatus> {
    return this.get("/api/v1/warmup");
  }

  getActiveRoles(): Promise<ModelActiveRoles> {
    // Backend may return { model_id: "provider/name" } for ASR/TTS instead of { provider, model }.
    // Normalize all roles to { provider, model } | null.
    return this.get<Record<string, unknown>>(
      "/api/v1/models/active-roles",
    ).then((raw) => {
      function normalize(
        r: unknown,
      ): { provider: string; model: string } | null {
        if (!r || typeof r !== "object") return null;
        const obj = r as Record<string, unknown>;
        // Already has provider + model
        if (obj.provider && obj.model)
          return {
            provider: obj.provider as string,
            model: obj.model as string,
          };
        // Has model_id like "whisper/base.en" or "gguf/gemma-2b"
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
  //
  // Both routes require a session id and neither defaults it: the session is
  // how the server learns who is asking, and a defaulted one would resolve to
  // the whole household — so a suggestion meant for one person would be shown
  // to, and answerable by, anyone.

  listProposals(sessionId: string): Promise<ProposalList> {
    return this.get<ProposalList>(
      `/api/v1/proposals?session_id=${encodeURIComponent(sessionId)}`,
    );
  }

  /**
   * What the household might want to ask.
   *
   * `sessionId` is OPTIONAL, unlike every proposal call, and that is the point:
   * `state.sessionId` is null on a cold launch and never persisted, so a Home
   * screen that waited for one would show nothing on exactly the launch this
   * fills. Passing one when it exists only sharpens the audience.
   */
  listSuggestions(sessionId?: string | null): Promise<SuggestionList> {
    const q = sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : "";
    return this.get<SuggestionList>(`/api/v1/suggestions${q}`);
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

  /** `limit` with no `offset`: server returns the N most recent messages
   *  (newest-aware), not an old-first page — see get_session_messages.
   *
   *  The mapping copies fields one at a time, and a field it does not name is
   *  dropped without a sound: `thinking` was, for every reloaded conversation,
   *  while the replay tests stayed green because they mocked this method and
   *  so never ran it. The `satisfies` below makes a key of `SessionMessage`
   *  that this literal leaves out a type error rather than a lost field. */
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
        return raw.map(
          (m): SessionMessage =>
            ({
              id: m.id as string,
              session_id: m.session_id as string,
              role: m.role as SessionMessage["role"],
              content: (m.content as string) ?? "",
              created_at: m.created_at as string,
              tool_calls: m.tool_calls as SessionMessageToolCall[] | undefined,
              tool_call_id: m.tool_call_id as string | undefined,
              images: m.images as SessionMessage["images"],
              // Passed through as sent: absent stays absent and `[]` stays
              // `[]`, the distinction the type documents.
              thinking: m.thinking as SessionMessage["thinking"],
              liked: m.liked as boolean | null | undefined,
            }) satisfies Record<keyof SessionMessage, unknown>,
        );
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

  /**
   * PAI-4 P7. Ask the server to compact this session now.
   *
   * Everything short of a server fault answers 200 with a `status`/`reason`
   * pair, so a refusal ("cooling_down", "not_under_pressure", …) arrives here
   * as a normal resolved `CompactionReport` — `request()` only throws on
   * non-2xx. Callers must render the reason, not treat it as an error: the
   * endpoint deliberately does not bypass the pressure axis's rate limiter, so
   * being refused is the common case rather than the exceptional one.
   */
  compactSession(sessionId: string): Promise<CompactionReport> {
    return this.post(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/compact`,
    );
  }

  /** Delete a message and every later message in the same session — the
   *  primitive behind "edit" and "refresh" on a user message. */
  deleteMessagesFrom(sessionId: string, messageId: string): Promise<void> {
    return this.del(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/messages/${encodeURIComponent(messageId)}`,
    );
  }

  /** Set (`true`/`false`) or clear (`null`) the like/dislike training-feedback
   *  flag on one message. */
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

  /**
   * Omitting `description` preserves the stored one — the server treats an
   * absent field as "leave it alone" rather than "clear it". Pass it only when
   * the caller actually means to change it.
   */
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
    images?: ImageAttachment[],
    resumable?: boolean,
  ): AsyncGenerator<ChatEvent> {
    const reqBody: ChatStreamRequest = {
      message,
      session_id: sessionId,
      canvas_mode: canvasMode ?? false,
      // Only set when non-empty so text-only turns keep today's exact body.
      ...(images && images.length > 0 ? { images } : {}),
      // Only set when asked, so a turn that does not want to outlive its
      // connection sends exactly the body it always did.
      ...(resumable ? { resumable: true } : {}),
    };
    yield* this.streamSse("/api/v1/chat/stream", reqBody, token);
  }

  /**
   * The run driving this session, if the server is still driving one.
   *
   * The way back in after a reload: the app knows its session id and nothing
   * else, so this is what turns that into a run to reattach to. `null` when
   * there is none — which is also the honest answer after a server restart,
   * since the run died with the process.
   */
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

  /**
   * Follow a run already in flight, replaying from `afterSeq` first.
   *
   * `epoch` is what the run reported when it started. Sending it back is what
   * earns a 410 with a reason instead of a bare 404 when the server has
   * restarted underneath the client.
   */
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

  /** Stop a run on purpose — the only way, now that hanging up is not one. */
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

  /**
   * The bytes of one persisted chat-image attachment (see SessionMessageImage).
   *
   * Fetched, never handed to `<img src>`. The route sits on the protected
   * router and the server accepts only an `Authorization: Bearer` header, which
   * an image element cannot send, so a bare URL answers 401 on every pond
   * started without the loopback dev bypass. Callers show the result through
   * an object URL they own and revoke.
   */
  async getSessionAttachment(
    sessionId: string,
    attachmentId: string,
  ): Promise<Blob> {
    const res = await this.send(
      "GET",
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/attachments/${encodeURIComponent(attachmentId)}`,
    );
    return res.blob();
  }

  /**
   * Execute a recipe by name. Server resolves the recipe's prompt and streams
   * the agent's response using the same SSE event shape as `chatStream`.
   */
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

  /**
   * POST a JSON body to an SSE endpoint and yield each parsed event.
   * Shared by chatStream and runRecipe.
   */
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

    // A rejected token (the server rotated it, or an app-held token went stale)
    // must not surface as a chat error. Re-authenticate once — coalesced with
    // any concurrent 401s — and reconnect with the fresh token. Unlike request(),
    // this SSE path used to just throw, which is why a chat send could fail with
    // "Invalid or expired token" while background calls quietly re-paired. This
    // also stops an SSE reconnect from re-pairing every cycle.
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
    // The server puts a run's frame sequence in the SSE `id:` field rather than
    // inside the JSON, so no frame shape changed and no per-token parse was
    // added on a Jetson's hot path. It is what a reattach resumes from, so it
    // has to be read here rather than thrown away with the rest of the envelope.
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
      // Abandoning this generator must CLOSE the body, not merely let go of it.
      // `releaseLock()` alone leaves the response un-cancelled, so the socket
      // stays open; the server's SSE generator is never dropped, so its
      // `sse_semaphore` permit and its `AttachGuard` are both still held. There
      // are four permits. Measured: four turns abandoned mid-stream (four
      // conversation switches) and every later send gets
      // `503 Too many concurrent streams` in under 2 ms, until the browser
      // happens to garbage-collect the Response.
      //
      // `cancel()` on a body already read to EOF is a no-op, so the normal
      // completion path is unchanged.
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

  /**
   * Pause, resume or cancel a transfer in flight.
   *
   * Pause and cancel are the same stop, differing in what happens to the
   * partial file: pause leaves it so resuming continues from there, cancel
   * throws it away. Resume starts the same transfer again — the Hugging Face
   * cache finds the partial and re-requests with a range header.
   */
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
   * Store a household member's preferences on the SERVER.
   *
   * The keys are a contract with the prompt builder, which reads
   * `preferred_name`, `birthday`, `language` and
   * `accessibility_atypical_speech` out of `profiles.preferences`
   * (`routes.rs :: particulars_for`). They are snake_case there and camelCase
   * in this app's own draft types, and a camelCase key sent here returns 200,
   * populates the row, and reaches the model as nothing at all.
   *
   * That is not hypothetical: until 2026-08-12 the onboarding wizard collected
   * a preferred name and birthday and wrote them to browser `localStorage`,
   * while the server read them from SQLite. Every piece worked and the
   * capability did not exist. `PROFILE_PREF_KEYS` is the single spelling of
   * these names on this side.
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
   * Stores an extension's credentials and restarts it so the running process
   * picks them up.
   *
   * `restarted` is false with no `restart_error` when there was deliberately
   * nothing to restart — the extension is not installed, or is disabled.
   * A non-null `restart_error` means the credentials are stored but the
   * extension is not running, so callers must surface it.
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

  /**
   * How the flow with this `state` nonce ended.
   *
   * `pending` while the browser hand-off is in flight, then `completed` /
   * `failed`. `unknown` means the nonce was never issued by the running server
   * (it restarted) or its outcome aged out.
   */
  async getOAuthStatus(
    state: string,
  ): Promise<import("./types").OAuthFlowStatus> {
    return this.get(`/api/v1/oauth/status/${encodeURIComponent(state)}`);
  }

  /** Refresh an expired OAuth access token. */
  async refreshOAuth(provider: string): Promise<void> {
    await this.post("/api/v1/oauth/refresh", { provider });
  }

  /** List supported OAuth providers. */
  async listOAuthProviders(): Promise<
    { id: string; display_name: string; scopes: string[] }[]
  > {
    const res = await this.get<{
      providers: { id: string; display_name: string; scopes: string[] }[];
    }>("/api/v1/oauth/providers");
    return res.providers;
  }
}

// Singleton — the Tauri backend injects window.__GIAP_SERVER_URL__
export const api = new PondApiClient();
