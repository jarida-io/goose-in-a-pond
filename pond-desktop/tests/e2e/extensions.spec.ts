import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

// ── Helper ────────────────────────────────────────────────────

async function goToExtensions(page: import("@playwright/test").Page) {
  await page
    .getByRole("button")
    .filter({ hasText: /extensions/i })
    .first()
    .click();
}

// ── Tests ─────────────────────────────────────────────────────

test.describe("Extensions section", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.goto("/");
    await goToExtensions(page);
  });

  // ── Installed tab ──────────────────────────────────────────

  test("renders Extensions page with Installed tab active by default", async ({ page }) => {
    await expect(page.getByRole("heading", { name: /extensions/i })).toBeVisible();
    const installedTab = page.getByRole("tab", { name: /installed/i })
      .or(page.locator(".ext-tab-bar__tab--active").filter({ hasText: /installed/i }))
      .first();
    await expect(installedTab).toBeVisible({ timeout: 8_000 });
  });

  test("shows empty state on Installed tab when no extensions registered", async ({ page }) => {
    await expect(page.getByText(/no extensions registered/i)).toBeVisible({ timeout: 8_000 });
  });

  test("Installed tab shows inline link to browse marketplace", async ({ page }) => {
    await expect(page.getByText(/browse the marketplace/i)).toBeVisible({ timeout: 8_000 });
  });

  test("Add Extension accordion is visible on Installed tab", async ({ page }) => {
    await expect(page.getByText(/add extension/i)).toBeVisible({ timeout: 8_000 });
  });

  // ── Browse tab ─────────────────────────────────────────────

  test("Browse tab button is visible", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await expect(browseTab).toBeVisible({ timeout: 8_000 });
  });

  test("clicking Browse tab switches view and shows marketplace extensions", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    await expect(page.getByText(/weather tools/i)).toBeVisible({ timeout: 8_000 });
    await expect(page.getByText(/git helper/i)).toBeVisible({ timeout: 8_000 });
  });

  test("Browse tab shows category badges", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    await expect(page.getByText(/productivity/i).first()).toBeVisible({ timeout: 8_000 });
    await expect(page.getByText(/development/i).first()).toBeVisible({ timeout: 8_000 });
  });

  test("Browse tab shows author attribution", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    await expect(page.getByText(/pond team/i).first()).toBeVisible({ timeout: 8_000 });
    await expect(page.getByText(/community/i).first()).toBeVisible({ timeout: 8_000 });
  });

  test("Browse tab shows tool counts on marketplace cards", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    await expect(page.getByText(/3 tools/i).first()).toBeVisible({ timeout: 8_000 });
  });

  test("Browse tab shows Install buttons for uninstalled extensions", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    await expect(page.getByText(/^install$/i).first()).toBeVisible({ timeout: 8_000 });
  });

  test("clicking Install transitions to Installed badge and refreshes installed list", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    const installBtn = page.locator(".mkt-card__install-btn").first();
    await expect(installBtn).toBeVisible({ timeout: 8_000 });
    await installBtn.click();

    await expect(
      page.locator(".mkt-card__installed-badge").or(page.getByText(/installed/i).first())
    ).toBeVisible({ timeout: 8_000 });
  });

  // ── Tab switch from empty state ────────────────────────────

  test("clicking 'browse the marketplace' link in empty state switches to Browse tab", async ({ page }) => {
    const link = page.getByText(/browse the marketplace/i);
    await expect(link).toBeVisible({ timeout: 8_000 });
    await link.click();

    await expect(page.getByText(/weather tools/i)).toBeVisible({ timeout: 8_000 });
  });

  // ── Secret Config Modal ─────────────────────────────────────

  test("Install on extension with required_secrets opens SecretConfigModal", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    // The GitHub mock has required_secrets.
    const githubCard = page.locator(".mkt-card").filter({ hasText: /github/i });
    await expect(githubCard).toBeVisible({ timeout: 8_000 });

    const installBtn = githubCard.locator(".mkt-card__install-btn");
    await installBtn.click();

    await expect(page.locator(".secret-modal-backdrop")).toBeVisible({ timeout: 5_000 });
    await expect(page.locator(".secret-modal")).toBeVisible();
  });

  test("SecretConfigModal shows field label and required badge for api_key secrets", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    const githubCard = page.locator(".mkt-card").filter({ hasText: /github/i });
    await githubCard.locator(".mkt-card__install-btn").click();

    await expect(page.locator(".secret-modal")).toBeVisible({ timeout: 5_000 });
    // Field label is the display_name
    await expect(page.getByText(/github personal access token/i)).toBeVisible();
    await expect(page.locator(".secret-modal__required-badge")).toBeVisible();
    // Help text from description
    await expect(page.getByText(/classic token with repo scope/i)).toBeVisible();
  });

  test("SecretConfigModal password input has show/hide toggle", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    const githubCard = page.locator(".mkt-card").filter({ hasText: /github/i });
    await githubCard.locator(".mkt-card__install-btn").click();
    await expect(page.locator(".secret-modal")).toBeVisible({ timeout: 5_000 });

    const input = page.locator(".secret-modal__input");
    await expect(input).toHaveAttribute("type", "password");

    await page.locator(".secret-modal__input-toggle").click();
    await expect(input).toHaveAttribute("type", "text");
  });

  test("SecretConfigModal close button (X) dismisses the modal", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    const githubCard = page.locator(".mkt-card").filter({ hasText: /github/i });
    await githubCard.locator(".mkt-card__install-btn").click();
    await expect(page.locator(".secret-modal")).toBeVisible({ timeout: 5_000 });

    await page.locator(".secret-modal__close").click();
    await expect(page.locator(".secret-modal")).not.toBeVisible({ timeout: 3_000 });
  });

  test("SecretConfigModal Escape key closes the modal", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    const githubCard = page.locator(".mkt-card").filter({ hasText: /github/i });
    await githubCard.locator(".mkt-card__install-btn").click();
    await expect(page.locator(".secret-modal")).toBeVisible({ timeout: 5_000 });

    await page.keyboard.press("Escape");
    await expect(page.locator(".secret-modal")).not.toBeVisible({ timeout: 3_000 });
  });

  test("SecretConfigModal validation blocks Save & Install with empty required field", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    const githubCard = page.locator(".mkt-card").filter({ hasText: /github/i });
    await githubCard.locator(".mkt-card__install-btn").click();
    await expect(page.locator(".secret-modal")).toBeVisible({ timeout: 5_000 });

    await page.locator(".secret-modal__save-btn").click();
    await expect(page.locator(".secret-modal__field-error")).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".secret-modal")).toBeVisible();
  });

  test("filling the api_key field and clicking Save & Install completes the flow", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    const githubCard = page.locator(".mkt-card").filter({ hasText: /github/i });
    await githubCard.locator(".mkt-card__install-btn").click();
    await expect(page.locator(".secret-modal")).toBeVisible({ timeout: 5_000 });

    await page.locator(".secret-modal__input").fill("ghp_test_token_12345");

    await page.locator(".secret-modal__save-btn").click();

    await expect(page.locator(".secret-modal")).not.toBeVisible({ timeout: 5_000 });
    await expect(
      page.locator(".mkt-card__installed-badge").or(page.getByText(/installed/i).first())
    ).toBeVisible({ timeout: 5_000 });
  });

  test("OAuth-only extension shows Sign in button instead of input fields", async ({ page }) => {
    const browseTab = page.getByRole("tab", { name: /browse/i })
      .or(page.locator(".ext-tab-bar__tab").filter({ hasText: /browse/i }))
      .first();
    await browseTab.click();

    const spotifyCard = page.locator(".mkt-card").filter({ hasText: /spotify/i });
    await expect(spotifyCard).toBeVisible({ timeout: 8_000 });
    await spotifyCard.locator(".mkt-card__install-btn").click();

    await expect(page.locator(".secret-modal")).toBeVisible({ timeout: 5_000 });

    await expect(page.locator(".secret-modal__oauth-block")).toBeVisible();
    await expect(page.locator(".secret-modal__oauth-btn")).toBeVisible();
    await expect(page.locator(".secret-modal__input")).not.toBeVisible();

    // Button text includes display_name
    await expect(page.locator(".secret-modal__oauth-btn")).toContainText(/sign in with spotify/i);
  });
});
