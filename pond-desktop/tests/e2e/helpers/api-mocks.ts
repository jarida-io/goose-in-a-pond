import type { Page } from "@playwright/test";

/** Build a text/event-stream body from JSON event payloads. */
export function mockSseStream(events: Array<Record<string, unknown>>): string {
  return `${events.map((ev) => `data: ${JSON.stringify(ev)}`).join("\n\n")}\n\n`;
}

/** Mocks every pond-api route so no pond-server is needed; call it in each test's beforeEach. */
export async function mockAllApiRoutes(page: Page): Promise<void> {
  // Pin a cross-origin API base: same-origin would hit Vite's SPA fallback, which answers any
  // unmocked /api/* path with index.html and breaks res.json().
  await page.addInitScript(() => {
    (window as unknown as { __GIAP_SERVER_URL__?: string }).__GIAP_SERVER_URL__ =
      "http://127.0.0.1:4000";
  });

  await page.route("**/api/v1/health", (route) =>
    route.fulfill({ json: { status: "ok", version: "test" } }),
  );

  await page.route("**/api/v1/handshake", (route) =>
    route.fulfill({ json: { token: "e2e-test-token", session_id: "e2e-session" } }),
  );

  // Direct tool invoke; holds switch state so the card's re-read reflects its write.
  const power = new Map<string, boolean>();
  await page.route("**/api/v1/tools/invoke", (route) => {
    const body = route.request().postDataJSON() as {
      server?: string;
      tool?: string;
      args?: { device_id?: string; power?: boolean };
    };
    const id = body.args?.device_id ?? "";
    if (body.tool === "set_device_state" && typeof body.args?.power === "boolean") {
      power.set(id, body.args.power);
    }
    const content =
      body.tool === "get_device_state" || body.tool === "set_device_state"
        ? `device: ${id}\npower: ${power.get(id) ? "on" : "off"}`
        : "ok";
    return route.fulfill({
      json: { tool: `${body.server ?? ""}__${body.tool ?? ""}`, success: true, content },
    });
  });

  await page.route("**/api/v1/onboard/status", (route) =>
    route.fulfill({ json: { onboarded: true, current_step: "Completed", steps_completed: 10, total_steps: 10 } }),
  );
  await page.route("**/api/v1/onboard/complete", (route) =>
    route.fulfill({ json: { status: "completed" } }),
  );
  // Echoes the reached step back in status shape.
  await page.route("**/api/v1/onboard/step/*", (route) => {
    const name = route.request().url().split("/").pop() ?? "Welcome";
    return route.fulfill({ json: { onboarded: false, current_step: name, steps_completed: 1, total_steps: 10 } });
  });
  // Reset onboarding ("Start over") — returns to the first step.
  await page.route("**/api/v1/onboard/reset", (route) =>
    route.fulfill({ json: { onboarded: false, current_step: "Welcome", steps_completed: 1, total_steps: 10 } }),
  );

  await page.route("**/api/v1/settings", (route) => {
    if (route.request().method() === "PUT") {
      return route.fulfill({ json: { assistant_name: "Pond", user_name: "Jerry", chat_provider: "llamafile", chat_model: "llama3.2", agent_memory_inject: false, prompt_style: "balanced", llm_temperature: 0.7, llm_max_tokens: 1024 } });
    }
    return route.fulfill({ json: { assistant_name: "Pond", user_name: "Jerry", chat_provider: "llamafile", chat_model: "llama3.2", agent_memory_inject: false, prompt_style: "balanced", llm_temperature: 0.7, llm_max_tokens: 1024 } });
  });

  await page.route("**/api/v1/schedules", (route) => {
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON() as Record<string, string>;
      return route.fulfill({
        status: 201,
        json: { id: "new-sched", name: body.name ?? "New Schedule", cron: body.cron ?? "0 0 8 * * *", prompt: body.prompt ?? "", enabled: true },
      });
    }
    return route.fulfill({ json: [] });
  });
  await page.route("**/api/v1/schedules/**", (route) =>
    route.fulfill({ json: { status: "ok" } }),
  );

  // Specs override this with populated sessions.
  await page.route("**/api/v1/sessions", (route) =>
    route.fulfill({ json: { sessions: [] } }),
  );
  await page.route("**/api/v1/sessions/*/messages", (route) =>
    route.fulfill({ json: { messages: [] } }),
  );
  // Manual compaction: defaults to the common "not under pressure" refusal. Registered before
  // the `/sessions/*` catch-all (last-registered wins; the catch-all falls through for it).
  await page.route("**/api/v1/sessions/*/compact", (route) =>
    route.fulfill({
      json: {
        session_id: "e2e-session",
        status: "skipped",
        reason: "not_under_pressure",
        outcome: null,
        context: {
          utilization_pct: 12.0,
          turns_remaining: null,
          avg_growth_rate: 0,
          should_compact: false,
          warning: null,
        },
      },
    }),
  );
  // Session rename/delete. Also matches /messages and /compact, so those fall through.
  await page.route("**/api/v1/sessions/*", (route) => {
    const url = route.request().url();
    if (url.includes("/messages") || url.includes("/compact")) return route.fallback();
    const method = route.request().method();
    if (method === "PATCH") {
      const body = (route.request().postDataJSON() ?? {}) as Record<string, unknown>;
      return route.fulfill({ json: { session_id: "mock", title: body.title ?? "" } });
    }
    if (method === "DELETE") {
      return route.fulfill({ status: 204, body: "" });
    }
    return route.fulfill({ json: {} });
  });

  // The catch-all goes first: Playwright tries the last-registered matching route first, so a
  // specific models route registered before it is never reached.
  await page.route("**/api/v1/models/**", (route) =>
    route.fulfill({ json: { status: "ok" } }),
  );
  await page.route("**/api/v1/models", (route) =>
    route.fulfill({
      json: { whisper: [], llamafile: [], tts: [], gguf: [], litert: [], ollama: [], embedding: [] },
    }),
  );
  await page.route("**/api/v1/models/active-roles", (route) =>
    route.fulfill({
      json: {
        chat: { provider: "llamafile", model: "llama3.2" },
        tool: { model: null },
        asr: { model_id: null },
        tts: { model_id: null },
        embedding: { model_id: null, model: "", provider: "fastembed" },
        router_name: "llamafile",
      },
    }),
  );
  await page.route("**/api/v1/models/memory-status", (route) =>
    route.fulfill({
      json: { total_mb: 8192, available_for_llm_mb: 4096, loaded_model: null, reclaimable_mb: 0 },
    }),
  );
  await page.route("**/api/v1/models/download/progress", (route) =>
    route.fulfill({ json: { downloads: [] } }),
  );
  await page.route("**/api/v1/models/ollama", (route) =>
    route.fulfill({ json: { models: [] } }),
  );
  await page.route("**/api/v1/models/disk-usage", (route) =>
    route.fulfill({
      json: { total_bytes: 0, by_category: {}, hf_cache_bytes: 0, incomplete_bytes: 0 },
    }),
  );
  await page.route("**/api/v1/models/vision-status", (route) =>
    route.fulfill({
      json: { model: "", state: { kind: "unknown" }, size_bytes: null, message: null },
    }),
  );
  // Mirrors WarmupStatus::default(); unmocked, it hits the network and fails console checks.
  await page.route("**/api/v1/warmup", (route) =>
    route.fulfill({
      json: {
        state: "skipped",
        reason: "not yet run",
        model: "",
        started_unix_ms: 0,
        finished_unix_ms: null,
        elapsed_ms: 0,
      },
    }),
  );

  // Asked by both chat composers on mount; unmocked it reaches the network and fails console checks.
  await page.route("**/api/v1/long-tasks/deep-research", (route) =>
    route.fulfill({ json: { available: true } }),
  );

  // Asked on mount by Home, Recipes and the agents hub; unmocked they fail console checks too.
  await page.route("**/api/v1/agents/prefills", (route) =>
    route.fulfill({
      json: {
        building: { active: false, current: null, done: 0, total: 0 },
        retention_days: 30,
        agents: [],
      },
    }),
  );
  await page.route("**/api/v1/agents/suggestions", (route) => route.fulfill({ json: [] }));

  await page.route("**/api/v1/devices", (route) =>
    route.fulfill({
      json: {
        devices: [
          {
            id: "driveway",
            name: "Driveway Light",
            device_type: "light",
            is_online: true,
            room: "Outdoor",
            capabilities: ["power"],
          },
        ],
      },
    }),
  );
  // Read by the Devices tab on mount; off is a fresh Pond's default.
  await page.route("**/api/v1/matter/status", (route) =>
    route.fulfill({ json: { enabled: false, url: "ws://127.0.0.1:5580/ws", state: "disabled" } }),
  );

  // The hub polls now-playing; unmocked, it breaks unrelated clean-console assertions.
  await page.route("**/api/v1/music/now-playing", (route) =>
    route.fulfill({ json: { playing: false, track: null } }),
  );
  await page.route("**/api/v1/music/**", (route) => route.fulfill({ json: { status: "ok" } }));

  // Mesh: no peers, disabled by default in tests.
  await page.route("**/api/v1/mesh/peers", (route) => {
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON() as {
        peer_id?: string;
        trust_scope?: string;
      };
      return route.fulfill({
        json: { peer_id: body.peer_id ?? "", trust_scope: body.trust_scope ?? "circle" },
      });
    }
    return route.fulfill({ json: { peers: [] } });
  });
  await page.route("**/api/v1/mesh/peers/*", (route) =>
    route.fulfill({ status: 204, body: "" }),
  );
  await page.route("**/api/v1/mesh/peers/*/capabilities", (route) =>
    route.fulfill({
      json: { peer_id: "", inference_available: false, lightning_available: false },
    }),
  );
  await page.route("**/api/v1/mesh/self", (route) =>
    route.fulfill({ json: { mesh_enabled: false } }),
  );
  await page.route("**/api/v1/mesh/settlement", (route) =>
    route.fulfill({
      json: { configured: false, millisats_per_token: 0, peers: [] },
    }),
  );

  // Weather — no location configured in tests, dashboard falls back to mock data.
  await page.route("**/api/v1/weather", (route) =>
    route.fulfill({ json: { enabled: false } }),
  );

  await page.route("**/api/v1/memories", (route) =>
    route.fulfill({ json: [] }),
  );

  // Home's left column (unmocked, its error path looks just like the quiet state). Proposals
  // stay empty: none has been produced on real hardware.
  await page.route("**/api/v1/proposals*", (route) =>
    route.fulfill({ json: { profile_id: "p-jerry", proposals: [] } }),
  );

  // Two real engine shapes, each `because` with a measured number, so the offers half renders.
  await page.route("**/api/v1/suggestions*", (route) =>
    route.fulfill({
      json: {
        suggestions: [
          {
            id: "memory_recall",
            prompt: "What do you remember about me?",
            because: "12 things remembered the pond can still reach.",
            answered_by: "giap-memory",
          },
          {
            id: "devices_online",
            prompt: "Which of my devices are online?",
            because: "3 devices registered here.",
            answered_by: "giap-device",
          },
        ],
        considered: [
          { id: "calendar_today", silent_because: "no calendar account is connected" },
          { id: "memory_recall", silent_because: null },
          { id: "devices_online", silent_because: null },
        ],
        audience: "personal",
      },
    }),
  );

  // The lane: Settings reads it on mount, and an unmocked route hits 127.0.0.1:4000. Shaped like
  // a real pond (jobs absent, never run, blocked) so the `present` distinction stays tested.
  await page.route("**/api/v1/lane", (route) =>
    route.fulfill({
      json: {
        lane: true,
        slot_busy: false,
        idle_for_secs: 240,
        saw_activity_since_start: true,
        would_run: "titling",
        idle_reason: null,
        jobs: [
          { job: "consolidation", title: "Tidy the memory store", present: true, registered: true, enabled: true, since_last_run_secs: 3600, interval_floor_secs: 86400, idle_threshold_secs: 900, blocked_by: "interval_floor", would_run_next: false },
          { job: "titling", title: "Name conversations", present: true, registered: true, enabled: true, since_last_run_secs: 300, interval_floor_secs: 300, idle_threshold_secs: 900, blocked_by: null, would_run_next: true },
          { job: "proactive_review", title: "Look for something to suggest", present: true, registered: false, enabled: false, since_last_run_secs: null, interval_floor_secs: 0, idle_threshold_secs: 0, blocked_by: null, would_run_next: false },
          { job: "summary_refresh", title: "Refresh conversation summaries", present: true, registered: true, enabled: true, since_last_run_secs: 120, interval_floor_secs: 30, idle_threshold_secs: 120, blocked_by: "still_active", would_run_next: false },
          { job: "index_maintenance", title: "Maintain the search index", present: false, registered: false, enabled: false, since_last_run_secs: null, interval_floor_secs: 0, idle_threshold_secs: 0, blocked_by: null, would_run_next: false },
          { job: "memory_extraction", title: "Read conversations for memories", present: true, registered: true, enabled: true, since_last_run_secs: null, interval_floor_secs: 60, idle_threshold_secs: 900, blocked_by: "still_active", would_run_next: false },
        ],
      },
    }),
  );
  await page.route("**/api/v1/lane/jobs/*/run", (route) =>
    route.fulfill({ json: { lane: true, job: "titling", woken: true } }),
  );

  // The Connections screens read these on open: the contract's example, Wi-Fi joined and a cable plugged in.
  await page.route("**/api/v1/connections", (route) =>
    route.fulfill({
      json: {
        hostname: "nano",
        https_port: 4443,
        wifi: {
          state: "connected",
          ssid: "Home",
          address: "192.168.1.11",
          signal: 70,
          capabilities: {
            scan: { available: true, reason: null },
            join: { available: true, reason: null },
            forget: { available: true, reason: null },
          },
        },
        ethernet: [{ interface: "eth0", link: "up", address: "192.168.1.20", speed_mbps: 1000 }],
        bluetooth: { enabled: true, state: "advertising", reason: null, advertising_as: "Pond 4e81", session_open: false },
        can_change: { allowed: true, reason: null },
      },
    }),
  );
  await page.route("**/api/v1/connections/wifi/networks", (route) =>
    route.fulfill({ json: { networks: [] } }),
  );
  await page.route("**/api/v1/setup/status", (route) =>
    route.fulfill({ json: { id: "4e81", claimed: true, claimed_at: "2026-10-18T12:06:12Z", claimed_by_this_device: false } }),
  );
  await page.route("**/api/v1/system/info", (route) =>
    route.fulfill({
      json: {
        hostname: "nano", lan_address: "192.168.1.11", tailnet_address: null, https_port: 4443,
        tls_spki_sha256: null, port: 4000, version: "test", platform: "linux", arch: "aarch64",
      },
    }),
  );

  await page.route("**/api/v1/skills", (route) =>
    route.fulfill({ json: [] }),
  );

  await page.route("**/api/v1/prompts", (route) =>
    route.fulfill({ json: [] }),
  );
  await page.route("**/api/v1/prompts/**", (route) =>
    route.fulfill({ json: { name: "balanced", content: "You are a helpful assistant.", is_system: true } }),
  );

  await page.route("**/api/v1/agent/extras", (route) =>
    route.fulfill({ json: [] }),
  );
  await page.route("**/api/v1/agent/tools", (route) =>
    route.fulfill({ json: [] }),
  );

  await page.route("**/api/v1/recipes", (route) =>
    route.fulfill({
      json: [
        {
          id: "recipe-morning",
          name: "Morning Check",
          description: "Read the briefing, report the weather",
          yaml: "title: Morning Check\ninstructions: read the briefing\n",
          title: "Morning Check",
          active: true,
        },
      ],
    }),
  );

  // Warmup: opening Chat requests it, and an unmocked call trips the strict console gate.
  await page.route("**/api/v1/warmup", (route) =>
    route.fulfill({ json: { warmed: false, reason: "no model in the test environment" } }),
  );

  await page.route("**/api/v1/transcribe", (route) =>
    route.fulfill({ json: { text: "hello from transcription" } }),
  );

  await page.route("**/api/v1/chat/stream", (route) =>
    route.fulfill({
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
      body: [
        'data: {"type":"text","content":"Hello! I am Pond.","token":"Hello! I am Pond."}',
        'data: {"done":true,"session_id":"e2e-session","model_role":"chat"}',
        "",
      ].join("\n"),
    }),
  );

  await page.route("**/api/v1/tts", (route) =>
    route.fulfill({ status: 503, json: { error: "TTS not configured in tests" } }),
  );

  await page.route("**/api/v1/profiles", (route) =>
    route.fulfill({ json: [] }),
  );

  await page.route("**/api/v1/extensions", (route) => {
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({
        status: 201,
        json: {
          name: body.name ?? "test-ext",
          kind: body.kind ?? "stdio",
          enabled: true,
          tools: [],
          description: null,
          status: "connected",
          last_error: null,
        },
      });
    }
    return route.fulfill({ json: { extensions: [] } });
  });
  await page.route("**/api/v1/extensions/**", (route) => {
    if (route.request().method() === "DELETE") {
      return route.fulfill({ status: 204, body: "" });
    }
    return route.fulfill({ json: { status: "ok" } });
  });

  await page.route("**/api/v1/marketplace", (route) =>
    route.fulfill({
      json: {
        extensions: [
          {
            id: "weather-tools",
            name: "Weather Tools",
            description: "Real-time weather data and forecasts for any location worldwide.",
            kind: "streamable_http",
            uri: "http://localhost:3010/mcp",
            category: "productivity",
            author: "Pond Team",
            tools: ["get_weather", "get_forecast", "get_alerts"],
            featured: true,
            required_secrets: [],
          },
          {
            id: "git-helper",
            name: "Git Helper",
            description: "Git operations: commit, diff, log, branch management from the agent.",
            kind: "stdio",
            command: "git-mcp",
            args: [],
            category: "development",
            author: "Community",
            tools: ["git_status", "git_diff", "git_commit"],
            featured: false,
            required_secrets: [],
          },
          {
            id: "github-mcp",
            name: "GitHub",
            description: "Access GitHub repos, issues, and pull requests with your personal access token.",
            kind: "stdio",
            command: "github-mcp",
            args: [],
            category: "development",
            author: "Pond Team",
            tools: ["list_repos", "get_issue", "create_pr"],
            featured: false,
            required_secrets: [
              {
                key: "GITHUB_PERSONAL_ACCESS_TOKEN",
                display_name: "GitHub Personal Access Token",
                description: "A classic token with repo scope. Generate one at github.com/settings/tokens.",
                required: true,
                kind: "api_key",
              },
            ],
          },
          {
            id: "spotify-mcp",
            name: "Spotify",
            description: "Control Spotify playback, search tracks, and manage playlists.",
            kind: "streamable_http",
            uri: "http://localhost:3011/mcp",
            category: "entertainment",
            author: "Community",
            tools: ["play_track", "search_music", "get_playlist"],
            featured: false,
            required_secrets: [
              {
                key: "SPOTIFY_ACCESS_TOKEN",
                display_name: "Spotify",
                description: "Sign in with your Spotify account to allow the agent to control playback.",
                required: true,
                kind: "oauth_flow",
              },
            ],
          },
        ],
      },
    }),
  );
  await page.route("**/api/v1/marketplace/*/install", (route) =>
    route.fulfill({
      status: 201,
      json: {
        name: "GitHub",
        kind: "stdio",
        enabled: true,
        tools: ["list_repos", "get_issue", "create_pr"],
        description: "Access GitHub repos, issues, and pull requests.",
        status: "connected",
        last_error: null,
      },
    }),
  );

  await page.route("**/api/v1/secrets/*/exists", (route) =>
    route.fulfill({ json: { exists: false } }),
  );
  await page.route("**/api/v1/secrets/**", (route) => {
    if (route.request().method() === "PUT") {
      return route.fulfill({ status: 204, body: "" });
    }
    return route.fulfill({ json: { keys: [] } });
  });
  // The bare collection path the Tools tab reads on mount; `secrets/**` doesn't match it.
  await page.route("**/api/v1/secrets", (route) =>
    route.fulfill({ json: { keys: [] } }),
  );

  // Web search starts Off; a spec that exercises it registers its own, later, routes.
  await page.route("**/api/v1/web-search", (route) =>
    route.fulfill({ json: { provider: "brave", configured: false, status: "not_configured" } }),
  );

  await page.route("**/api/v1/extensions/*/secrets", (route) => {
    if (route.request().method() === "POST") {
      // The documented response shape; a 204 would test the undefined-body path users never hit.
      return route.fulfill({ json: { stored: 1, restarted: false, restart_error: null } });
    }
    return route.fulfill({ json: { requirements: [], fulfilled: {} } });
  });

  await page.route("**/api/v1/logs", (route) =>
    route.fulfill({
      json: [
        { id: "1", timestamp: new Date().toISOString(), level: "INFO", source: "pond-server", message: "Server bound to 127.0.0.1:4000", metadata: null },
        { id: "2", timestamp: new Date().toISOString(), level: "INFO", source: "models", message: "Loaded gemma-4-E4B-it-Q4_K_M (5363 MB)", metadata: null },
        { id: "3", timestamp: new Date().toISOString(), level: "WARN", source: "memory", message: "Available memory below 25% threshold (1.8 GB)", metadata: null },
      ],
    }),
  );

  await page.route("**/api/v1/usage", (route) =>
    route.fulfill({
      json: {
        total_prompt_tokens: 142000,
        total_completion_tokens: 38000,
        total_tokens: 180000,
        session_count: 3,
        cloud_input_price_per_m: 2.50,
        cloud_output_price_per_m: 10.00,
      },
    }),
  );

  // OAuth — catch-all first, then the specific route (last registered wins).
  await page.route("**/api/v1/oauth/**", (route) =>
    route.fulfill({ json: { auth_url: "https://accounts.spotify.com/authorize?test=1", state: "test-state" } }),
  );
  // The sign-in modal polls this for the flow's outcome.
  await page.route("**/api/v1/oauth/status/**", (route) =>
    route.fulfill({ json: { status: "completed" } }),
  );
}
