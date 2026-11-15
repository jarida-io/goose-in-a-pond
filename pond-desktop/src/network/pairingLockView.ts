import { ApiError } from "../api/types";
import type { PairingLock } from "../api/types";

export const MAX_PIN_DIGITS = 12;

/** The three lines a status block shows, in words the household uses. */
export function lockLines(lock: PairingLock, ownerName: string | null): { state: string; owner: string; pin: string; warning: string | null } {
  return {
    state: lock.locked ? "Pairing is locked. A new phone waits for the main member to let it in." : "Pairing is open. Anyone with the code can pair a phone.",
    owner: lock.owner_profile_id ? `Main member: ${ownerName ?? "someone this app cannot name"}` : "No main member has been chosen yet.",
    pin: lock.has_recovery_pin ? "A recovery PIN is set." : "No recovery PIN is set.",
    warning:
      lock.locked && !lock.enforced
        ? "The lock is not holding yet: the main member has no paired phone and no recovery PIN, so a new phone can still pair. Pair their phone or set a recovery PIN."
        : null,
  };
}

/** What kind of device is asking, in a word; the pond's own name for it when it is neither. */
export function clientLabel(clientType: string): string {
  if (clientType === "gotg") return "Phone";
  if (clientType === "desktop") return "Desktop";
  return clientType;
}

/** What is wrong with a PIN, or null. Digits only, between the pond's minimum and 12. */
export function pinProblem(pin: string, minDigits: number): string | null {
  const floor = Math.max(minDigits, 6);
  if (!/^\d*$/.test(pin)) return "Use digits only.";
  if (pin.length < floor) return `Use at least ${floor} digits.`;
  if (pin.length > MAX_PIN_DIGITS) return `Use at most ${MAX_PIN_DIGITS} digits.`;
  return null;
}

/** A 403 means this device may look but not change: the pond's own screen is often not a member's phone. */
export function isReadOnlyRefusal(e: unknown): boolean {
  return e instanceof ApiError && e.status === 403;
}
