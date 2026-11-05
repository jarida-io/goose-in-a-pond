import { describe, it, expect, vi, beforeEach } from "vitest";

// Mock the API singleton before importing the store (the store imports `api`).
vi.mock("../../api/PondApiClient", () => ({
  api: { invokeTool: vi.fn() },
}));

import { api } from "../../api/PondApiClient";
import { controlDevice, hubGetDevice } from "./hubStore";

const invokeTool = (api as unknown as { invokeTool: ReturnType<typeof vi.fn> })
  .invokeTool;

describe("hubStore.controlDevice", () => {
  beforeEach(() => {
    invokeTool.mockReset();
  });

  it("optimistically applies the patch and calls invokeTool with mapped args", async () => {
    invokeTool.mockResolvedValue({ tool: "x", success: true, content: "ok" });

    const ok = await controlDevice("lamp-test", { on: true, brightness: 75 });

    expect(ok).toBe(true);
    expect(hubGetDevice("lamp-test").on).toBe(true);
    expect(hubGetDevice("lamp-test").brightness).toBe(75);
    expect(invokeTool).toHaveBeenCalledWith({
      server: "giap-device-control",
      tool: "set_device_state",
      args: { device_id: "lamp-test", power: true, brightness: 75 },
    });
  });

  it("reverts the optimistic patch when the backend call fails", async () => {
    invokeTool.mockResolvedValueOnce({ tool: "x", success: true, content: "ok" });
    await controlDevice("door-test", { locked: true });
    expect(hubGetDevice("door-test").locked).toBe(true);

    invokeTool.mockRejectedValueOnce(new Error("network down"));
    const ok = await controlDevice("door-test", { locked: false });

    expect(ok).toBe(false);
    expect(hubGetDevice("door-test").locked).toBe(true); // reverted
  });

  it("reverts the optimistic patch when the device refuses the command", async () => {
    invokeTool.mockResolvedValueOnce({ tool: "x", success: true, content: "ok" });
    await controlDevice("plug-test", { on: true });

    invokeTool.mockResolvedValueOnce({ tool: "x", success: false, content: "Couldn't set power on 'plug-test'" });
    const ok = await controlDevice("plug-test", { on: false });

    expect(ok).toBe(false);
    expect(hubGetDevice("plug-test").on).toBe(true);
  });

  it("maps a thermostat target to target_temp", async () => {
    invokeTool.mockResolvedValue({ tool: "x", success: true, content: "ok" });
    await controlDevice("thermo-test", { target: 72 });
    expect(invokeTool).toHaveBeenCalledWith({
      server: "giap-device-control",
      tool: "set_device_state",
      args: { device_id: "thermo-test", target_temp: 72 },
    });
  });

  it("stays local (no backend call) for non-actuatable patches", async () => {
    const ok = await controlDevice("lamp-test", { temp: "cool" });
    expect(ok).toBe(true);
    expect(invokeTool).not.toHaveBeenCalled();
    expect(hubGetDevice("lamp-test").temp).toBe("cool");
  });
});
