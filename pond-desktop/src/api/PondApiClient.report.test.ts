import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";
import { ApiError } from "./types";
import { TASK_ID } from "../longtasks/longTaskFixtures";

function okJson(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

function file(body: string, type: string, disposition?: string): Response {
  return new Response(body, {
    status: 200,
    headers: { "Content-Type": type, ...(disposition ? { "Content-Disposition": disposition } : {}) },
  });
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

const REPORT = {
  id: TASK_ID,
  title: "Kenya tax rules",
  state: "completed",
  kind: "research",
  markdown: "# Kenya tax rules\n\nSummary.\n\n## Sources\n\n- [KRA](https://www.kra.go.ke/)\n",
  sections: 3,
  sources: [{ title: "KRA", url: "https://www.kra.go.ke/" }],
  calendar_events: 2,
};

describe("getLongTaskReport()", () => {
  it("asks for the report of one task, by GET", async () => {
    fetchMock.mockResolvedValueOnce(okJson(REPORT));
    expect(await client().getLongTaskReport(TASK_ID)).toEqual(REPORT);
    expect(urlOf(0)).toBe(`http://localhost:4000/api/v1/long-tasks/${TASK_ID}/report`);
    expect(methodOf(0)).toBe("GET");
  });

  it("encodes the id", async () => {
    fetchMock.mockResolvedValueOnce(okJson(REPORT));
    await client().getLongTaskReport("a/b c");
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/long-tasks/a%2Fb%20c/report");
  });

  it("fills in what an answer leaves out, and drops a source that is not a title and an address", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({ markdown: "Text", title: "  ", sources: [{ title: "ok", url: "https://a.co" }, { url: 3 }, "nope", null] }),
    );
    expect(await client().getLongTaskReport(TASK_ID)).toEqual({
      id: TASK_ID,
      title: "Report",
      state: undefined,
      kind: "general",
      markdown: "Text",
      sections: 0,
      sources: [{ title: "ok", url: "https://a.co" }],
      calendar_events: 0,
    });
  });

  it.each([
    ["a 404 that says nothing", () => new Response("", { status: 404 })],
    ["a 404 whose JSON has no error sentence", () => okJson({}, 404)],
    ["a 404 that is a page", () => new Response("<html>Not found</html>", { status: 404, headers: { "Content-Type": "text/html" } })],
    ["a 405", () => okJson({ error: "Method Not Allowed" }, 405)],
    ["a page that is not JSON", () => new Response("<html></html>", { status: 200, headers: { "Content-Type": "text/html" } })],
    ["JSON that is not a report", () => okJson({ hello: "world" })],
    ["JSON null", () => okJson(null)],
  ])("answers null for %s: an older pond", async (_name, response) => {
    fetchMock.mockResolvedValueOnce(response());
    expect(await client().getLongTaskReport(TASK_ID)).toBeNull();
  });

  it("throws the pond's own refusal: nobody identified, or a task it will not show", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "nobody identified" }, 403));
    await expect(client().getLongTaskReport(TASK_ID)).rejects.toMatchObject({ status: 403, message: "nobody identified" });
    fetchMock.mockResolvedValueOnce(okJson({ error: "task not found" }, 404));
    await expect(client().getLongTaskReport(TASK_ID)).rejects.toMatchObject({ status: 404, message: "task not found" });
  });

  it("throws any other failure", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "boom" }, 500));
    await expect(client().getLongTaskReport(TASK_ID)).rejects.toBeInstanceOf(ApiError);
  });
});

describe("getLongTaskExport()", () => {
  it.each(["markdown", "html", "ics"] as const)("asks for the %s file at its own address, by GET", async (format) => {
    fetchMock.mockResolvedValueOnce(file("body", "text/plain"));
    await client().getLongTaskExport(TASK_ID, format);
    expect(urlOf(0)).toBe(`http://localhost:4000/api/v1/long-tasks/${TASK_ID}/export?format=${format}`);
    expect(methodOf(0)).toBe("GET");
  });

  it("hands back the file's bytes and the name the pond gave it", async () => {
    fetchMock.mockResolvedValueOnce(file("# Plan", "text/markdown", 'attachment; filename="kenya-tax-rules.md"'));
    const { blob, filename } = await client().getLongTaskExport(TASK_ID, "markdown");
    expect(await blob.text()).toBe("# Plan");
    expect(filename).toBe("kenya-tax-rules.md");
  });

  it("has no name when the page cannot read the header", async () => {
    fetchMock.mockResolvedValueOnce(file("<p>x</p>", "text/html"));
    expect((await client().getLongTaskExport(TASK_ID, "html")).filename).toBeNull();
  });

  it("throws a calendar file's 404, which means no dated events, and a bad format's 400", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ error: "this report has no dated events for a calendar" }, 404));
    await expect(client().getLongTaskExport(TASK_ID, "ics")).rejects.toMatchObject({ status: 404 });
    fetchMock.mockResolvedValueOnce(okJson({ error: "format is markdown, html or ics" }, 400));
    await expect(client().getLongTaskExport(TASK_ID, "html")).rejects.toMatchObject({ status: 400 });
  });
});
