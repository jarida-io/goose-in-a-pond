import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, /* fireEvent, */ cleanup /* , waitFor */ } from "@testing-library/react";

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    listModels: vi.fn(),
    getActiveRoles: vi.fn(),
    activateModel: vi.fn(),
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    getVisionStatus: vi.fn(),
  },
}));

import { ModelsDetail } from "./Models";
import { api } from "../../../api/PondApiClient";

const MODELS = [
  {
    id: "gguf/gemma-4-E2B-it-Q4_K_M",
    provider: "gguf",
    name: "gemma-4-E2B-it-Q4_K_M",
    is_active: true,
    downloaded: true,
    recommended_role: "chat",
  },
];

const ROLES = { chat: { provider: "gguf", model: "gemma-4-E2B-it-Q4_K_M" }, tool: null, asr: null, tts: null, embedding: null };

/** Draw-only settings reply -- the fields this view actually reads. */
function modelsSettings(overrides: Record<string, unknown> = {}) {
  return { ...overrides };
}

// Speculative decoding was taken out of the llama.cpp engine on 2026-09-24 (goose 743649d98),
// so the Speed card these tests drove is commented out; they are kept, commented, to restore
// with it.
// async function renderModels() {
//   render(<ModelsDetail go={() => {}} />);
//   await screen.findByText("Language models");
//   // Let the (separately-loaded) settings promise land too.
//   await screen.findByText("Guess ahead with a helper model");
// }
//
// /** The Speed row's switch — Voice.test.tsx's pattern, found via its own row
//  *  rather than by index. */
// function speedSwitch(): HTMLButtonElement {
//   const label = screen.getByText("Guess ahead with a helper model");
//   const row = label.closest(".srow");
//   if (!row) throw new Error("speed row not found");
//   const btn = row.querySelector("button.htoggle");
//   if (!btn) throw new Error("speed row has no switch");
//   return btn as HTMLButtonElement;
// }

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.listModels).mockResolvedValue(MODELS as never);
  vi.mocked(api.getActiveRoles).mockResolvedValue(ROLES as never);
  vi.mocked(api.activateModel).mockResolvedValue(undefined as never);
  vi.mocked(api.getSettings).mockResolvedValue(modelsSettings() as never);
  vi.mocked(api.updateSettings).mockResolvedValue({} as never);
  vi.mocked(api.getVisionStatus).mockResolvedValue({
    model: "", state: { kind: "unknown" }, size_bytes: null, message: null,
  } as never);
});

describe("Hub Models, while speculative decoding is out of the engine", () => {
  it("offers no guess-ahead switch and never reads settings for one", async () => {
    render(<ModelsDetail go={() => {}} />);
    await screen.findByText("Language models");
    expect(screen.queryByText("Guess ahead with a helper model")).toBeNull();
    expect(api.getSettings).not.toHaveBeenCalled();
  });
});

// Speculative decoding was taken out of the llama.cpp engine on 2026-09-24 (goose 743649d98),
// so the Speed card these tests drove is commented out; they are kept, commented, to restore
// with it.
// // The hub Toggle seeds its own state from its `on` prop once (controls.tsx),
// // and settings arrive a render after mount — these assert what is DRAWN
// // after the async load, not merely that a control exists. Same regression
// // Voice.test.tsx's thinking-tone suite guards.
// describe("Hub Models — guess ahead with a helper model", () => {
//   it("draws ON when the key is absent", async () => {
//     // The field defaults ON in Rust, so a settings row written before this
//     // field existed carries no such key.
//     vi.mocked(api.getSettings).mockResolvedValue(modelsSettings() as never);
//     await renderModels();
//
//     expect(speedSwitch().getAttribute("aria-pressed")).toBe("true");
//   });
//
//   it("draws OFF when the stored setting is off", async () => {
//     vi.mocked(api.getSettings).mockResolvedValue(
//       modelsSettings({ speculative_decoding_enabled: false }) as never,
//     );
//     await renderModels();
//
//     await waitFor(() => {
//       expect(speedSwitch().getAttribute("aria-pressed")).toBe("false");
//     });
//   });
//
//   it("writes speculative_decoding_enabled and nothing else", async () => {
//     vi.mocked(api.getSettings).mockResolvedValue(modelsSettings() as never);
//     await renderModels();
//
//     fireEvent.click(speedSwitch());
//
//     await waitFor(() => {
//       expect(vi.mocked(api.updateSettings)).toHaveBeenCalledWith({
//         speculative_decoding_enabled: false,
//       });
//     });
//   });
//
//   it("reverts and flashes the failure when the save is refused", async () => {
//     vi.mocked(api.getSettings).mockResolvedValue(modelsSettings() as never);
//     vi.mocked(api.updateSettings).mockRejectedValueOnce(new Error("offline"));
//     await renderModels();
//
//     fireEvent.click(speedSwitch());
//
//     await waitFor(() => {
//       expect(screen.getByText(/Could not turn guessing ahead off: offline\. It is still on; try again\./)).toBeTruthy();
//     });
//     expect(speedSwitch().getAttribute("aria-pressed")).toBe("true");
//   });
//
//   it("has an accessible name", async () => {
//     await renderModels();
//     expect(speedSwitch().getAttribute("aria-label")).toBe("Guess ahead with a helper model");
//   });
//
//   it("does not adopt the PUT echo — the optimistic value stands even if the server answers differently", async () => {
//     vi.mocked(api.getSettings).mockResolvedValue(modelsSettings() as never);
//     // A stale echo naming the OLD value, as a fast second click or a lagging
//     // response could produce.
//     vi.mocked(api.updateSettings).mockResolvedValue({ speculative_decoding_enabled: true } as never);
//     await renderModels();
//
//     fireEvent.click(speedSwitch());
//
//     await waitFor(() => {
//       expect(vi.mocked(api.updateSettings)).toHaveBeenCalledWith({
//         speculative_decoding_enabled: false,
//       });
//     });
//     expect(speedSwitch().getAttribute("aria-pressed")).toBe("false");
//   });
//
//   it("renders no switch, and no guessed value, when settings cannot be read", async () => {
//     vi.mocked(api.getSettings).mockRejectedValue(new Error("offline"));
//     render(<ModelsDetail go={() => {}} />);
//
//     // The model list still renders — a settings failure must not blank the
//     // whole page into the offline view (they are loaded separately).
//     await screen.findByText("Language models");
//     await screen.findByText("Could not read this setting. Use Refresh above to try again.");
//     expect(document.querySelector(".srow button.htoggle")).toBeNull();
//   });
// });
