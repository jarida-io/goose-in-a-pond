import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getContextIndexHealth: vi.fn(),
    getActiveRoles: vi.fn(),
    activateModel: vi.fn(),
    getDownloadProgress: vi.fn(),
  },
}));

import { api } from "../api/PondApiClient";
import { useEmbedderHealth } from "./useEmbedderHealth";
import { memoryRoles } from "../sections/models/fixtures";
import type { ContextIndexHealth, EmbedderState } from "../api/types";

function health(state: EmbedderState, matching = 95, reason: string | null = null): ContextIndexHealth {
  return {
    indexed: true, model_id: "embeddinggemma-2-256", dims: 256, rows: 95, matching,
    mismatched: 0, missing: 95 - matching, coverage: matching / 95, corpora: [],
    embedder: { state, model_id: "embeddinggemma-2-256", reason },
  };
}

async function settle() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}
async function wait(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  vi.mocked(api.getActiveRoles).mockResolvedValue(memoryRoles("embeddinggemma-2"));
  vi.mocked(api.activateModel).mockResolvedValue(undefined as never);
  vi.mocked(api.getDownloadProgress).mockResolvedValue({ downloads: [] });
});
afterEach(() => vi.useRealTimers());

describe("useEmbedderHealth", () => {
  it("reads once and asks no more of a settled pond", async () => {
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(health("ready"));
    const { result } = renderHook(() => useEmbedderHealth());
    expect(result.current.loading).toBe(true);
    await settle();
    expect(result.current.loading).toBe(false);
    expect(result.current.health?.embedder?.state).toBe("ready");

    await wait(60_000);
    expect(api.getContextIndexHealth).toHaveBeenCalledTimes(1);
  });

  it("keeps asking while the model loads and the store is re-read, then stops", async () => {
    vi.mocked(api.getContextIndexHealth)
      .mockResolvedValueOnce(health("loading", 0))
      .mockResolvedValueOnce(health("ready", 40))
      .mockResolvedValue(health("ready", 95));
    const { result } = renderHook(() => useEmbedderHealth());
    await settle();
    expect(result.current.health?.embedder?.state).toBe("loading");

    await wait(3_000);
    expect(result.current.health?.matching).toBe(40);
    await wait(3_000);
    expect(result.current.health?.matching).toBe(95);

    await wait(60_000);
    expect(api.getContextIndexHealth).toHaveBeenCalledTimes(3);
  });

  it("looks at the downloads each time it looks, so a file that starts late is seen", async () => {
    const onWatch = vi.fn();
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(
      health("failed", 0, "x.gguf is not on disk; the models pipeline is downloading it"),
    );
    renderHook(() => useEmbedderHealth({ onWatch }));
    await settle();
    expect(onWatch).not.toHaveBeenCalled();
    await wait(3_000);
    expect(onWatch).toHaveBeenCalledTimes(1);
  });

  it("asks the tracker whether a missing file is on its way, and only then", async () => {
    const missing = health("failed", 0, "x.gguf is not on disk; the models pipeline is downloading it");
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(missing);
    vi.mocked(api.getDownloadProgress).mockResolvedValue({
      downloads: [{ filename: "x.gguf", category: "embedding", downloaded_bytes: 1, total_bytes: 9, status: "downloading" }],
    });
    const { result, unmount } = renderHook(() => useEmbedderHealth());
    await settle();
    expect(result.current.downloading).toBe(true);
    unmount();

    // A stopped transfer leaves a cancelled entry, which is not on its way.
    vi.mocked(api.getDownloadProgress).mockResolvedValue({
      downloads: [{ filename: "x.gguf", category: "embedding", downloaded_bytes: 0, total_bytes: 9, status: "cancelled" }],
    });
    const again = renderHook(() => useEmbedderHealth());
    await settle();
    expect(again.result.current.downloading).toBe(false);

    // A model that is ready never asks.
    vi.mocked(api.getDownloadProgress).mockClear();
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(health("ready"));
    renderHook(() => useEmbedderHealth());
    await settle();
    expect(api.getDownloadProgress).not.toHaveBeenCalled();
  });

  it("does not poll a load that failed for good", async () => {
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(health("failed", 0, "out of memory"));
    renderHook(() => useEmbedderHealth());
    await settle();
    await wait(30_000);
    expect(api.getContextIndexHealth).toHaveBeenCalledTimes(1);
  });

  it("says why it could not read, and keeps what it last knew", async () => {
    vi.mocked(api.getContextIndexHealth).mockRejectedValue(new Error("connection refused"));
    const { result } = renderHook(() => useEmbedderHealth());
    await settle();
    expect(result.current.error).toContain("connection refused");
    expect(result.current.health).toBeNull();
    expect(result.current.loading).toBe(false);
  });

  it("survives a partial api, which throws before it can reject", async () => {
    vi.mocked(api.getContextIndexHealth).mockImplementation(() => {
      throw new TypeError("not a function");
    });
    const { result } = renderHook(() => useEmbedderHealth());
    await settle();
    expect(result.current.error).toContain("not a function");
  });

  // A retry must never clear the store: nothing is lost when a model fails to load.
  it("retries by choosing the same model again, which clears nothing", async () => {
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(health("failed", 0, "out of memory"));
    const { result } = renderHook(() => useEmbedderHealth());
    await settle();
    await act(async () => {
      await result.current.retry();
    });
    expect(api.activateModel).toHaveBeenCalledWith("embedding", "embeddinggemma-2", "embedding");
    expect(api.getContextIndexHealth).toHaveBeenCalledTimes(2);
    expect(result.current.retrying).toBe(false);
  });

  it("rejects with the pond's words when asking again fails", async () => {
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(health("failed", 0, "out of memory"));
    vi.mocked(api.activateModel).mockRejectedValue(new Error("registry not available"));
    const { result } = renderHook(() => useEmbedderHealth());
    await settle();
    let thrown: unknown;
    await act(async () => {
      await result.current.retry().catch((e) => { thrown = e; });
    });
    expect((thrown as Error).message).toBe("registry not available");
    expect(result.current.retrying).toBe(false);
  });
});
