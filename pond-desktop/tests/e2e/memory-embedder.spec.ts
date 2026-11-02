/** The Memory job in both UIs: EmbeddingGemma 2 offered, the switch watched, search by meaning on or off. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";
import { memoryHealth, memoryRows, mockModels, typicalModels, type ModelMocks } from "./helpers/model-mocks";

const ROLES = (embedding: Record<string, unknown>) => ({
  chat: { provider: "local", model: "gemma-4-E2B-it-qat-UD-Q4_K_XL", model_id: "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL" },
  tool: { model: null },
  asr: { model_id: null },
  tts: { model_id: null },
  embedding,
  router_name: "LLaMA_CPP",
});

const DEFAULT_MEMORY = { model_id: null, model: "", provider: "fastembed" };
const ON_EG2 = { model_id: "embedding/embeddinggemma-2", model: "embeddinggemma-2", provider: "auto" };

const FETCHING = "embeddinggemma-2-Q8_0.gguf is not on disk; the models pipeline is downloading it";
const EG2_DOWNLOAD = {
  filename: "embeddinggemma-2-Q8_0.gguf", category: "embedding", downloaded_bytes: 123_000_000,
  total_bytes: 309_855_456, status: "downloading", model_id: "embedding/embeddinggemma-2", part: "model", resumable: true,
};

interface Seen {
  activated: { url: string; body: unknown }[];
  rebuilt: number;
  saved: unknown[];
}

/** Registered before navigating. `seen` records what the screen asks the pond to do. */
async function setup(
  page: Page,
  opts: { section: string; route?: string; scheme?: "Light" | "Dark"; size?: [number, number] } & ModelMocks,
): Promise<Seen> {
  const seen: Seen = { activated: [], rebuilt: 0, saved: [] };
  await mockAllApiRoutes(page);
  await mockModels(page, {
    models: typicalModels({ embedding: memoryRows() }),
    roles: ROLES(DEFAULT_MEMORY),
    health: memoryHealth({ model_id: "all-MiniLM-L6-v2" }, { model_id: "all-MiniLM-L6-v2", dims: 384 }),
    ...opts,
  });
  await page.route("**/api/v1/models/embedding/*/activate", (route) => {
    seen.activated.push({ url: route.request().url(), body: route.request().postDataJSON() });
    return route.fulfill({ json: { role: "embedding", model_id: "embedding/embeddinggemma-2" } });
  });
  await page.route("**/api/v1/context/index/rebuild", (route) => {
    seen.rebuilt += 1;
    return route.fulfill({ json: { indexed: true, cleared: 0, corpora: [] } });
  });
  await page.route("**/api/v1/settings", (route) => {
    if (route.request().method() === "PUT") {
      seen.saved.push(route.request().postDataJSON());
      return route.fulfill({ json: { ...(route.request().postDataJSON() as object) } });
    }
    return route.fulfill({
      json: {
        assistant_name: "Pond", user_name: "Jerry", chat_provider: "local",
        chat_model: "gemma-4-E2B-it-qat-UD-Q4_K_XL", embedding_provider: "fastembed",
      },
    });
  });
  await page.addInitScript(({ scheme, section, route }) => {
    if (scheme) localStorage.setItem("goosehub_theme", scheme);
    localStorage.setItem("giap-section", section);
    if (section === "hub") {
      localStorage.setItem("giap-force-hub", "1");
      localStorage.setItem("goosehub_route", route ?? "home");
    }
  }, { scheme: opts.scheme, section: opts.section, route: opts.route });
  const [width, height] = opts.size ?? (opts.section === "hub" ? [1024, 600] : [1280, 900]);
  await page.setViewportSize({ width, height });
  return seen;
}

async function openClassicModels(page: Page) {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Jobs" })).toBeVisible({ timeout: 10_000 });
  return page.getByRole("region", { name: "Memory models" });
}

async function openHubModels(page: Page) {
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  await navigateTo(page, "Models");
  await expect(page.getByRole("heading", { name: "Models" })).toBeVisible({ timeout: 10_000 });
  return page.locator(".setcard", { has: page.locator(".setcard__title", { hasText: /^Memory$/ }) });
}

async function scan(page: Page, within: string, label: string) {
  const results = await new AxeBuilder({ page }).include(within).withTags(["wcag2a", "wcag2aa"]).analyze();
  const found = results.violations.flatMap((v) =>
    v.nodes.map((n) => `${v.id}: ${n.target.join(" ")} (${n.failureSummary?.split("\n")[1]?.trim()})`),
  );
  expect(found, label).toEqual([]);
}

test.describe("Memory, classic Models page", () => {
  test("offers EmbeddingGemma 2 first, with what it buys and its size, and leaves the default in use", async ({ page }) => {
    await setup(page, { section: "models" });
    const memory = await openClassicModels(page);
    const rows = memory.getByRole("listitem");
    await expect(rows).toHaveCount(3);
    await expect(rows.nth(0)).toContainText("EmbeddingGemma 2");
    await expect(rows.nth(0)).toContainText("Recommended");
    await expect(rows.nth(0)).toContainText("Finds facts in Swahili, Sheng or English, whichever language you ask in");
    await expect(rows.nth(0)).toContainText("295 MB to download");
    await expect(rows.nth(0).getByRole("button")).toHaveCount(1);
    await expect(rows.filter({ hasText: "all-MiniLM-L6-v2" })).toContainText("In use");
    // The Memory card names the model in a word or two, never its description.
    const card = page.locator(".mdl-role", { hasText: "Memory" });
    await expect(card.locator(".mdl-role__holder")).toHaveText("all-MiniLM-L6-v2");
    await expect(card).toContainText("Built-in default");
  });

  test("Use assigns the Memory role, then shows the download with the same controls every model has", async ({ page }) => {
    const seen = await setup(page, { section: "models" });
    const memory = await openClassicModels(page);
    // After the press the pond is fetching the file and says it failed to load only because it is not here yet.
    await page.route("**/api/v1/models/download/progress", (r) => r.fulfill({ json: { downloads: [EG2_DOWNLOAD] } }));
    await page.route("**/api/v1/context/index/health", (r) =>
      r.fulfill({ json: memoryHealth({ state: "failed", reason: FETCHING }, { matching: 0 }) }),
    );
    await page.route("**/api/v1/models/active-roles", (r) => r.fulfill({ json: ROLES(ON_EG2) }));

    await memory.getByRole("button", { name: "Use EmbeddingGemma 2 for memory" }).click();
    await expect.poll(() => seen.activated.length).toBe(1);
    expect(seen.activated[0].body).toEqual({ role: "embedding" });
    expect(seen.activated[0].url).toContain("/models/embedding/embeddinggemma-2/activate");
    await expect(
      page.getByText("Using EmbeddingGemma 2 for memory once it downloads (295 MB). Memory searches by keyword until then."),
    ).toBeVisible();

    await expect(memory.getByRole("progressbar", { name: /EmbeddingGemma 2.*model/i })).toBeVisible();
    await expect(memory.getByRole("button", { name: "Pause" })).toBeVisible();
    await expect(memory.getByRole("button", { name: "Stop" })).toBeVisible();
    await expect(memory.getByText("Searching by keyword until EmbeddingGemma 2 finishes downloading")).toBeVisible();
    await expect(memory.getByRole("alert")).toHaveCount(0);
  });

  test("keeps MiniLM selectable once EmbeddingGemma 2 is in use", async ({ page }) => {
    const seen = await setup(page, { section: "models", roles: ROLES(ON_EG2) });
    const memory = await openClassicModels(page);
    await expect(memory.getByRole("listitem").filter({ hasText: "EmbeddingGemma 2" })).toContainText("In use");
    await memory.getByRole("button", { name: "Use all-MiniLM-L6-v2 for memory" }).click();
    await expect.poll(() => seen.activated.length).toBe(1);
    expect(seen.activated[0].url).toContain("/models/embedding/all-MiniLM-L6-v2/activate");
  });

  test("says it searches by keyword while the model loads, and counts the re-index", async ({ page }) => {
    await setup(page, {
      section: "models", roles: ROLES(ON_EG2),
      health: memoryHealth({ state: "loading" }, { matching: 0 }),
    });
    const memory = await openClassicModels(page);
    await expect(memory.getByText("Searching by keyword while EmbeddingGemma 2 loads")).toBeVisible();

    await page.unroute("**/api/v1/context/index/health");
    await page.route("**/api/v1/context/index/health", (r) => r.fulfill({ json: memoryHealth({}, { matching: 40 }) }));
    await page.reload();
    await expect(page.getByRole("region", { name: "Memory models" }).getByText("Re-indexing: 40 of 95")).toBeVisible();
    await expect(page.getByRole("progressbar", { name: "Memories re-indexed" })).toHaveAttribute("aria-valuenow", "42");
  });

  test("says a failed load in plain words, and Try again chooses the model again without clearing the index", async ({ page }) => {
    const seen = await setup(page, {
      section: "models", roles: ROLES(ON_EG2),
      health: memoryHealth({ state: "failed", reason: "loading embeddinggemma-2-Q8_0.gguf on the Gpu: out of memory" }, { matching: 0 }),
    });
    const memory = await openClassicModels(page);
    const alert = memory.getByRole("alert");
    await expect(alert).toContainText("EmbeddingGemma 2 did not load, so Memory is searching by keyword.");
    await expect(alert).toContainText("out of memory");
    await alert.getByRole("button", { name: "Try again" }).click();
    await expect.poll(() => seen.activated.length).toBe(1);
    expect(seen.activated[0].body).toEqual({ role: "embedding" });
    expect(seen.rebuilt).toBe(0);
  });

  test("says search by meaning is off, and turns it on with one press", async ({ page }) => {
    const seen = await setup(page, {
      section: "models",
      roles: ROLES({ model_id: null, model: "", provider: "none" }),
      health: { indexed: false, reason: "no embedding model is configured", model_id: null, dims: null, coverage: null, corpora: [], embedder: { state: "off", model_id: null, reason: null } },
    });
    const memory = await openClassicModels(page);
    await expect(memory.getByText("Search by meaning is off. Memory searches by keyword.")).toBeVisible();
    await expect(memory.getByText("In use")).toHaveCount(0);
    await memory.getByRole("button", { name: "Turn on" }).click();
    await expect.poll(() => seen.saved).toContainEqual({ embedding_provider: "auto" });
  });

  test("keeps the four bands the page always had", async ({ page }) => {
    await setup(page, { section: "models" });
    await openClassicModels(page);
    await expect(page.locator(".mdl-band__title")).toHaveText([
      "Jobs", "Recommended for this pond", "On this device", "Get more",
    ]);
  });
});

test.describe("Memory, classic Context and Settings", () => {
  test("Lineage says what the pond is doing while the model loads", async ({ page }) => {
    await setup(page, {
      section: "context", roles: ROLES(ON_EG2),
      health: memoryHealth({ state: "loading" }, { matching: 0 }),
    });
    await page.goto("/");
    await page.getByRole("button", { name: /Lineage/ }).click();
    await expect(page.getByRole("status").filter({ hasText: "Searching by keyword while EmbeddingGemma 2 loads" })).toBeVisible();
  });

  test("Settings offers search by meaning as a switch, not a choice of engines", async ({ page }) => {
    const seen = await setup(page, { section: "settings" });
    await page.goto("/");
    await page.getByRole("button", { name: /^Models/ }).click();
    const row = page.locator(".scat__row", { hasText: "Search memories by meaning" });
    await expect(row).toBeVisible();
    await expect(row).toContainText("The model is chosen on the Models page.");
    await expect(page.getByText(/does not initialise on the Orin/i)).toHaveCount(0);
    await expect(page.getByRole("radiogroup", { name: /Embedding provider/ })).toHaveCount(0);

    // The pond stored "fastembed" before the switch existed, and that is on.
    const toggle = row.getByRole("switch", { name: "Search memories by meaning" }).or(row.getByLabel("Search memories by meaning"));
    await expect(toggle).toBeChecked();
    await row.locator(".switch__control").click();
    await page.getByRole("button", { name: /Save 1 change/ }).click();
    await expect.poll(() => seen.saved).toContainEqual({ embedding_provider: "none" });
  });
});

test.describe("Memory, hub", () => {
  test("shows the recommendation and the model in use, the rest a press away, with every action touch sized", async ({ page }) => {
    await setup(page, { section: "hub" });
    const card = await openHubModels(page);
    const rows = card.getByRole("listitem");
    await expect(rows).toHaveCount(2);
    await expect(rows.nth(0)).toContainText("EmbeddingGemma 2");
    await expect(rows.nth(0)).toContainText("295 MB to download");
    await expect(rows.nth(1)).toContainText("In use");
    await card.getByRole("button", { name: "Other Memory models (1)" }).click();
    await expect(rows).toHaveCount(3);
    for (const button of await card.getByRole("button").all()) {
      const box = await button.boundingBox();
      expect(box?.height ?? 0, await button.innerText()).toBeGreaterThanOrEqual(44);
    }
  });

  test("Use assigns the Memory role from the hub", async ({ page }) => {
    const seen = await setup(page, { section: "hub" });
    const card = await openHubModels(page);
    await card.getByRole("button", { name: "Use EmbeddingGemma 2 for memory" }).click();
    await expect.poll(() => seen.activated.length).toBe(1);
    expect(seen.activated[0].body).toEqual({ role: "embedding" });
  });

  test("the Memory screen has one switch, the model on the Models screen and a plain status", async ({ page }) => {
    const seen = await setup(page, {
      section: "hub", roles: ROLES(ON_EG2),
      health: memoryHealth({ state: "loading" }, { matching: 0 }),
    });
    await page.goto("/");
    await page.waitForSelector(".ghub", { timeout: 10_000 });
    await navigateTo(page, "Context");
    const card = page.locator(".setcard", { has: page.locator(".setcard__title", { hasText: "Semantic search" }) });
    await expect(card.getByText("Searching by keyword while EmbeddingGemma 2 loads")).toBeVisible();
    await expect(card.getByRole("combobox")).toHaveCount(0);

    await card.getByRole("button", { name: "Search by meaning" }).click();
    await expect.poll(() => seen.saved).toContainEqual({ embedding_provider: "none" });

    await card.getByRole("button", { name: "Choose a model" }).click();
    await expect(page.getByRole("heading", { name: "Models" })).toBeVisible();
  });
});

for (const scheme of ["Light", "Dark"] as const) {
  test.describe(`Memory accessibility, ${scheme.toLowerCase()}`, () => {
    test("the classic Memory models, with a download coming down", async ({ page }) => {
      await setup(page, {
        section: "models", scheme, roles: ROLES(ON_EG2), downloads: [EG2_DOWNLOAD],
        health: memoryHealth({ state: "failed", reason: FETCHING }, { matching: 0 }),
      });
      const memory = await openClassicModels(page);
      await expect(memory.getByRole("progressbar", { name: /EmbeddingGemma 2.*model/i })).toBeVisible();
      await scan(page, "#mdl-memory", "classic Memory models");
    });

    test("the classic Memory models, with a load that failed and a re-index counting", async ({ page }) => {
      await setup(page, {
        section: "models", scheme, roles: ROLES(ON_EG2),
        health: memoryHealth({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }, { matching: 0 }),
      });
      const memory = await openClassicModels(page);
      await expect(memory.getByRole("alert")).toBeVisible();
      await scan(page, "#mdl-memory", "classic Memory models, failed");

      await page.unroute("**/api/v1/context/index/health");
      await page.route("**/api/v1/context/index/health", (r) => r.fulfill({ json: memoryHealth({}, { matching: 40 }) }));
      await page.reload();
      await expect(page.getByText("Re-indexing: 40 of 95")).toBeVisible();
      await scan(page, "#mdl-memory", "classic Memory models, re-indexing");
    });

    test("the hub Memory card and the Memory screen", async ({ page }) => {
      await setup(page, {
        section: "hub", scheme, roles: ROLES(ON_EG2),
        health: memoryHealth({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }, { matching: 0 }),
      });
      const card = await openHubModels(page);
      await expect(card.getByRole("alert")).toBeVisible();
      await scan(page, ".ghub__main", "hub Memory card");

      await navigateTo(page, "Context");
      await expect(page.getByText("Semantic search")).toBeVisible();
      await expect(page.getByRole("alert").filter({ hasText: "did not load" })).toBeVisible();
      // The card this change owns; the screen's own Add button measures 3.74:1 and is not part of it.
      await scan(page, ".setcard:has(.hm-embed)", "hub Memory screen, Semantic search");
    });

    test("the settings switch row", async ({ page }) => {
      await setup(page, { section: "settings", scheme });
      await page.goto("/");
      await page.getByRole("button", { name: /^Models/ }).click();
      await expect(page.locator(".scat__row", { hasText: "Search memories by meaning" })).toBeVisible();
      // The row this change owns; a "New" badge elsewhere in the category measures 2.74:1 in dark.
      await scan(page, ".scat__row:has([aria-label='Search memories by meaning'])", "settings switch");
    });
  });
}
