/** A task asked for behind others: its card says where it waits, and the member stops one of theirs to move it up. */
import { test, expect } from "@playwright/test";
import { mockAllApiRoutes, mockSseStream } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";

const TASK = "0b6f1c1e-52a1-4c55-9a53-3d3a8a1f7a10";
const FRONT = "5f0a3c2e-9b1d-4e6a-8c7b-2d4e6f8a0b1c";
const SESSION = "e2e-session";

function waitingTask(position: number, ahead: unknown[]) {
  return {
    id: TASK,
    goal: "Plan a trip for 4 to Arusha",
    kind: "research",
    state: "planning",
    origin_session_id: SESSION,
    created_at: "2026-10-29T08:00:00Z",
    updated_at: "2026-10-29T08:00:00Z",
    finished_at: null,
    steps: [],
    artifacts: [],
    result_artifact_id: null,
    queue: { position, of: position, ahead },
  };
}

test("a waiting task offers Keep going or Stop for the member's own tasks ahead of it", async ({ page }) => {
  const stopped: string[] = [];
  await mockAllApiRoutes(page);
  await page.route("**/api/v1/chat/stream", (route) =>
    route.fulfill({
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
      body: mockSseStream([
        { type: "tool_call", id: "dr-1", tool: "giap-tasks__task_start", result: {} },
        { type: "tool_result", id: "dr-1", tool: "giap-tasks__task_start", content: `Started task ${TASK}. The pond works on it while the household is quiet.` },
        { type: "text", content: "I'll look into it once the trip plan ahead of it is done.", token: "I'll look into it once the trip plan ahead of it is done." },
        { done: true, session_id: SESSION, model_role: "chat" },
      ]),
    }),
  );
  const current = () =>
    stopped.includes(FRONT)
      ? waitingTask(2, [{ another_member: true }])
      : waitingTask(3, [{ id: FRONT, goal: "Plan a four-day trip to Arusha", state: "running" }, { another_member: true }]);
  await page.route(
    (url) => url.pathname === "/api/v1/long-tasks",
    (route) => route.fulfill({ json: [current()] }),
  );
  await page.route(`**/api/v1/long-tasks/${TASK}`, (route) => route.fulfill({ json: current() }));
  await page.route(`**/api/v1/long-tasks/${FRONT}/cancel`, (route) => {
    stopped.push(FRONT);
    return route.fulfill({ json: { id: FRONT, state: "cancelled" } });
  });

  await page.goto("/");
  await navigateTo(page, "Chat");
  const textarea = page.locator("textarea").first();
  await textarea.fill("Plan a trip for 4 to Arusha");
  await textarea.press("Meta+Enter");

  const card = page.getByRole("group", { name: "Deep research: Plan a trip for 4 to Arusha" });
  await expect(card.getByText("Waiting its turn: 3rd in line")).toBeVisible({ timeout: 10_000 });
  const line = card.getByRole("region", { name: "Tasks in line" });
  await expect(line.getByText("Ahead of this one: 1 of your tasks and 1 for another member.")).toBeVisible();
  await expect(line.getByText("Everything here keeps going.")).toBeVisible();
  await card.screenshot({ path: "test-results/long-task-queue-waiting.png" });

  const front = line.getByRole("group", { name: /Plan a four-day trip to Arusha/ });
  await front.getByText("Stop", { exact: true }).click();
  await expect(front.getByRole("radio", { name: "Stop" })).toBeChecked();
  await card.screenshot({ path: "test-results/long-task-queue-picked.png" });
  await line.getByRole("button", { name: "Stop 1 task" }).click();

  await expect(line.getByRole("status")).toHaveText("Stopped 1 task.");
  expect(stopped).toEqual([FRONT]);
  await expect(card.getByText("Waiting its turn: 2nd in line")).toBeVisible({ timeout: 10_000 });
  await expect(line.getByText("Ahead of this one: 1 for another member.")).toBeVisible();
  await expect(line.getByRole("radio")).toHaveCount(0);
});
