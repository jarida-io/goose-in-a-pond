import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";
import { ApiError } from "./types";

function okJson(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

function sse(lines: string[]): Response {
  const encoder = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const line of lines) controller.enqueue(encoder.encode(line + "\n"));
      controller.close();
    },
  });
  return new Response(body, { status: 200 });
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

async function drain(stream: AsyncGenerator<unknown>): Promise<void> {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars -- draining the stream is the point; the events are not read
  for await (const _ of stream) {
    /* drain */
  }
}

function sentBody(call = 0): Record<string, unknown> {
  return JSON.parse(fetchMock.mock.calls[call][1].body as string) as Record<string, unknown>;
}

describe("getDeepResearchAvailability()", () => {
  it("asks the pond once, by GET, at the long-task route", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ available: true }));
    expect(await client().getDeepResearchAvailability()).toEqual({ available: true });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(String(fetchMock.mock.calls[0][0])).toBe("http://localhost:4000/api/v1/long-tasks/deep-research");
    expect(fetchMock.mock.calls[0][1].method).toBe("GET");
  });

  it.each(["off", "unidentified"] as const)("reads %s as the reason it is not offered", async (reason) => {
    fetchMock.mockResolvedValueOnce(okJson({ available: false, reason }));
    expect(await client().getDeepResearchAvailability()).toEqual({ available: false, reason });
  });

  it("reads a reason it does not know, or none, as off, so the item stays hidden", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ available: false, reason: "quota" }));
    expect(await client().getDeepResearchAvailability()).toEqual({ available: false, reason: "off" });
    fetchMock.mockResolvedValueOnce(okJson({ available: false }));
    expect(await client().getDeepResearchAvailability()).toEqual({ available: false, reason: "off" });
  });

  it.each([404, 405, 501])("answers null for a %i, which is an older pond", async (status) => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "Not Found" }, status));
    expect(await client().getDeepResearchAvailability()).toBeNull();
  });

  it("answers null for a page that is not JSON, or an answer that says neither", async () => {
    fetchMock.mockResolvedValueOnce(new Response("<html></html>", { status: 200, headers: { "Content-Type": "text/html" } }));
    expect(await client().getDeepResearchAvailability()).toBeNull();
    fetchMock.mockResolvedValueOnce(okJson({ hello: "world" }));
    expect(await client().getDeepResearchAvailability()).toBeNull();
    fetchMock.mockResolvedValueOnce(okJson(null));
    expect(await client().getDeepResearchAvailability()).toBeNull();
  });

  it("throws any other failure, so the caller can tell a missing route from a pond that is down", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "boom" }, 500));
    await expect(client().getDeepResearchAvailability()).rejects.toMatchObject({ status: 500 });
  });
});

describe("chatStream() and deep research", () => {
  it("carries deep_research in the body when asked, beside the resumable flag", async () => {
    fetchMock.mockResolvedValueOnce(sse(["data: [DONE]"]));
    await drain(client().chatStream("What changed in Kenya's tax rules?", undefined, undefined, undefined, undefined, true, true));
    expect(sentBody()).toMatchObject({
      message: "What changed in Kenya's tax rules?",
      resumable: true,
      deep_research: true,
    });
    expect(String(fetchMock.mock.calls[0][0])).toBe("http://localhost:4000/api/v1/chat/stream");
  });

  it("carries the chosen length with a research question, and leaves auto to the pond", async () => {
    fetchMock.mockResolvedValueOnce(sse(["data: [DONE]"]));
    await drain(client().chatStream("Who rang the bell?", undefined, undefined, undefined, undefined, true, true, "short"));
    expect(sentBody()).toMatchObject({ deep_research: true, deep_research_answer: "short" });
    fetchMock.mockResolvedValueOnce(sse(["data: [DONE]"]));
    await drain(client().chatStream("Plan our trip", undefined, undefined, undefined, undefined, true, true, "auto"));
    expect("deep_research_answer" in sentBody(1)).toBe(false);
    // A length means nothing without the research flag, so an ordinary turn never carries one.
    fetchMock.mockResolvedValueOnce(sse(["data: [DONE]"]));
    await drain(client().chatStream("hi", undefined, undefined, undefined, undefined, true, false, "report"));
    expect("deep_research_answer" in sentBody(2)).toBe(false);
  });

  it("leaves the field out of every other body", async () => {
    fetchMock.mockResolvedValueOnce(sse(["data: [DONE]"]));
    await drain(client().chatStream("hi", undefined, undefined, undefined, undefined, true));
    expect("deep_research" in sentBody()).toBe(false);
    fetchMock.mockResolvedValueOnce(sse(["data: [DONE]"]));
    await drain(client().chatStream("hi", undefined, undefined, undefined, undefined, true, false));
    expect("deep_research" in sentBody(1)).toBe(false);
  });

  it("hands back the refusal's reason in the error body, where the pond puts it", async () => {
    const refusal = { error: "Deep research is switched off on this pond.", reason: "deep_research_off" };
    fetchMock.mockResolvedValueOnce(okJson(refusal, 409));
    const gen = client().chatStream("q", undefined, undefined, undefined, undefined, true, true);
    const err = await gen.next().then(
      () => null,
      (e: unknown) => e,
    );
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(409);
    expect((err as ApiError).message).toBe("Deep research is switched off on this pond.");
    expect((err as ApiError).body).toEqual(refusal);
  });
});
