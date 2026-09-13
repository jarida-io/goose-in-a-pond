import type { Page } from "@playwright/test";

/** Build a text/event-stream body from JSON event payloads. */
export function mockSseStream(events: Array<Record<string, unknown>>): string {
  return `${events.map((ev) => `data: ${JSON.stringify(ev)}`).join("\n\n")}\n\n`;
}

/**
 * Intercept all pond-api calls and return sensible mock data.
 * This prevents tests from requiring a running pond-server.
 *
 * Call `await mockAllApiRoutes(page)` in each test's beforeEach.
 */
export async function mockAllApiRoutes(page: Page): Promise<void> {
  // Pin the API base to the conventional local server. In a real browser the
  // app now defaults to window.location.origin (so the single-executable works
  // same-origin over the LAN); in tests that origin is the Vite dev server,
  // whose SPA fallback returns index.html for any UNMOCKED /api/* path, which
  // would make the app's res.json() throw. Pinning a distinct cross-origin base
  // keeps unmocked calls failing fast/gracefully, as they did before.
  await page.addInitScript(() => {
    (window as unknown as { __GIAP_SERVER_URL__?: string }).__GIAP_SERVER_URL__ =
      "http://127.0.0.1:4000";
  });

  // Health
  await page.route("**/api/v1/health", (route) =>
    route.fulfill({ json: { status: "ok", version: "test" } }),
  );

  // Handshake
  await page.route("**/api/v1/handshake", (route) =>
    route.fulfill({ json: { token: "e2e-test-token", session_id: "e2e-session" } }),
  );

  // Direct tool invoke (Hub device control bypasses the LLM)
  await page.route("**/api/v1/tools/invoke", (route) => {
    const body = route.request().postDataJSON() as { server?: string; tool?: string };
    return route.fulfill({
      json: { tool: `${body.server ?? ""}__${body.tool ?? ""}`, success: true, content: "ok" },
    });
  });

  // Onboarding
  await page.route("**/api/v1/onboard/status", (route) =>
    route.fulfill({ json: { onboarded: true, current_step: "Completed", steps_completed: 10, total_steps: 10 } }),
  );
  await page.route("**/api/v1/onboard/complete", (route) =>
    route.fulfill({ json: { status: "completed" } }),
  );
  // Per-step progress tracking (POST /onboard/step/:name) — echoes the reached
  // step back in status shape.
  await page.route("**/api/v1/onboard/step/*", (route) => {
    const name = route.request().url().split("/").pop() ?? "Welcome";
    return route.fulfill({ json: { onboarded: false, current_step: name, steps_completed: 1, total_steps: 10 } });
  });
  // Reset onboarding ("Start over") — returns to the first step.
  await page.route("**/api/v1/onboard/reset", (route) =>
    route.fulfill({ json: { onboarded: false, current_step: "Welcome", steps_completed: 1, total_steps: 10 } }),
  );

  // Settings
  await page.route("**/api/v1/settings", (route) => {
    if (route.request().method() === "PUT") {
      return route.fulfill({ json: { assistant_name: "Pond", user_name: "Jerry", chat_provider: "llamafile", chat_model: "llama3.2", agent_memory_inject: false, prompt_style: "balanced", llm_temperature: 0.7, llm_max_tokens: 1024 } });
    }
    return route.fulfill({ json: { assistant_name: "Pond", user_name: "Jerry", chat_provider: "llamafile", chat_model: "llama3.2", agent_memory_inject: false, prompt_style: "balanced", llm_temperature: 0.7, llm_max_tokens: 1024 } });
  });

  // Schedules
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

  // Sessions. The list carries `message_count` so the sidebar badge renders.
  // Individual tests override `**/api/v1/sessions` with populated data.
  await page.route("**/api/v1/sessions", (route) =>
    route.fulfill({ json: { sessions: [] } }),
  );
  await page.route("**/api/v1/sessions/*/messages", (route) =>
    route.fulfill({ json: { messages: [] } }),
  );
  // Manual compaction (PAI-4 P7b). Defaults to the refusal a real install hits
  // most often — the endpoint does not bypass the pressure axis's rate limiter,
  // so "not under pressure" is the ordinary answer. Registered before the
  // `/sessions/*` catch-all below so a spec can still override it (page.route
  // is last-registered-wins, and that catch-all falls through for this path).
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
  // Rename (PATCH) and delete (DELETE) on an individual session. This pattern
  // also matches `/sessions/:id/messages`, so fall through for those to let the
  // more specific messages route above handle them. Echoes the body for PATCH
  // and returns 204 for DELETE; specs that need stateful behaviour route these
  // themselves before calling into the app.
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

  // Models
  await page.route("**/api/v1/models/active-roles", (route) =>
    route.fulfill({ json: { chat: { provider: "llamafile", model: "llama3.2" }, think: {}, task: {}, asr: {}, tts: {}, router_name: "llamafile" } }),
  );
  await page.route("**/api/v1/models/memory-status", (route) =>
    route.fulfill({ json: { total_mb: 8192, available_for_llm_mb: 4096, loaded_model: null } }),
  );
  // Prefix warm-up. Unmocked, this is the one request that still reached a real
  // server during e2e — `ERR_CONNECTION_REFUSED` on 127.0.0.1:4000/api/v1/warmup
  // — which failed the console-error assertion in hub-visual-verify. Mirrors
  // `WarmupStatus::default()`: skipped, never run.
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
  await page.route("**/api/v1/models/download/progress", (route) =>
    route.fulfill({ json: { downloads: [] } }),
  );
  await page.route("**/api/v1/models/ollama", (route) =>
    route.fulfill({ json: { models: [] } }),
  );
  await page.route("**/api/v1/models", (route) =>
    route.fulfill({ json: { whisper: [], llamafile: [], tts: [], gguf: [], ollama: [], embedding: [] } }),
  );
  await page.route("**/api/v1/models/**", (route) =>
    route.fulfill({ json: { status: "ok" } }),
  );

  // Devices
  await page.route("**/api/v1/devices", (route) =>
    route.fulfill({ json: { devices: [] } }),
  );
  // The Devices tab reads this on mount for its Matter section. Off is the
  // default a fresh Pond is in.
  await page.route("**/api/v1/matter/status", (route) =>
    route.fulfill({ json: { enabled: false, url: "ws://127.0.0.1:5580/ws", state: "disabled" } }),
  );

  // Music. `now-playing` is POLLED by the hub, so leaving it unmocked does not
  // fail the music tests — it fails whichever unrelated test happens to assert
  // a clean console. The request leaves the browser for 127.0.0.1:4000 and hits
  // whatever is really listening: on a developer machine usually a live
  // pond-server (CORS errors), in CI nothing at all (connection refused).
  // That is what took out the hub state test.
  await page.route("**/api/v1/music/now-playing", (route) =>
    route.fulfill({ json: { playing: false, track: null } }),
  );
  await page.route("**/api/v1/music/**", (route) => route.fulfill({ json: { status: "ok" } }));

  // Mesh (#132) — no peers, mesh disabled by default in tests.
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

  // Memories
  await page.route("**/api/v1/memories", (route) =>
    route.fulfill({ json: [] }),
  );

  // Skills
  await page.route("**/api/v1/skills", (route) =>
    route.fulfill({ json: [] }),
  );

  // Prompts
  await page.route("**/api/v1/prompts", (route) =>
    route.fulfill({ json: [] }),
  );
  await page.route("**/api/v1/prompts/**", (route) =>
    route.fulfill({ json: { name: "balanced", content: "You are a helpful assistant.", is_system: true } }),
  );

  // Agent
  await page.route("**/api/v1/agent/extras", (route) =>
    route.fulfill({ json: [] }),
  );
  await page.route("**/api/v1/agent/tools", (route) =>
    route.fulfill({ json: [] }),
  );

  // Recipes
  await page.route("**/api/v1/recipes", (route) =>
    route.fulfill({ json: [] }),
  );

  // Transcribe (for voice pipeline)
  await page.route("**/api/v1/transcribe", (route) =>
    route.fulfill({ json: { text: "hello from transcription" } }),
  );

  // Chat stream
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

  // TTS
  await page.route("**/api/v1/tts", (route) =>
    route.fulfill({ status: 503, json: { error: "TTS not configured in tests" } }),
  );

  // Profiles
  await page.route("**/api/v1/profiles", (route) =>
    route.fulfill({ json: [] }),
  );

  // Extensions
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

  // Marketplace
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

  // Secrets
  await page.route("**/api/v1/secrets/*/exists", (route) =>
    route.fulfill({ json: { exists: false } }),
  );
  await page.route("**/api/v1/secrets/**", (route) => {
    if (route.request().method() === "PUT") {
      return route.fulfill({ status: 204, body: "" });
    }
    return route.fulfill({ json: { keys: [] } });
  });
  // The COLLECTION route. `secrets/**` above has a literal slash before the
  // wildcard, so it does not match the bare `/api/v1/secrets` the Tools tab
  // calls on mount (PAI-2 P2). Registered last on purpose: page.route is
  // last-registered-wins, and this pattern is the more specific of the two.
  await page.route("**/api/v1/secrets", (route) =>
    route.fulfill({ json: { keys: [] } }),
  );

  // Extensions secrets endpoint
  await page.route("**/api/v1/extensions/*/secrets", (route) => {
    if (route.request().method() === "POST") {
      return route.fulfill({ status: 204, body: "" });
    }
    return route.fulfill({ json: { requirements: [], fulfilled: {} } });
  });

  // Logs
  await page.route("**/api/v1/logs", (route) =>
    route.fulfill({
      json: [
        { id: "1", timestamp: new Date().toISOString(), level: "INFO", source: "pond-server", message: "Server bound to 127.0.0.1:4000", metadata: null },
        { id: "2", timestamp: new Date().toISOString(), level: "INFO", source: "models", message: "Loaded gemma-4-E4B-it-Q4_K_M (5363 MB)", metadata: null },
        { id: "3", timestamp: new Date().toISOString(), level: "WARN", source: "memory", message: "Available memory below 25% threshold (1.8 GB)", metadata: null },
      ],
    }),
  );

  // Usage
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
  // The sign-in modal polls this for the outcome of the flow it started; it no
  // longer infers success from the token key existing.
  await page.route("**/api/v1/oauth/status/**", (route) =>
    route.fulfill({ json: { status: "completed" } }),
  );
}
