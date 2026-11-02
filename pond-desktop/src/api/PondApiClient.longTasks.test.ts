import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";
import { doneTask, TASK_ID } from "../longtasks/longTaskFixtures";

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

describe("listLongTasks()", () => {
  it("asks for one conversation's tasks by its session", async () => {
    fetchMock.mockResolvedValueOnce(okJson([doneTask()]));
    expect(await client().listLongTasks("sess-1")).toEqual([doneTask()]);
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/long-tasks?session_id=sess-1");
    expect(methodOf(0)).toBe("GET");
  });

  it("encodes the session id", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));
    await client().listLongTasks("a b&c");
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/long-tasks?session_id=a%20b%26c");
  });

  it("asks for every task the caller reaches when there is no session", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));
    await client().listLongTasks(null);
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/long-tasks");
  });

  it("reads an answer that is not a list as none", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "unexpected" }));
    expect(await client().listLongTasks("sess-1")).toEqual([]);
  });

  it("throws the pond's refusal for a route it does not have", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "Not Found" }, 404));
    await expect(client().listLongTasks("sess-1")).rejects.toMatchObject({ status: 404 });
  });
});

describe("getLongTask() and getLongTaskArtifact()", () => {
  it("reads one task", async () => {
    fetchMock.mockResolvedValueOnce(okJson(doneTask()));
    expect(await client().getLongTask(TASK_ID)).toEqual(doneTask());
    expect(urlOf(0)).toBe(`http://localhost:4000/api/v1/long-tasks/${TASK_ID}`);
    expect(methodOf(0)).toBe("GET");
  });

  it("throws a 404 for a task the caller does not reach", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "Not Found" }, 404));
    await expect(client().getLongTask(TASK_ID)).rejects.toMatchObject({ status: 404 });
  });

  it("reads an artifact's body", async () => {
    const body = { id: "art-1", title: "Trip plan", kind: "result", summary: "Three days.", body: "# Plan" };
    fetchMock.mockResolvedValueOnce(okJson(body));
    expect(await client().getLongTaskArtifact(TASK_ID, "art-1")).toEqual(body);
    expect(urlOf(0)).toBe(`http://localhost:4000/api/v1/long-tasks/${TASK_ID}/artifacts/art-1`);
  });

  it("encodes both ids in an artifact's address", async () => {
    fetchMock.mockResolvedValueOnce(okJson({}));
    await client().getLongTaskArtifact("a/b", "c d");
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/long-tasks/a%2Fb/artifacts/c%20d");
  });
});

describe("cancelLongTask()", () => {
  it("POSTs to the task's cancel route", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ ok: true }));
    await client().cancelLongTask(TASK_ID);
    expect(urlOf(0)).toBe(`http://localhost:4000/api/v1/long-tasks/${TASK_ID}/cancel`);
    expect(methodOf(0)).toBe("POST");
  });

  it("throws a 409 for a task that has already ended", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "already ended" }, 409));
    await expect(client().cancelLongTask(TASK_ID)).rejects.toMatchObject({ status: 409 });
  });

  it("throws a 404 for a task the caller does not reach", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "Not Found" }, 404));
    await expect(client().cancelLongTask(TASK_ID)).rejects.toMatchObject({ status: 404 });
  });
});
