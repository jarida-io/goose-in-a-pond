import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";
import { ApiError } from "./types";

function okJson(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const client = () => new PondApiClient("http://localhost:4000");
const urlOf = (call: number) => String(fetchMock.mock.calls[call][0]);
const methodOf = (call: number) => fetchMock.mock.calls[call][1].method as string;
const bodyOf = (call: number) => JSON.parse(fetchMock.mock.calls[call][1].body as string) as unknown;

describe("validateRecipe()", () => {
  it("POSTs the YAML and the name, and returns the pond's answer as it is", async () => {
    const answer = {
      ok: false,
      role: null,
      errors: [{ path: "giap_role.activations[2].event.on", message: "unknown event" }],
    };
    fetchMock.mockResolvedValueOnce(okJson(answer));
    expect(await client().validateRecipe("giap_role: {}", "greeter")).toEqual(answer);
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/recipes/validate");
    expect(methodOf(0)).toBe("POST");
    expect(bodyOf(0)).toEqual({ yaml: "giap_role: {}", name: "greeter" });
  });

  it("leaves the name out for a draft that has none", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ ok: true, role: null, errors: [] }));
    await client().validateRecipe("giap_role: {}");
    expect(bodyOf(0)).toEqual({ yaml: "giap_role: {}" });
  });

  it("throws the pond's refusal for a route it does not have, so the builder can say it cannot check", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "Not Found" }, 404));
    await expect(client().validateRecipe("x")).rejects.toMatchObject({ status: 404 });
  });
});

describe("agent suggestions", () => {
  const suggestion = {
    template: "morning-brief",
    title: "Morning brief",
    description: "Tells the house the weather.",
    why: "This pond has a weather source.",
    needs: ["weather"],
    activations_summary: "Every weekday at 7:00",
  };

  it("lists them with a GET, as an array", async () => {
    fetchMock.mockResolvedValueOnce(okJson([suggestion]));
    expect(await client().listAgentSuggestions()).toEqual([suggestion]);
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/agents/suggestions");
    expect(methodOf(0)).toBe("GET");
  });

  it("reads a wrapped list too, and an empty answer as none", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ suggestions: [suggestion] }));
    expect(await client().listAgentSuggestions()).toEqual([suggestion]);
    fetchMock.mockResolvedValueOnce(okJson({}));
    expect(await client().listAgentSuggestions()).toEqual([]);
  });

  const recipe = { id: "r5", name: "morning-brief", description: "", yaml: "x", active: true };

  it("turns one on with a POST to its own route, and reads the recipe the pond answers with", async () => {
    fetchMock.mockResolvedValueOnce(okJson(recipe, 201));
    expect(await client().enableAgentSuggestion("morning-brief")).toEqual({ recipe });
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/agents/suggestions/morning-brief/enable");
    expect(methodOf(0)).toBe("POST");
  });

  it("reads a recipe wrapped as { recipe } the same way", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ recipe }, 201));
    expect(await client().enableAgentSuggestion("morning-brief")).toEqual({ recipe });
  });

  it("encodes the template name in the path", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ recipe: {} }, 201));
    await client().enableAgentSuggestion("a b/c");
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/agents/suggestions/a%20b%2Fc/enable");
  });

  it("throws a 409 when it is already on, with the status for the caller to read", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "already enabled" }, 409));
    const failure = await client()
      .enableAgentSuggestion("morning-brief")
      .catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).status).toBe(409);
  });

  it("dismisses one with a POST, and reads the empty 204 as done", async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await expect(client().dismissAgentSuggestion("morning-brief")).resolves.toBeUndefined();
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/agents/suggestions/morning-brief/dismiss");
    expect(methodOf(0)).toBe("POST");
  });
});

describe("agent start-up copies", () => {
  const report = {
    building: { active: true, current: "study-planner", done: 2, total: 5 },
    retention_days: 14,
    agents: [
      { agent: "long-task-planner", kind: "default", status: "ready", compiled_at: "2026-10-26T08:00:00Z", last_used_at: null },
      { agent: "study-planner", kind: "member", status: "building", compiled_at: null, last_used_at: null },
    ],
  };

  it("reads the status with a GET, as the pond sends it", async () => {
    fetchMock.mockResolvedValueOnce(okJson(report));
    expect(await client().getAgentPrefills()).toEqual(report);
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/agents/prefills");
    expect(methodOf(0)).toBe("GET");
  });

  it("throws a 404 on an older pond, with the status for the caller to read", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "Not Found" }, 404));
    const failure = await client()
      .getAgentPrefills()
      .catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).status).toBe(404);
  });

  it("asks for one agent to be rebuilt with a POST to its own route, and reads the 202 as done", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ queued: ["study-planner"] }, 202));
    await expect(client().rebuildAgentPrefill("study-planner")).resolves.toBeUndefined();
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/agents/study-planner/prefill/rebuild");
    expect(methodOf(0)).toBe("POST");
  });

  it("encodes the agent's name in the path", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ queued: ["a b"] }, 202));
    await client().rebuildAgentPrefill("a b/c");
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/agents/a%20b%2Fc/prefill/rebuild");
  });

  it("throws a 404 for an agent the pond does not know", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "there is no agent named gone" }, 404));
    const failure = await client()
      .rebuildAgentPrefill("gone")
      .catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).status).toBe(404);
    expect((failure as ApiError).message).toBe("there is no agent named gone");
  });

  it("asks for every agent to be rebuilt with a POST", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ queued: "all" }, 202));
    await expect(client().rebuildAllPrefills()).resolves.toBeUndefined();
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/agents/prefills/rebuild");
    expect(methodOf(0)).toBe("POST");
  });
});
