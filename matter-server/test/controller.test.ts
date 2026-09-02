import { describe, expect, it } from "vitest";

import { commissionOptionsFor } from "../src/controller.js";
import { setupCodeKind } from "../src/log.js";

/**
 * `peers.commission()` has no QR-aware option — a `pairingCode` string is always run
 * through `ManualPairingCodeCodec`, which requires exactly 11 or 21 digits after
 * stripping every non-digit character. A QR payload's base-38 letters get stripped
 * along with everything else and almost never land on that length, so handing an
 * "MT:" string through as `pairingCode` fails in milliseconds with "Invalid pairing
 * code" before any network activity — this is the bug `commissionOptionsFor` fixes by
 * decoding the QR payload itself and going through the `passcode` path instead.
 */
describe("commissionOptionsFor", () => {
  it("decodes a QR payload into a passcode and discriminator, not a pairingCode string", () => {
    const code = "MT:-24J0AFN00KA0648G00";
    const options = commissionOptionsFor(code, setupCodeKind(code));
    expect(options).toEqual({ passcode: 20202021, discriminator: 3840 });
  });

  it("throws a commission_failed OpError for a QR payload that fails to decode", () => {
    expect(() => commissionOptionsFor("MT:not-a-real-payload", "pairing_code")).toThrow();
  });

  it("passes an 11-digit manual pairing code through unchanged, as pairingCode", () => {
    const code = "34970112332";
    expect(commissionOptionsFor(code, setupCodeKind(code))).toEqual({ pairingCode: code });
  });

  it("passes a 21-digit manual pairing code through unchanged, as pairingCode", () => {
    const code = "123456789012345678901";
    expect(commissionOptionsFor(code, setupCodeKind(code))).toEqual({ pairingCode: code });
  });

  it("converts a bare 8-digit passcode to a number, with no discriminator", () => {
    const code = "20202021";
    expect(commissionOptionsFor(code, setupCodeKind(code))).toEqual({ passcode: 20202021 });
  });
});
