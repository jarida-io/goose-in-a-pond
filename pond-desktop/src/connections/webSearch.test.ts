import { describe, it, expect } from "vitest";
import { ApiError } from "../api/types";
import {
  BRAVE_DASHBOARD_URL,
  KEY_REFUSED,
  OLD_POND,
  PRIVACY_LINE,
  STATUS_LINES,
  describeProblem,
  readWebSearchState,
} from "./webSearch";

describe("the wording of web search", () => {
  it("says each status in the words of section 8.5", () => {
    expect(Object.fromEntries(Object.entries(STATUS_LINES).map(([k, v]) => [k, v.label]))).toEqual({
      ok: "On, searching with Brave",
      not_configured: "Off",
      rejected: "Brave did not accept the key",
      quota: "This month's free searches are used up",
      unverified: "Saved, not yet checked",
    });
  });

  it("states the privacy line and the dashboard exactly", () => {
    expect(PRIVACY_LINE).toBe(
      "When the pond searches the web, your question is sent to Brave, and the top pages are read from their sites.",
    );
    expect(BRAVE_DASHBOARD_URL).toBe("https://api-dashboard.search.brave.com");
    expect(KEY_REFUSED).toBe("Brave did not accept this key.");
  });
});

describe("readWebSearchState", () => {
  it("reads a reply of the contract", () => {
    expect(readWebSearchState({ provider: "brave", configured: true, status: "quota" })).toEqual({
      provider: "brave",
      configured: true,
      status: "quota",
    });
  });

  it("refuses what is not a reply, such as the empty body `request` hands back", () => {
    expect(readWebSearchState(undefined)).toBeNull();
    expect(readWebSearchState(null)).toBeNull();
    expect(readWebSearchState("<!doctype html>")).toBeNull();
    expect(readWebSearchState({ status: "ok" })).toBeNull();
  });

  it("never lets configured and status disagree", () => {
    expect(readWebSearchState({ configured: false, status: "ok" })?.status).toBe("not_configured");
    expect(readWebSearchState({ configured: true, status: "not_configured" })?.status).toBe(
      "unverified",
    );
  });

  it("reads a status this build does not know from configured", () => {
    expect(readWebSearchState({ configured: true, status: "rate_limited" })?.status).toBe(
      "unverified",
    );
    expect(readWebSearchState({ configured: false, status: "rate_limited" })?.status).toBe(
      "not_configured",
    );
  });
});

describe("describeProblem", () => {
  it("says a refused key is refused, on save only", () => {
    expect(describeProblem(new ApiError(422, "Brave did not accept this key."), "save")).toBe(
      KEY_REFUSED,
    );
    expect(describeProblem(new ApiError(422, "x"), "remove")).not.toBe(KEY_REFUSED);
  });

  it("says an older pond is older, for a route it does not have", () => {
    expect(describeProblem(new ApiError(404, "Not Found"), "read")).toBe(OLD_POND);
    expect(describeProblem(new ApiError(405, "Method Not Allowed"), "save")).toBe(OLD_POND);
  });

  it("never shows a server code or a transport error", () => {
    expect(describeProblem(new ApiError(500, "no_secret_store", "no_secret_store"), "save")).toBe(
      "Could not save the key. Try again in a moment.",
    );
    expect(describeProblem(new TypeError("Failed to fetch"), "read")).toBe(
      "Could not read the web search setting. Try again in a moment.",
    );
    expect(describeProblem(new ApiError(408, "Request timed out"), "remove")).toBe(
      "Could not remove the key. Try again in a moment.",
    );
  });
});
