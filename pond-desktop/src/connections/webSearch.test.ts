import { describe, it, expect } from "vitest";
import { ApiError } from "../api/types";
import {
  BRAVE_DASHBOARD_URL,
  KEY_REFUSED,
  OLD_POND,
  PRIVACY_LINE,
  STATUS_LINES,
  describeProblem,
  nearbyDescription,
  readWebSearchState,
  townSentence,
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
      "When the pond searches the web, your question is sent to Brave, and the top pages are read from their sites. Pictures in the results load from Brave's image server.",
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

describe("readWebSearchState, nearby places", () => {
  const base = { provider: "brave", configured: true, status: "ok" };

  it("reads what a search would send and whether it is on", () => {
    expect(
      readWebSearchState({ ...base, local_results: { enabled: true, town: "Nairobi" } })?.local_results,
    ).toEqual({ enabled: true, town: "Nairobi" });
    expect(
      readWebSearchState({ ...base, configured: false, local_results: { enabled: false, town: null } })
        ?.local_results,
    ).toEqual({ enabled: false, town: null });
  });

  it("is silent about it from a pond that predates it, rather than saying it is off", () => {
    expect(readWebSearchState(base)).not.toHaveProperty("local_results");
    expect(readWebSearchState({ ...base, local_results: null })).not.toHaveProperty("local_results");
    expect(readWebSearchState({ ...base, local_results: { town: "Nairobi" } })).not.toHaveProperty(
      "local_results",
    );
    expect(readWebSearchState({ ...base, local_results: { enabled: "yes" } })).not.toHaveProperty(
      "local_results",
    );
  });

  it("reads a blank or mistyped town as none, since none would be sent", () => {
    for (const town of ["", "   ", 7, undefined, {}]) {
      expect(readWebSearchState({ ...base, local_results: { enabled: false, town } })?.local_results).toEqual({
        enabled: false,
        town: null,
      });
    }
    expect(
      readWebSearchState({ ...base, local_results: { enabled: false, town: "  Mombasa " } })?.local_results?.town,
    ).toBe("Mombasa");
  });
});

describe("the words of nearby places", () => {
  it("says what leaves the house, the town and no more", () => {
    expect(nearbyDescription("Nairobi", false)).toBe(
      "Sends your town (Nairobi) to Brave with each search, so it can find places near you. Your exact location is never sent.",
    );
    expect(nearbyDescription("Nairobi", true)).toBe(nearbyDescription("Nairobi", false));
    expect(nearbyDescription("Nairobi", false)).toMatch(/exact location is never sent/);
  });

  it("says to set the location first when there is no town, and not that something is sent", () => {
    expect(nearbyDescription(null, false)).toMatch(/^Needs your location first\./);
    expect(nearbyDescription(null, true)).toMatch(/so no town is sent/);
    expect(nearbyDescription(null, false)).not.toMatch(/Sends your town/);
  });

  it("adds the town to the privacy line only while a search would carry it", () => {
    expect(townSentence({ enabled: true, town: "Nairobi" })).toBe(
      "Each web search sends Nairobi to Brave, so the results can include places near you.",
    );
    expect(townSentence({ enabled: false, town: "Nairobi" })).toBeNull();
    expect(townSentence({ enabled: true, town: null })).toBeNull();
    expect(townSentence(null)).toBeNull();
  });
});
