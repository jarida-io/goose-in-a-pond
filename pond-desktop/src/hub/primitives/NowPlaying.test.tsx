import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";

const store = vi.hoisted(() => ({
  nowPlaying: {
    track: "Blue Train",
    artist: "John Coltrane",
    elapsed: 0.4,
    hue: 260,
    connected: true,
    playing: true,
  } as Record<string, unknown>,
}));

vi.mock("../state/hubDataStore", () => ({
  useHomeData: () => ({ nowPlaying: store.nowPlaying }),
  controlNowPlaying: vi.fn(),
  refreshNowPlaying: vi.fn(),
}));

import { NowPlaying } from "./NowPlaying";
import { controlNowPlaying, refreshNowPlaying } from "../state/hubDataStore";

afterEach(cleanup);
beforeEach(() => vi.clearAllMocks());

describe("NowPlaying", () => {
  it("offers transport controls while playback is healthy", () => {
    render(<NowPlaying />);
    expect(screen.getByLabelText("Pause")).toBeTruthy();
    expect(screen.getByLabelText("Next")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Try again/ })).toBeNull();

    fireEvent.click(screen.getByLabelText("Pause"));
    expect(vi.mocked(controlNowPlaying)).toHaveBeenCalledWith("pause");
  });

  // Polling has stopped on this refusal, so nothing else will bring the widget back.
  it("replaces the dead transport controls with a way to ask again", () => {
    store.nowPlaying = {
      track: "Spotify not authorised",
      artist: "Sign in to Spotify again.",
      elapsed: 0,
      hue: 260,
      connected: true,
      playing: false,
      error: "unauthorized",
      message: "Sign in to Spotify again.",
    };
    render(<NowPlaying />);

    expect(screen.queryByLabelText("Pause")).toBeNull();
    expect(screen.queryByLabelText("Next")).toBeNull();
    expect(screen.queryByLabelText("Previous")).toBeNull();

    const retry = screen.getByRole("button", { name: /Try again/ });
    expect((retry as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(retry);
    expect(vi.mocked(refreshNowPlaying)).toHaveBeenCalled();
    // Asking again is not a playback command.
    expect(vi.mocked(controlNowPlaying)).not.toHaveBeenCalled();
  });

  it("carries the server's explanation rather than a generic failure", () => {
    store.nowPlaying = {
      track: "Spotify unavailable",
      artist: "Spotify is rate-limiting requests. Playback should reappear shortly.",
      elapsed: 0,
      hue: 260,
      connected: true,
      playing: false,
      error: "rate_limited",
      message: "Spotify is rate-limiting requests. Playback should reappear shortly.",
    };
    render(<NowPlaying />);
    expect(screen.getByText(/rate-limiting/)).toBeTruthy();
  });
});
