import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";

vi.mock("../api/PondApiClient", () => ({
  api: {
    calibrateWakeWord: vi.fn(),
    removeWakeWordVariant: vi.fn(),
    resetWakeWordCalibration: vi.fn().mockResolvedValue(undefined),
  },
}));
// A take is recorded at once: the countdown and the 3.5 s window are not what is under test.
vi.mock("../modes/voice/micRecorder", () => ({
  recordFixedDuration: () => ({ done: Promise.resolve({ wav: new ArrayBuffer(8) }), abort: () => {} }),
  micErrorMessage: (e: unknown) => String(e),
}));
vi.mock("../shell", () => ({ invoke: vi.fn(), isDesktopShell: () => false }));

import { WakeWordCalibration } from "./WakeWordCalibration";
import { api } from "../api/PondApiClient";

const calibrate = vi.mocked(api.calibrateWakeWord);
const remove = vi.mocked(api.removeWakeWordVariant);

function heard(word: string, isNew: boolean, variants: string[]) {
  return {
    transcript: `Hey ${word}, good morning.`,
    normalized: `hey ${word} good morning`,
    heard: word,
    new_variant: isNew,
    all_variants: variants,
    sample_count: variants.length,
    target_count: 5,
    complete: false,
  };
}

/** Records and submits one take. */
async function take(n: number) {
  fireEvent.click(await screen.findByRole("button", { name: `Record sample ${n} of 5` }));
  fireEvent.click(await screen.findByRole("button", { name: /Submit/ }, { timeout: 4000 }));
}

describe("WakeWordCalibration", () => {
  beforeEach(() => {
    calibrate.mockReset();
    remove.mockReset();
  });
  afterEach(cleanup);

  it("takes back a take that taught the Pond a wrong variant, and only that one", async () => {
    calibrate
      .mockResolvedValueOnce(heard("goose", true, ["goose"]))
      .mockResolvedValueOnce(heard("juice", true, ["goose", "juice"]));
    remove.mockResolvedValueOnce(["goose"]);
    render(<WakeWordCalibration phrase="hey goose" onComplete={() => {}} onCancel={() => {}} />);

    await take(1);
    await take(2);
    expect(await screen.findByText("2 of 5 samples — read the sentence aloud")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Redo sample 2" }));
    await waitFor(() => expect(remove).toHaveBeenCalledWith("juice"));
    expect(await screen.findByText("1 of 5 samples — read the sentence aloud")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Record sample 2 of 5" })).toBeTruthy();
  }, 15000);

  it("takes back a take that heard a known variant without asking the Pond to forget anything", async () => {
    calibrate
      .mockResolvedValueOnce(heard("goose", true, ["goose"]))
      .mockResolvedValueOnce(heard("goose", false, ["goose"]));
    render(<WakeWordCalibration phrase="hey goose" onComplete={() => {}} onCancel={() => {}} />);

    await take(1);
    await take(2);
    fireEvent.click(await screen.findByRole("button", { name: "Redo sample 2" }));
    expect(await screen.findByText("1 of 5 samples — read the sentence aloud")).toBeTruthy();
    expect(remove).not.toHaveBeenCalled();
  }, 15000);

  it("is brought into view and focused when it opens", () => {
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    render(<WakeWordCalibration phrase="hey goose" onComplete={() => {}} onCancel={() => {}} />);
    const region = screen.getByRole("region", { name: 'Calibrate the wake word "hey goose"' });
    expect(scroll).toHaveBeenCalled();
    expect(document.activeElement).toBe(region);
  });
});
