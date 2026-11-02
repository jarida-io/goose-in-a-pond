// Shapes the prefill status route answers with, for tests: the agents, a build, and a stand-in hook state.
import type { PrefillAgent, PrefillBuild, PrefillReport } from "../api/types";
import type { PrefillsState } from "./useAgentPrefills";

export const HOUR = 3_600_000;
export const DAY = 24 * HOUR;

/** A time `ms` before now, as the pond writes one. */
export function ago(ms: number): string {
  return new Date(Date.now() - ms).toISOString();
}

export function agentRow(agent: string, over: Partial<PrefillAgent> = {}): PrefillAgent {
  const builtIn = agent.startsWith("long-task-");
  return {
    agent,
    kind: builtIn ? "default" : "member",
    status: "ready",
    compiled_at: ago(2 * DAY),
    last_used_at: ago(3 * HOUR),
    ...over,
  };
}

export const IDLE_BUILD: PrefillBuild = { active: false, current: null, done: 0, total: 0 };

export function reportOf(agents: PrefillAgent[], building: Partial<PrefillBuild> = {}): PrefillReport {
  return { building: { ...IDLE_BUILD, ...building }, retention_days: 14, agents };
}

/** The three built-ins and one agent of the household's, all ready. */
export function readyReport(member = "greeter"): PrefillReport {
  return reportOf([
    agentRow("long-task-planner"),
    agentRow("long-task-worker"),
    agentRow("long-task-verifier"),
    agentRow(member),
  ]);
}

export function fakePrefills(over: Partial<PrefillsState> = {}): PrefillsState {
  return {
    report: null,
    supported: true,
    busy: {},
    errors: {},
    refresh: () => {},
    rebuild: async () => true,
    rebuildAll: async () => true,
    ...over,
  };
}
