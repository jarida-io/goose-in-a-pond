import { describe, expect, it } from "vitest";

import { commissioningOptions } from "../src/controller.js";
import { setupCodeKind } from "../src/log.js";
import { OpError } from "../src/protocol.js";

/** What `commission` does with a code: classify it, then choose matter.js options. */
function optionsFor(code: string) {
  return commissioningOptions(code.trim(), setupCodeKind(code.trim()));
}

describe("commissioning options", () => {
  it("decodes a QR payload here, because matter.js cannot", () => {
    // MVD's QR payload. matter.js's `commission({pairingCode})` always runs the manual-code codec,
    // which strips non-digits. The payload carries discriminator 3840, passcode 20202021.
    expect(optionsFor("MT:Y.K9042C00KA0648G00")).toEqual({
      passcode: 20202021,
      discriminator: 3840,
    });
  });

  it("takes the long discriminator, which the manual form cannot carry", () => {
    // A manual code has only the short (top 4 bits) discriminator; the long one pins mDNS to one device.
    const options = optionsFor("MT:Y.K9042C00KA0648G00");
    expect(options).toHaveProperty("discriminator", 3840);
    expect(options).not.toHaveProperty("pairingCode");
  });

  it("accepts a QR payload typed in lower case", () => {
    // Base-38 is uppercase and the codec matches `MT:` case-sensitively, so upcasing is lossless.
    expect(optionsFor("mt:y.k9042c00ka0648g00")).toEqual({
      passcode: 20202021,
      discriminator: 3840,
    });
  });

  it("passes a manual pairing code to matter.js, whose decoder reads that form", () => {
    expect(optionsFor("3497-011-2332")).toEqual({ pairingCode: "3497-011-2332" });
  });

  it("passes a bare passcode as a number", () => {
    expect(optionsFor(" 2020-2021 ")).toEqual({ passcode: 20202021 });
  });

  it("calls a malformed QR payload an invalid code, not a failed commission", () => {
    // `commission_failed` means pairing was attempted; an undecodable payload never leaves this process.
    let raised: unknown;
    try {
      optionsFor("MT:...");
    } catch (error) {
      raised = error;
    }
    expect(raised).toBeInstanceOf(OpError);
    expect((raised as OpError).code).toBe("invalid_setup_code");
  });

  it("refuses a payload carrying several devices rather than silently taking one", () => {
    let raised: unknown;
    try {
      optionsFor("MT:Y.K9042C00KA0648G00*Y.K9042C00KA0648G00");
    } catch (error) {
      raised = error;
    }
    expect(raised).toBeInstanceOf(OpError);
    expect((raised as OpError).code).toBe("invalid_setup_code");
    expect((raised as OpError).message).toContain("one at a time");
  });
});
