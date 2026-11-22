import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";
import { ApiError } from "./types";

function okJson(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

const noContent = () => new Response(null, { status: 204 });

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const client = () => new PondApiClient("http://localhost:4000", "tok");
const urlOf = (call: number) => String(fetchMock.mock.calls[call][0]).replace("http://localhost:4000", "");
const methodOf = (call: number) => (fetchMock.mock.calls[call][1] as RequestInit).method;
const bodyOf = (call: number) => JSON.parse(String((fetchMock.mock.calls[call][1] as RequestInit).body));

describe("anchors and deliverables", () => {
  it("reads, adds, confirms, dismisses, stops and shares by the routes the pond serves", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ anchors: [{ id: "a 1" }] }))
      .mockResolvedValueOnce(okJson({ id: "a 1" }))
      .mockResolvedValueOnce(okJson({ id: "a 2" }, 201))
      .mockResolvedValueOnce(okJson({ id: "a 1" }))
      .mockResolvedValueOnce(noContent())
      .mockResolvedValueOnce(noContent())
      .mockResolvedValueOnce(okJson({ id: "a 1" }));
    const c = client();
    expect(await c.listAnchors()).toEqual([{ id: "a 1" }]);
    await c.getAnchor("a 1");
    await c.addAnchor({ kind: "trip", title: "Diani trip", place: "Diani" });
    await c.confirmAnchor("a 1");
    await c.dismissAnchor("a 1");
    await c.stopAnchor("a 1", { reasons: ["too_generic"], never: true });
    await c.shareAnchor("a 1", ["p-amina"]);

    expect(urlOf(0)).toBe("/api/v1/me/anchors");
    expect(urlOf(1)).toBe("/api/v1/me/anchors/a%201");
    expect([methodOf(2), urlOf(2), bodyOf(2)]).toEqual(["POST", "/api/v1/me/anchors", { kind: "trip", title: "Diani trip", place: "Diani" }]);
    expect([methodOf(3), urlOf(3)]).toEqual(["POST", "/api/v1/me/anchors/a%201/confirm"]);
    expect([methodOf(4), urlOf(4)]).toEqual(["POST", "/api/v1/me/anchors/a%201/dismiss"]);
    expect([methodOf(5), urlOf(5), bodyOf(5)]).toEqual(["POST", "/api/v1/me/anchors/a%201/stop", { reasons: ["too_generic"], never: true }]);
    expect([methodOf(6), urlOf(6), bodyOf(6)]).toEqual(["PUT", "/api/v1/me/anchors/a%201/shares", { profile_ids: ["p-amina"] }]);
  });

  it("refreshes, rejects, marks seen and downloads a deliverable", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ id: "d1" }))
      .mockResolvedValueOnce(okJson({ state: "queued" }, 202))
      .mockResolvedValueOnce(noContent())
      .mockResolvedValueOnce(noContent())
      .mockResolvedValueOnce(
        new Response("# Plan", {
          status: 200,
          headers: { "Content-Type": "text/markdown", "Content-Disposition": 'attachment; filename="plan-for-diani.md"' },
        }),
      );
    const c = client();
    await c.getDeliverable("d1");
    expect(await c.refreshDeliverable("d1")).toEqual({ state: "queued" });
    await c.rejectDeliverable("d1", { verb: "never", reasons: [], text: "No beach holidays" });
    await c.markDeliverableSeen("d1");
    const file = await c.getDeliverableFile("d1");

    expect(urlOf(0)).toBe("/api/v1/me/deliverables/d1");
    expect([methodOf(1), urlOf(1)]).toEqual(["POST", "/api/v1/me/deliverables/d1/refresh"]);
    expect(bodyOf(2)).toEqual({ verb: "never", reasons: [], text: "No beach holidays" });
    expect(urlOf(3)).toBe("/api/v1/me/deliverables/d1/seen");
    expect(urlOf(4)).toBe("/api/v1/me/deliverables/d1/file");
    expect(file.filename).toBe("plan-for-diani.md");
    expect(await file.blob.text()).toBe("# Plan");
  });

  it("keeps the pond's refusal, so a screen can tell an unknown caller", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "who_are_you: this phone is not linked to a member yet" }, 409));
    const refused = await client().listAnchors().catch((e: unknown) => e);
    expect(refused).toBeInstanceOf(ApiError);
    expect((refused as ApiError).status).toBe(409);
    expect((refused as ApiError).message).toMatch(/^who_are_you/);
  });
});

describe("questions, facts, the don't-list, specialists and the engine", () => {
  it("answers and skips questions one at a time", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ batches: [] })).mockResolvedValueOnce(noContent()).mockResolvedValueOnce(noContent());
    const c = client();
    expect(await c.listQuestionBatches()).toEqual([]);
    await c.answerQuestion("q1", { choices: [], text: "About KSh 120,000" });
    await c.skipQuestion("q2");
    expect(urlOf(0)).toBe("/api/v1/me/questions");
    expect([urlOf(1), bodyOf(1)]).toEqual(["/api/v1/me/questions/q1/answer", { choices: [], text: "About KSh 120,000" }]);
    expect(urlOf(2)).toBe("/api/v1/me/questions/q2/skip");
  });

  it("reads and changes the fact sheet and the don't-list", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ facts: [], limit: 100 }))
      .mockResolvedValueOnce(okJson({ facts: [], limit: 100 }))
      .mockResolvedValueOnce(okJson({ id: "f1" }))
      .mockResolvedValueOnce(noContent())
      .mockResolvedValueOnce(okJson({ state: "queued" }, 202))
      .mockResolvedValueOnce(okJson({ rules: [] }))
      .mockResolvedValueOnce(okJson({ rules: [{ id: "r1" }] }))
      .mockResolvedValueOnce(okJson({ rules: [] }));
    const c = client();
    await c.listFacts();
    await c.addFact("food", "Vegetarian");
    await c.changeFact("f1", { pinned: true });
    await c.removeFact("f1");
    await c.rebuildFacts();
    await c.listDontRules();
    expect(await c.addDontRule("Beach holidays", "never")).toEqual([{ id: "r1" }]);
    expect(await c.removeDontRule("r1")).toEqual([]);

    expect([methodOf(1), urlOf(1), bodyOf(1)]).toEqual(["POST", "/api/v1/me/facts", { category: "food", fact: "Vegetarian" }]);
    expect([methodOf(2), urlOf(2), bodyOf(2)]).toEqual(["PATCH", "/api/v1/me/facts/f1", { pinned: true }]);
    expect([methodOf(3), urlOf(3)]).toEqual(["DELETE", "/api/v1/me/facts/f1"]);
    expect([methodOf(4), urlOf(4)]).toEqual(["POST", "/api/v1/me/facts/rebuild"]);
    expect(urlOf(5)).toBe("/api/v1/me/dont");
    expect(bodyOf(6)).toEqual({ rule: "Beach holidays", strength: "never" });
    expect([methodOf(7), urlOf(7)]).toEqual(["DELETE", "/api/v1/me/dont/r1"]);
  });

  it("switches specialists, reads the engine log and withdraws an intention", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ agents: [] }))
      .mockResolvedValueOnce(okJson({ agents: [{ id: "budget" }] }))
      .mockResolvedValueOnce(okJson({ jobs: [] }))
      .mockResolvedValueOnce(okJson({ intentions: [] }))
      .mockResolvedValueOnce(noContent());
    const c = client();
    await c.listSpecialists();
    await c.switchSpecialist("budget", true);
    await c.listEngineJobs(20);
    await c.listIntentions();
    await c.withdrawIntention("i1");

    expect(urlOf(0)).toBe("/api/v1/me/agents");
    expect([methodOf(1), urlOf(1), bodyOf(1)]).toEqual(["PUT", "/api/v1/me/agents/budget", { on: true }]);
    expect(urlOf(2)).toBe("/api/v1/me/engine/jobs?limit=20");
    expect(urlOf(3)).toBe("/api/v1/me/intentions?state=waiting");
    expect([methodOf(4), urlOf(4)]).toEqual(["DELETE", "/api/v1/me/intentions/i1"]);
  });
});
