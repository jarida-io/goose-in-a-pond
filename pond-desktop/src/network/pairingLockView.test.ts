import { describe, expect, it } from "vitest";
import { ApiError } from "../api/types";
import type { PairingLock } from "../api/types";
import { clientLabel, isReadOnlyRefusal, lockLines, pinProblem } from "./pairingLockView";

const lock = (over: Partial<PairingLock> = {}): PairingLock => ({
  locked: true, enforced: true, owner_profile_id: "p1", has_recovery_pin: false,
  owner_has_a_phone: true, min_pin_digits: 6, can_change: false, ...over,
});

describe("lockLines", () => {
  it("names the main member and the recovery PIN", () => {
    const l = lockLines(lock({ has_recovery_pin: true }), "Amina");
    expect(l.owner).toBe("Main member: Amina");
    expect(l.pin).toBe("A recovery PIN is set.");
    expect(l.warning).toBeNull();
  });
  it("warns only when the lock is on and not holding", () => {
    expect(lockLines(lock({ enforced: false }), null).warning).toMatch(/not holding/);
    expect(lockLines(lock({ locked: false, enforced: false }), null).warning).toBeNull();
  });
  it("says so when nobody is the main member", () => {
    expect(lockLines(lock({ owner_profile_id: null }), null).owner).toMatch(/No main member/);
  });
});

describe("pinProblem", () => {
  it("takes 6 to 12 digits", () => {
    expect(pinProblem("123456", 6)).toBeNull();
    expect(pinProblem("12345", 6)).toMatch(/at least 6/);
    expect(pinProblem("1234567890123", 6)).toMatch(/at most 12/);
    expect(pinProblem("12a456", 6)).toMatch(/digits only/);
  });
  it("honours a higher minimum from the pond", () => {
    expect(pinProblem("123456", 8)).toMatch(/at least 8/);
  });
});

describe("isReadOnlyRefusal", () => {
  it("is a 403 and nothing else", () => {
    expect(isReadOnlyRefusal(new ApiError(403, "pairing_main_member_only"))).toBe(true);
    expect(isReadOnlyRefusal(new ApiError(500, "x"))).toBe(false);
    expect(isReadOnlyRefusal(new Error("x"))).toBe(false);
  });
});

describe("clientLabel", () => {
  it("says what kind of device is asking in a word", () => {
    expect(clientLabel("gotg")).toBe("Phone");
    expect(clientLabel("desktop")).toBe("Desktop");
    expect(clientLabel("kiosk")).toBe("kiosk");
  });
});
