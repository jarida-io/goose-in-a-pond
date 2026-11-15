import { describe, expect, it } from "vitest";
import { readReach } from "./remoteReach";

describe("readReach", () => {
  it("calls a stopped helper or an unfinished login a problem, not a connection in progress", () => {
    expect(readReach(null, { state: "Unavailable" }).reach).toBe("problem");
    expect(readReach(null, { state: "NeedsLogin" }).reach).toBe("problem");
    expect(readReach(null, { state: "Starting" }).reach).toBe("connecting");
    expect(readReach(null, { state: "Running" }).reach).toBe("on");
  });
});
