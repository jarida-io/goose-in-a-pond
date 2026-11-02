/** The Memory job in both UIs: EmbeddingGemma 2 offered, the switch watched, search by meaning on or off. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { contrastOf, fillContrastOf } from "./helpers/contrast";
import { navigateTo } from "./helpers/nav";
import {
  memoryHealth, memoryRows, mockModels, ORIN_SERVING_MEMORY, typicalModels, type ModelMocks,
} from "./helpers/model-mocks";

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

/** The Memory model a roles reply names, or the built-in default when nobody chose. */
const heldBy = (roles: Record<string, unknown>) =>
  ((roles.embedding as { model?: string } | undefined)?.model || "all-MiniLM-L6-v2");

/** Registered before navigating. `seen` records what the screen asks the pond to do. */
async function setup(
  page: Page,
  opts: {
    section: string; route?: string; scheme?: "Light" | "Dark"; size?: [number, number];
    networkMode?: "open" | "allowlist" | "offline";
  } & ModelMocks,
): Promise<Seen> {
  const seen: Seen = { activated: [], rebuilt: 0, saved: [] };
  await mockAllApiRoutes(page);
  const roles = opts.roles ?? ROLES(DEFAULT_MEMORY);
  await mockModels(page, {
    // The model in use is on this device, unless its file is the one coming down.
    models: typicalModels({ embedding: memoryRows(opts.downloads?.length ? [] : [heldBy(roles)]) }),
    health: memoryHealth({ model_id: "all-MiniLM-L6-v2" }, { model_id: "all-MiniLM-L6-v2", dims: 384 }),
    ...opts,
    roles,
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
        ...(opts.networkMode ? { network_mode: opts.networkMode } : {}),
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

/** Holds every index report until the returned function is called, as a slow pond does on its first read. */
async function holdHealth(page: Page, json: Record<string, unknown>): Promise<() => void> {
  let release = () => {};
  const answered = new Promise<void>((ok) => { release = ok; });
  await page.route("**/api/v1/context/index/health", async (route) => {
    await answered;
    await route.fulfill({ json }).catch(() => undefined);
  });
  return release;
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
    // The press that spends 295 MB says so, with the size in its name.
    await expect(rows.nth(0).getByRole("button")).toHaveCount(1);
    await expect(rows.nth(0).getByRole("button", { name: "Download and use EmbeddingGemma 2 for memory, 295 MB" })).toHaveText("Download and use");
    await expect(rows.filter({ hasText: "all-MiniLM-L6-v2" })).toContainText("In use");
    await expect(rows.filter({ hasText: "all-MiniLM-L6-v2" })).not.toContainText("to download");
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

    await memory.getByRole("button", { name: "Download and use EmbeddingGemma 2 for memory, 295 MB" }).click();
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
    const seen = await setup(page, {
      section: "models", roles: ROLES(ON_EG2), health: memoryHealth({}, { matching: 95 }),
    });
    const memory = await openClassicModels(page);
    const inUse = memory.getByRole("listitem").filter({ hasText: "EmbeddingGemma 2" });
    await expect(inUse).toContainText("In use");
    await expect(inUse).toContainText("On this device, 295 MB");
    await expect(inUse).not.toContainText("to download");
    await memory.getByRole("button", { name: "Use all-MiniLM-L6-v2 for memory" }).click();
    await expect.poll(() => seen.activated.length).toBe(1);
    expect(seen.activated[0].url).toContain("/models/embedding/all-MiniLM-L6-v2/activate");
  });

  test("says it searches by keyword while the model loads, and counts reading memories again", async ({ page }) => {
    await setup(page, {
      section: "models", roles: ROLES(ON_EG2),
      health: memoryHealth({ state: "loading" }, { matching: 0 }),
    });
    const memory = await openClassicModels(page);
    await expect(memory.getByText("Searching by keyword while EmbeddingGemma 2 loads")).toBeVisible();
    // Loading is not in use, and the row says which.
    const row = memory.getByRole("listitem").filter({ hasText: "EmbeddingGemma 2" });
    await expect(row).toContainText("Loading");
    await expect(row).not.toContainText("In use");

    await page.unroute("**/api/v1/context/index/health");
    await page.route("**/api/v1/context/index/health", (r) => r.fulfill({ json: memoryHealth({}, { matching: 40 }) }));
    await page.reload();
    await expect(page.getByRole("region", { name: "Memory models" }).getByText("Reading memories again: 40 of 95 items")).toBeVisible();
    await expect(page.getByRole("progressbar", { name: "Items read again" })).toHaveAttribute("aria-valuenow", "42");
    await expect(page.getByRole("region", { name: "Memory models" }).getByRole("listitem").filter({ hasText: "EmbeddingGemma 2" })).toContainText("In use");
  });

  test("says a failed load in plain words, names the lighter model, and Try again chooses the model again without clearing the index", async ({ page }) => {
    const seen = await setup(page, {
      section: "models", roles: ROLES(ON_EG2),
      health: memoryHealth({ state: "failed", reason: "loading embeddinggemma-2-Q8_0.gguf on the Gpu: out of memory" }, { matching: 0 }),
    });
    const memory = await openClassicModels(page);
    const alert = memory.getByRole("alert");
    await expect(alert).toContainText("There is not enough free memory to load EmbeddingGemma 2, so Memory is searching by keyword.");
    await expect(alert).toContainText("Choose the lighter all-MiniLM-L6-v2 (23 MB), or try again.");
    // The pond's own words wait behind Details, and the row does not claim the model runs.
    await expect(alert).not.toContainText("on the Gpu");
    await alert.getByRole("button", { name: "Details" }).click();
    await expect(alert).toContainText("loading embeddinggemma-2-Q8_0.gguf on the Gpu: out of memory");
    const row = memory.getByRole("listitem").filter({ hasText: "EmbeddingGemma 2" });
    await expect(row).toContainText("Chosen, not loaded");
    await expect(memory.getByText("In use")).toHaveCount(0);
    // The Jobs card names the model and, while it is not running, says so too.
    await expect(page.locator(".mdl-role", { hasText: "Memory" })).toContainText("Chosen, not loaded");
    await alert.getByRole("button", { name: "Try again" }).click();
    await expect.poll(() => seen.activated.length).toBe(1);
    expect(seen.activated[0].body).toEqual({ role: "embedding" });
    expect(seen.rebuilt).toBe(0);
  });

  test("claims nothing about the chosen model before the pond's first answer", async ({ page }) => {
    await setup(page, { section: "models", roles: ROLES(ON_EG2) });
    const answer = await holdHealth(page, memoryHealth({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }, { matching: 0 }));
    const memory = await openClassicModels(page);
    const row = memory.getByRole("listitem").filter({ hasText: "EmbeddingGemma 2" });
    await expect(row).toContainText("Checking");
    await expect(memory.getByText("In use")).toHaveCount(0);
    answer();
    await expect(row).toContainText("Chosen, not loaded");
    await expect(memory.getByText("In use")).toHaveCount(0);
  });

  test("rests Download and use, with the reason, when the pond reaches nothing outside the house", async ({ page }) => {
    await setup(page, {
      section: "models", networkMode: "offline",
      models: typicalModels({ embedding: memoryRows(["all-MiniLM-L6-v2", "nomic-embed-text-v1.5"]) }),
    });
    const memory = await openClassicModels(page);
    const press = memory.getByRole("button", { name: "Download and use EmbeddingGemma 2 for memory, 295 MB" });
    await expect(press).toBeDisabled();
    await expect(memory.getByText("Needs a connection to download")).toBeVisible();
    await expect(press).toHaveAccessibleDescription("Needs a connection to download");
    // A model whose file is here needs no connection.
    await expect(memory.getByRole("button", { name: "Use Nomic Embed Text v1.5 for memory" })).toBeEnabled();
  });

  test("Use while search by meaning is off sets the model and promises no re-read", async ({ page }) => {
    const seen = await setup(page, {
      section: "models",
      roles: ROLES({ model_id: null, model: "", provider: "none" }),
      models: typicalModels({ embedding: memoryRows(["nomic-embed-text-v1.5"]) }),
      health: { indexed: false, reason: "no embedding model is configured", model_id: null, dims: null, coverage: null, corpora: [], embedder: { state: "off", model_id: null, reason: null } },
    });
    const memory = await openClassicModels(page);
    // Nothing is fetched while it is off, so no row offers a download.
    await expect(memory.getByRole("button", { name: /Download and use/ })).toHaveCount(0);
    await memory.getByRole("button", { name: "Use Nomic Embed Text v1.5 for memory" }).click();
    await expect.poll(() => seen.activated.length).toBe(1);
    await expect(page.getByText("Nomic Embed Text v1.5 is set for memory. The pond uses it once you turn on search by meaning.")).toBeVisible();
    await expect(page.getByText(/reads memories again|Now using/)).toHaveCount(0);
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

  test("says what each model needs while it runs, and whether this pond has room for it", async ({ page }) => {
    await setup(page, { section: "models", memory: ORIN_SERVING_MEMORY });
    const memory = await openClassicModels(page);
    const row = (name: string) => memory.getByRole("listitem").filter({ hasText: name });
    await expect(row("EmbeddingGemma 2")).toContainText("Needs about 368 MB while it runs");
    await expect(row("EmbeddingGemma 2")).toContainText("Tight on this pond");
    await expect(row("Nomic Embed Text v1.5")).toContainText("Fits");
    // The model in use says how it is doing, not whether it would fit.
    await expect(row("all-MiniLM-L6-v2")).toContainText("Needs about 50 MB while it runs");
    await expect(row("all-MiniLM-L6-v2")).not.toContainText("Fits");
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

  test("Lineage keeps one name for reading memories again, and names the model rather than its stamp", async ({ page }) => {
    await setup(page, {
      section: "context", roles: ROLES(ON_EG2),
      health: memoryHealth({}, { matching: 40 }),
    });
    await page.goto("/");
    await page.getByRole("button", { name: /Lineage/ }).click();
    await expect(page.getByRole("status").filter({ hasText: "Reading memories again: 40 of 95 items" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Read memories again" })).toBeVisible();
    await expect(page.getByText("Read by EmbeddingGemma 2.")).toBeVisible();
    await expect(page.getByText(/embeddinggemma-2-256|dimensions/)).toHaveCount(0);
    // The bars count what the line counts: memories, then summaries, 40 + 0 of 45 + 50.
    await expect(page.locator(".lin__corpusCount")).toHaveText(["40 of 45", "nothing here yet", "0 of 50"]);
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
    await expect(rows.nth(0).getByRole("button")).toHaveText("Download and use");
    await expect(rows.nth(1)).toContainText("In use");
    // What a model costs is read at body size in the text colour, not the faintest line on the card.
    const cost = rows.nth(0).getByText("295 MB to download");
    await expect(cost).toHaveCSS("font-size", "13px");
    expect(await contrastOf(cost)).toBeGreaterThanOrEqual(15);
    await card.getByRole("button", { name: "Other Memory models (1)" }).click();
    await expect(rows).toHaveCount(3);
    for (const button of await card.getByRole("button").all()) {
      const box = await button.boundingBox();
      expect(box?.height ?? 0, await button.innerText()).toBeGreaterThanOrEqual(44);
    }
  });

  test("a failed load keeps every action touch sized, Details and Try again included", async ({ page }) => {
    await setup(page, {
      section: "hub", size: [800, 480], roles: ROLES(ON_EG2),
      health: memoryHealth({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }, { matching: 0 }),
    });
    const card = await openHubModels(page);
    await expect(card.getByRole("alert")).toContainText("not enough free memory");
    await expect(card.getByRole("listitem").first()).toContainText("Chosen, not loaded");
    await expect(page.locator(".hm-tile", { hasText: "Memory" })).toContainText("Chosen, not loaded");
    await card.getByRole("button", { name: "Details" }).click();
    await expect(card.getByRole("alert")).toContainText("on the Gpu: out of memory");
    for (const button of await card.getByRole("button").all()) {
      const box = await button.boundingBox();
      expect(box?.height ?? 0, await button.innerText()).toBeGreaterThanOrEqual(44);
    }
  });

  test("claims nothing about the chosen model before the pond's first answer", async ({ page }) => {
    await setup(page, { section: "hub", size: [800, 480], roles: ROLES(ON_EG2) });
    const answer = await holdHealth(page, memoryHealth({ state: "loading" }, { matching: 0 }));
    const card = await openHubModels(page);
    await expect(card.getByRole("listitem").first()).toContainText("Checking");
    await expect(card.getByText("In use")).toHaveCount(0);
    answer();
    await expect(card.getByRole("listitem").first()).toContainText("Loading");
  });

  test("the Memory tile takes a press to the Memory card, and the keyboard with it", async ({ page }) => {
    await setup(page, {
      section: "hub", size: [800, 480], roles: ROLES(ON_EG2),
      health: memoryHealth({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }, { matching: 0 }),
    });
    const card = await openHubModels(page);
    await expect(card).not.toBeInViewport();
    const tile = page.getByRole("button", { name: "Memory: EmbeddingGemma 2, Chosen, not loaded. Change" });
    expect((await tile.boundingBox())?.height ?? 0).toBeGreaterThanOrEqual(44);
    await tile.click();
    await expect(card).toBeInViewport();
    await expect(card).toBeFocused();
    // Listening and the voice are both chosen on the Speech card.
    await page.getByRole("button", { name: /^Speaking: / }).click();
    await expect(page.locator("#hm-card-speech")).toBeFocused();
  });

  test("Use assigns the Memory role from the hub", async ({ page }) => {
    const seen = await setup(page, { section: "hub" });
    const card = await openHubModels(page);
    await card.getByRole("button", { name: "Download and use EmbeddingGemma 2 for memory, 295 MB" }).click();
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
    const card = page.locator(".setcard", { has: page.locator(".setcard__title", { hasText: /^Search by meaning$/ }) });
    await expect(card.getByText("Searching by keyword while EmbeddingGemma 2 loads")).toBeVisible();
    // Loading is not "Using": the Model line says what the embedder has done with the model.
    await expect(card.getByText("EmbeddingGemma 2 is loading.")).toBeVisible();
    await expect(card.getByText(/^Using /)).toHaveCount(0);
    await expect(card.getByText(/Swahili/)).toHaveCount(0);
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
      await memory.getByRole("button", { name: "Details" }).click();
      await expect(memory.getByRole("alert")).toContainText("on the Gpu: out of memory");
      await scan(page, "#mdl-memory", "classic Memory models, failed, with Details open");

      await page.unroute("**/api/v1/context/index/health");
      await page.route("**/api/v1/context/index/health", (r) => r.fulfill({ json: memoryHealth({}, { matching: 40 }) }));
      await page.reload();
      await expect(page.getByText("Reading memories again: 40 of 95 items")).toBeVisible();
      await scan(page, "#mdl-memory", "classic Memory models, reading memories again");
    });

    // axe files text on a translucent colour as incomplete, so the new words are measured over what is behind them.
    test("the state words and the Details toggle, measured", async ({ page }) => {
      await setup(page, {
        section: "models", scheme, roles: ROLES(ON_EG2),
        health: memoryHealth({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }, { matching: 0 }),
      });
      const memory = await openClassicModels(page);
      const alert = memory.getByRole("alert");
      expect(await contrastOf(alert.getByRole("button", { name: "Details" })), "Details").toBeGreaterThanOrEqual(4.5);
      expect(await contrastOf(memory.getByText("Chosen, not loaded")), "Chosen, not loaded").toBeGreaterThanOrEqual(4.5);
      expect(
        await contrastOf(page.locator(".mdl-role").getByText("Chosen, not loaded")), "the Jobs card's state",
      ).toBeGreaterThanOrEqual(4.5);
      await alert.getByRole("button", { name: "Details" }).click();
      expect(await contrastOf(alert.getByText(/on the Gpu: out of memory/)), "the pond's reason").toBeGreaterThanOrEqual(4.5);

      await page.unroute("**/api/v1/context/index/health");
      await page.route("**/api/v1/context/index/health", (r) => r.fulfill({ json: memoryHealth({ state: "loading" }, { matching: 0 }) }));
      await page.reload();
      expect(await contrastOf(page.getByRole("region", { name: "Memory models" }).getByText("Loading", { exact: true })), "Loading").toBeGreaterThanOrEqual(4.5);
    });

    test("the need and the fit word, measured", async ({ page }) => {
      await setup(page, { section: "models", scheme, memory: ORIN_SERVING_MEMORY });
      const memory = await openClassicModels(page);
      expect(await contrastOf(memory.getByText("Tight on this pond")), "Tight").toBeGreaterThanOrEqual(4.5);
      expect(await contrastOf(memory.getByText("Fits").first()), "Fits").toBeGreaterThanOrEqual(4.5);
      expect(
        await contrastOf(memory.getByText("Needs about 368 MB while it runs")), "the need",
      ).toBeGreaterThanOrEqual(4.5);
      await scan(page, "#mdl-memory", "classic Memory models, with a fit reading");
    });

    test("the offline note, measured", async ({ page }) => {
      await setup(page, { section: "models", scheme, networkMode: "offline" });
      const memory = await openClassicModels(page);
      expect(await contrastOf(memory.getByText("Needs a connection to download").first())).toBeGreaterThanOrEqual(4.5);
    });

    test("the classic Memory models, with a download that needs a connection", async ({ page }) => {
      await setup(page, { section: "models", scheme, networkMode: "offline" });
      const memory = await openClassicModels(page);
      // EmbeddingGemma 2 and Nomic both need a file, so both rest.
      await expect(memory.getByText("Needs a connection to download")).toHaveCount(2);
      await scan(page, "#mdl-memory", "classic Memory models, offline");
    });

    test("the hub Memory card and the Memory screen", async ({ page }) => {
      await setup(page, {
        section: "hub", scheme, roles: ROLES(ON_EG2),
        health: memoryHealth({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }, { matching: 0 }),
      });
      const card = await openHubModels(page);
      await expect(card.getByRole("alert")).toBeVisible();
      await scan(page, ".ghub__main", "hub Memory card");
      expect(
        await contrastOf(page.locator(".hm-tile").getByText("Chosen, not loaded")), "the Memory tile's state",
      ).toBeGreaterThanOrEqual(4.5);

      await navigateTo(page, "Context");
      await expect(page.locator(".setcard__title", { hasText: /^Search by meaning$/ })).toBeVisible();
      // The hub's switch wears the same accent as the classic one, lifted the same way in dark.
      const accent = await page.evaluate((dark) => {
        const probe = document.createElement("span");
        probe.style.background = dark ? "color-mix(in srgb, var(--pp) 85%, white)" : "var(--pp)";
        document.body.append(probe);
        const value = getComputedStyle(probe).backgroundColor;
        probe.remove();
        return value;
      }, scheme === "Dark");
      const hubSwitch = page.getByRole("button", { name: "Search by meaning" });
      await expect(hubSwitch).toHaveCSS("background-color", accent);
      await expect(page.getByRole("alert").filter({ hasText: "not enough free memory" })).toBeVisible();
      await expect(page.getByText("EmbeddingGemma 2 is chosen but did not load.")).toBeVisible();
      // The card this change owns; the screen's own Add button measures 3.74:1 and is not part of it.
      await scan(page, ".setcard:has(.hm-embed)", "hub Memory screen, Search by meaning");
      // Off is a slate track that still clears 3:1 on the card.
      await hubSwitch.click();
      await expect(hubSwitch).toHaveAttribute("aria-pressed", "false");
      await page.mouse.move(0, 0);
      await expect.poll(() => fillContrastOf(hubSwitch), { message: "the hub switch, off" }).toBeGreaterThanOrEqual(3);
      // The knob rests at the track's ends, inside it: left when off, right when on.
      const knobAt = async () => {
        const [track, knob] = await Promise.all([hubSwitch.boundingBox(), hubSwitch.locator(".htoggle__knob").boundingBox()]);
        return { left: (knob?.x ?? 0) - (track?.x ?? 0), right: (track?.x ?? 0) + (track?.width ?? 0) - (knob?.x ?? 0) - (knob?.width ?? 0) };
      };
      await expect.poll(async () => (await knobAt()).left).toBeLessThanOrEqual(4);
      await hubSwitch.click();
      await expect.poll(async () => (await knobAt()).right).toBeGreaterThanOrEqual(0);
      await expect.poll(async () => (await knobAt()).right).toBeLessThanOrEqual(4);
    });

    test("the settings switch row", async ({ page }) => {
      await setup(page, { section: "settings", scheme });
      await page.goto("/");
      await page.getByRole("button", { name: /^Models/ }).click();
      const row = page.locator(".scat__row", { hasText: "Search memories by meaning" });
      await expect(row).toBeVisible();
      // On is the household's accent, lifted in dark to clear 3:1 on the card, never HeroUI's blue.
      const accent = await page.evaluate((dark) => {
        const probe = document.createElement("span");
        probe.style.background = dark ? "color-mix(in srgb, var(--color-accent) 85%, white)" : "var(--color-accent)";
        document.body.append(probe);
        const value = getComputedStyle(probe).backgroundColor;
        probe.remove();
        return value;
      }, scheme === "Dark");
      await expect(row.locator(".switch__control")).toHaveCSS("background-color", accent);
      await row.locator(".switch__control").click();
      await page.mouse.move(0, 0);
      await expect.poll(() => fillContrastOf(row.locator(".switch__control")), { message: "the Settings switch, off" })
        .toBeGreaterThanOrEqual(3);
      // The row this change owns; a "New" badge elsewhere in the category measures 2.74:1 in dark.
      await scan(page, ".scat__row:has([aria-label='Search memories by meaning'])", "settings switch");
    });
  });
}
