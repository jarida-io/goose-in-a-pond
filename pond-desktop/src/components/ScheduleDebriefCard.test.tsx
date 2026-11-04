import { describe, it, expect, afterEach } from "vitest";
import { render, waitFor, cleanup } from "@testing-library/react";
import { ScheduleDebriefCard } from "./ScheduleDebriefCard";
import type { ScheduleRunNotification } from "../api/types";

afterEach(cleanup);

const briefing =
  "### Kenya & Nairobi Briefing\n\n**Weather & Climate:**\n\n*   **Nairobi Current Weather:** 24C, clear\n*   **Exchange Rate:** 129 KES";

function run(recipe: string): ScheduleRunNotification {
  return {
    id: "r1",
    scheduleId: "s1",
    scheduleName: "Daily Brief",
    status: "completed",
    result: briefing,
    error: null,
    startedAt: new Date().toISOString(),
    finishedAt: new Date().toISOString(),
    durationMs: 1200,
    read: false,
    excerpt: "",
    recipe,
  } as ScheduleRunNotification;
}

describe("ScheduleDebriefCard result", () => {
  it.each(["daily-summary", "weekly-report", "custom"])("renders a %s result as markdown once typed", async (recipe) => {
    const { container } = render(<ScheduleDebriefCard run={run(recipe)} />);
    const zone = () => container.querySelector(".debrief-tw") as HTMLElement;

    await waitFor(() => expect(container.querySelector(".debrief-cursor")).toBeNull(), { timeout: 5000 });

    expect(zone().querySelector(".md__h")?.textContent).toBe("Kenya & Nairobi Briefing");
    expect(zone().querySelectorAll("li")).toHaveLength(2);
    expect(zone().querySelector("strong")?.textContent).toBe("Weather & Climate:");
    expect(zone().textContent).not.toMatch(/\*\*|###/);
  });
});
