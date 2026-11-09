import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PondApiClient } from "./PondApiClient";

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

const client = () => new PondApiClient("http://localhost:4000", "tok");
const urlOf = (call: number) => String(fetchMock.mock.calls[call][0]);
const initOf = (call: number) => fetchMock.mock.calls[call][1] as RequestInit;
const bodyOf = (call: number) => JSON.parse(String(initOf(call).body));

describe("web watches", () => {
  it("lists, reads, edits and deletes by the routes the pond serves", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson([]))
      .mockResolvedValueOnce(okJson({ id: "w 1", items: [], changes: [] }))
      .mockResolvedValueOnce(okJson({ id: "w 1" }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const c = client();
    await c.listWebWatches();
    await c.getWebWatch("w 1");
    await c.editWebWatch("w 1", { paused: true, filters: { words: ["subaru"], max_price: 1_500_000 } });
    await c.deleteWebWatch("w 1");

    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/web-watches");
    expect(initOf(0).method).toBe("GET");
    expect(urlOf(1)).toBe("http://localhost:4000/api/v1/web-watches/w%201");
    expect(urlOf(2)).toBe("http://localhost:4000/api/v1/web-watches/w%201");
    expect(initOf(2).method).toBe("PATCH");
    expect(bodyOf(2)).toEqual({ paused: true, filters: { words: ["subaru"], max_price: 1_500_000 } });
    expect(initOf(3).method).toBe("DELETE");
  });
});

describe("saved cards", () => {
  it("lists by collection and kind, and reads the collections", async () => {
    fetchMock.mockResolvedValueOnce(okJson([])).mockResolvedValueOnce(okJson([])).mockResolvedValueOnce(okJson([]));
    const c = client();
    await c.listSavedItems();
    await c.listSavedItems({ collection: "Cars & vans", kind: "listing" });
    await c.listSavedCollections();
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/saved-items");
    expect(urlOf(1)).toBe("http://localhost:4000/api/v1/saved-items?collection=Cars+%26+vans&kind=listing");
    expect(urlOf(2)).toBe("http://localhost:4000/api/v1/saved-items/collections");
  });

  it("keeps a card, edits it, takes it out of its collection with null, and deletes it", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ id: "s1" }, 201))
      .mockResolvedValueOnce(okJson({ id: "s1" }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const c = client();
    await c.createSavedItem({ kind: "listing", title: "2017 Forester", price: "KES 1,450,000", link: "https://cars.example/1" });
    await c.editSavedItem("s1", { collection: null, shared: true });
    await c.deleteSavedItem("s1");

    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/saved-items");
    expect(initOf(0).method).toBe("POST");
    expect(bodyOf(0)).toEqual({ kind: "listing", title: "2017 Forester", price: "KES 1,450,000", link: "https://cars.example/1" });
    expect(initOf(1).method).toBe("PATCH");
    expect(bodyOf(1)).toEqual({ collection: null, shared: true });
    expect(urlOf(2)).toBe("http://localhost:4000/api/v1/saved-items/s1");
    expect(initOf(2).method).toBe("DELETE");
  });

  it("watches a saved listing's price", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ id: "w1" }, 201));
    await client().watchSavedItem("s1");
    expect(urlOf(0)).toBe("http://localhost:4000/api/v1/saved-items/s1/watch");
    expect(initOf(0).method).toBe("POST");
    expect(bodyOf(0)).toEqual({});
  });
});

describe("report pictures", () => {
  const SRC = "/api/v1/report-images/0123456789abcdef0123";

  it("fetches a picture the pond keeps with the bearer token, as bytes", async () => {
    fetchMock.mockResolvedValueOnce(new Response(new Blob(["x"], { type: "image/png" }), { status: 200, headers: { "Content-Type": "image/png" } }));
    const blob = await client().getReportImage(SRC);
    expect(urlOf(0)).toBe(`http://localhost:4000${SRC}`);
    expect((initOf(0).headers as Record<string, string>).Authorization).toBe("Bearer tok");
    expect(blob.type).toBe("image/png");
  });

  it.each(["https://evil.example/x.png", "/api/v1/report-images/../settings", "/api/v1/report-images/zz", "//evil.example/api/v1/report-images/0123456789abcdef"])(
    "refuses %s, which is not a picture the pond keeps",
    async (src) => {
      await expect(client().getReportImage(src)).rejects.toThrow();
      expect(fetchMock).not.toHaveBeenCalled();
    },
  );

  it("reads a report's options and pictures, dropping what is not one", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({
        markdown: "x",
        options: [
          { title: "2017 Forester", price: "KES 1,450,000", link: "https://cars.example/1", fields: [["Mileage", "98,000 km"], ["bad"]], sources: [] },
          { price: "no title" },
        ],
        images: [
          { src: SRC, credit: "Photo: A. Person, CC BY-SA 4.0", page: "https://en.wikipedia.org/wiki/Lamu", file_page: "https://commons.wikimedia.org/wiki/File:Lamu.jpg" },
          { src: "https://evil.example/pixel.png", credit: "tracker", page: "" },
          { src: SRC },
        ],
      }),
    );
    const report = await client().getLongTaskReport("t1");
    expect(report?.options).toEqual([
      expect.objectContaining({ title: "2017 Forester", price: "KES 1,450,000", fields: [["Mileage", "98,000 km"]] }),
    ]);
    expect(report?.images).toHaveLength(1);
    expect(report?.images[0]).toEqual(expect.objectContaining({ src: SRC, credit: "Photo: A. Person, CC BY-SA 4.0" }));
  });
});
