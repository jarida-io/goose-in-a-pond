import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import { ScheduleOrRecipe } from "./ScheduleOrRecipe";
import { TOOL_REACH } from "./toolReach";

afterEach(cleanup);

function cells(tool: string): string[] {
  const row = screen.getByRole("rowheader", { name: tool }).closest("tr")!;
  return within(row).getAllByRole("cell").map((c) => c.textContent ?? "");
}

describe("ScheduleOrRecipe", () => {
  it("tells, for each tool, whether a schedule, a recipe and an agent can use it", () => {
    render(<ScheduleOrRecipe />);
    expect(cells("Weather")).toEqual(["Yes", "If ticked", "If chosen"]);
    expect(cells("Look things up and search the web")).toEqual(["Yes", "Only if none ticked", "If chosen"]);
    expect(cells("Switch devices on and off")).toEqual(["Yes", "If ticked", "Never"]);
    expect(screen.getAllByRole("row")).toHaveLength(TOOL_REACH.length + 1);
  });

  // A name the server does not map narrows a recipe that ticks only it to no tools at all.
  it("offers recipes only tool names the server maps to a tool group", () => {
    const routes = readFileSync(resolve(process.cwd(), "../crates/pond-api/src/routes.rs"), "utf8");
    const mapping = routes.slice(routes.indexOf("fn recipe_extension_to_tool_group"));
    for (const t of TOOL_REACH.filter((t) => t.recipe)) {
      expect(mapping, t.recipe).toMatch(new RegExp(`"${t.recipe}"[^=]*=> Some\\("${t.group}"\\)`));
    }
  });
});
