import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import { AgentPrefill, AgentPrefillLine } from "./AgentPrefill";
import { BuiltInAgents, PrefillBar } from "./BuiltInAgents";
import { DAY, HOUR, agentRow, ago, fakePrefills, readyReport, reportOf } from "./prefillFixtures";
import { PREFILL_ACTIVE_POLL_MS, PREFILL_IDLE_POLL_MS } from "./useAgentPrefills";

vi.mock("../api/PondApiClient", () => ({
  api: { getAgentPrefills: vi.fn(), rebuildAgentPrefill: vi.fn(), rebuildAllPrefills: vi.fn() },
}));

const confirmMock = vi.hoisted(() => vi.fn());
vi.mock("../components/shared", async () => {
  const actual = (await vi.importActual("../components/shared")) as Record<string, unknown>;
  return { ...actual, useConfirm: () => confirmMock };
});

beforeEach(() => {
  vi.resetAllMocks();
  confirmMock.mockResolvedValue(true);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("an agent's start-up copy, on its card and in its builder", () => {
  function show(status: Parameters<typeof agentRow>[1], over: Parameters<typeof fakePrefills>[0] = {}) {
    const prefills = fakePrefills({ report: reportOf([agentRow("greeter", status)]), rebuild: vi.fn(async () => true), ...over });
    render(<AgentPrefill agent="greeter" prefills={prefills} variant="card" />);
    return prefills;
  }

  it("says it is ready, and when it was built and last used", () => {
    show({ status: "ready", compiled_at: ago(2 * DAY), last_used_at: ago(3 * HOUR) });
    expect(screen.getByText("Start-up copy")).toBeTruthy();
    expect(screen.getByText("Ready")).toBeTruthy();
    expect(screen.getByText("Built 2 days ago. Last used 3 hours ago.")).toBeTruthy();
  });

  it.each([
    ["building", "Getting ready"],
    ["queued", "Waiting to be rebuilt"],
    ["not_built", "Not built yet"],
    ["failed", "Couldn't build"],
  ] as const)("says %s in plain words", (status, words) => {
    show({ status, compiled_at: null, last_used_at: null });
    expect(screen.getByText(words)).toBeTruthy();
  });

  it("says how long a resting copy has gone unused, and that its next run rebuilds it", () => {
    show({ status: "idle", last_used_at: ago(20 * DAY), compiled_at: ago(25 * DAY) });
    expect(screen.getByText("Resting: unused for 20 days, rebuilt on its next run")).toBeTruthy();
    expect(screen.getByText("Built 25 days ago. Last used 20 days ago.")).toBeTruthy();
  });

  it("asks for a rebuild when Rebuild is pressed", () => {
    const prefills = show({ status: "ready" });
    fireEvent.click(screen.getByRole("button", { name: "Rebuild greeter" }));
    expect(prefills.rebuild).toHaveBeenCalledWith("greeter");
  });

  it.each(["building", "queued"] as const)("does not offer a rebuild while the pond is already on it (%s)", (status) => {
    show({ status });
    expect((screen.getByRole("button", { name: "Rebuild greeter" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("does not offer a second ask while one is out", () => {
    show({ status: "ready" }, { busy: { greeter: true } });
    expect((screen.getByRole("button", { name: "Rebuild greeter" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("says what went wrong with the ask", () => {
    show({ status: "ready" }, { errors: { greeter: "Could not ask for a rebuild. offline" } });
    expect(screen.getByRole("alert").textContent).toBe("Could not ask for a rebuild. offline");
  });

  it("shows nothing for a pond that cannot say, a report not in yet, or an agent it does not list", () => {
    const a = render(<AgentPrefill agent="greeter" prefills={fakePrefills({ supported: false, report: readyReport() })} variant="card" />);
    expect(a.container.textContent).toBe("");
    a.unmount();
    const b = render(<AgentPrefill agent="greeter" prefills={fakePrefills({ report: null })} variant="card" />);
    expect(b.container.textContent).toBe("");
    b.unmount();
    const c = render(<AgentPrefill agent="stranger" prefills={fakePrefills({ report: readyReport() })} variant="builder" />);
    expect(c.container.textContent).toBe("");
  });
});

describe("Home's quiet line", () => {
  it("says how far the pond has got while it builds agents", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(
      reportOf([agentRow("study-planner", { status: "building" })], { active: true, current: "study-planner", done: 2, total: 5 }),
    );
    render(<AgentPrefillLine />);
    expect(await screen.findByText("Getting agents ready (2 of 5): study-planner")).toBeTruthy();
  });

  it("shows nothing when the pond is not building", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(readyReport());
    const { container } = render(<AgentPrefillLine />);
    await waitFor(() => expect(api.getAgentPrefills).toHaveBeenCalled());
    expect(container.textContent).toBe("");
  });

  it("shows nothing on a pond with no such route, and does not ask again", async () => {
    vi.useFakeTimers();
    vi.mocked(api.getAgentPrefills).mockRejectedValue(new ApiError(404, "Not Found"));
    const { container } = render(<AgentPrefillLine />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(PREFILL_IDLE_POLL_MS * 5);
    });
    expect(container.textContent).toBe("");
    expect(api.getAgentPrefills).toHaveBeenCalledTimes(1);
  });

  it("asks every 5 seconds while building, and goes quiet when the build ends", async () => {
    vi.useFakeTimers();
    vi.mocked(api.getAgentPrefills)
      .mockResolvedValueOnce(reportOf([], { active: true, current: "a", done: 0, total: 2 }))
      .mockResolvedValue(readyReport());
    const { container } = render(<AgentPrefillLine />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(container.textContent).toBe("Getting agents ready (0 of 2): a");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(PREFILL_ACTIVE_POLL_MS);
    });
    expect(api.getAgentPrefills).toHaveBeenCalledTimes(2);
    expect(container.textContent).toBe("");
  });
});

describe("the built-in agents", () => {
  it("lists the pond's own agents with their state, and not the household's", () => {
    render(<BuiltInAgents prefills={fakePrefills({ report: readyReport("greeter") })} variant="classic" />);
    const group = screen.getByRole("region", { name: "Built-in agents" });
    for (const name of ["Point Gander", "Forager Gosling", "Sentinel Goose"]) {
      const row = within(group).getByRole("listitem", { name });
      expect(within(row).getByText("Ready")).toBeTruthy();
      expect(within(row).getByText("Built 2 days ago. Last used 3 hours ago.")).toBeTruthy();
    }
    expect(within(group).queryByText("greeter")).toBeNull();
  });

  it("is read-only: no edit, no delete, no switch", () => {
    render(<BuiltInAgents prefills={fakePrefills({ report: readyReport() })} variant="hub" />);
    const group = screen.getByRole("region", { name: "Built-in agents" });
    expect(within(group).queryByRole("button", { name: /edit|delete/i })).toBeNull();
    expect(within(group).queryByRole("switch")).toBeNull();
    expect(within(group).getByText(/You cannot edit or delete them/)).toBeTruthy();
  });

  it("still lets a built-in be rebuilt", () => {
    const rebuild = vi.fn(async () => true);
    render(<BuiltInAgents prefills={fakePrefills({ report: readyReport(), rebuild })} variant="classic" />);
    fireEvent.click(screen.getByRole("button", { name: "Rebuild Forager Gosling" }));
    expect(rebuild).toHaveBeenCalledWith("long-task-worker");
  });

  it("shows each built-in's own state", () => {
    const report = reportOf([
      agentRow("long-task-planner", { status: "failed" }),
      agentRow("long-task-worker", { status: "idle", last_used_at: ago(20 * DAY) }),
      agentRow("long-task-verifier", { status: "queued" }),
    ]);
    render(<BuiltInAgents prefills={fakePrefills({ report })} variant="classic" />);
    expect(within(screen.getByRole("listitem", { name: "Point Gander" })).getByText("Couldn't build")).toBeTruthy();
    expect(within(screen.getByRole("listitem", { name: "Forager Gosling" })).getByText(/^Resting: unused for 20 days/)).toBeTruthy();
    expect(within(screen.getByRole("listitem", { name: "Sentinel Goose" })).getByText("Waiting to be rebuilt")).toBeTruthy();
  });

  it("is not there when the pond lists no built-ins, or cannot say", () => {
    const a = render(<BuiltInAgents prefills={fakePrefills({ report: reportOf([agentRow("greeter")]) })} variant="classic" />);
    expect(a.container.textContent).toBe("");
    a.unmount();
    const b = render(<BuiltInAgents prefills={fakePrefills({ supported: false, report: readyReport() })} variant="classic" />);
    expect(b.container.textContent).toBe("");
  });
});

describe("Rebuild all", () => {
  it("says how every agent stands, and what the pond is building", () => {
    const report = reportOf(
      [agentRow("a"), agentRow("b"), agentRow("c", { status: "building" })],
      { active: true, current: "c", done: 2, total: 3 },
    );
    render(<PrefillBar prefills={fakePrefills({ report })} />);
    expect(screen.getByRole("status").textContent).toBe("2 of 3 ready. 1 getting ready. Getting agents ready (2 of 3): c");
  });

  it("asks first, saying agents will be slower until the pond is quiet again, then rebuilds them all", async () => {
    const rebuildAll = vi.fn(async () => true);
    render(<PrefillBar prefills={fakePrefills({ report: readyReport(), rebuildAll })} />);
    fireEvent.click(screen.getByRole("button", { name: "Rebuild all" }));
    await waitFor(() => expect(rebuildAll).toHaveBeenCalled());
    expect(confirmMock).toHaveBeenCalledWith(
      expect.stringMatching(/Agents will be slower until the pond is quiet again/),
      expect.objectContaining({ confirmLabel: "Rebuild all" }),
    );
  });

  it("does nothing when the question is answered no", async () => {
    confirmMock.mockResolvedValue(false);
    const rebuildAll = vi.fn(async () => true);
    render(<PrefillBar prefills={fakePrefills({ report: readyReport(), rebuildAll })} />);
    fireEvent.click(screen.getByRole("button", { name: "Rebuild all" }));
    await waitFor(() => expect(confirmMock).toHaveBeenCalled());
    expect(rebuildAll).not.toHaveBeenCalled();
  });

  it("is off while every agent is already waiting or building, or an ask is out", () => {
    const working = reportOf([agentRow("a", { status: "queued" }), agentRow("b", { status: "building" })]);
    const a = render(<PrefillBar prefills={fakePrefills({ report: working })} />);
    expect((screen.getByRole("button", { name: "Rebuild all" }) as HTMLButtonElement).disabled).toBe(true);
    a.unmount();
    render(<PrefillBar prefills={fakePrefills({ report: readyReport(), busy: { "*": true } })} />);
    expect((screen.getByRole("button", { name: "Rebuild all" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("says what went wrong", () => {
    render(<PrefillBar prefills={fakePrefills({ report: readyReport(), errors: { "*": "Could not ask for a rebuild. offline" } })} />);
    expect(screen.getByRole("alert").textContent).toBe("Could not ask for a rebuild. offline");
  });

  it("is not there when the pond cannot say, or has no agents", () => {
    const a = render(<PrefillBar prefills={fakePrefills({ supported: false, report: readyReport() })} />);
    expect(a.container.textContent).toBe("");
    a.unmount();
    const b = render(<PrefillBar prefills={fakePrefills({ report: reportOf([]) })} />);
    expect(b.container.textContent).toBe("");
  });
});
