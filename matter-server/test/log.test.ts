import { describe, expect, it } from "vitest";

import { describeError, redactSetupCode, setupCodeKind } from "../src/log.js";

describe("setup code redaction", () => {
  // Security: a pairing code grants fabric access, so none may reach a log or an API error.
  it("removes QR payloads", () => {
    expect(redactSetupCode("commissioning MT:Y.K9042C00KA0648G00 failed")).toBe(
      "commissioning [redacted:setup-code] failed",
    );
  });

  it("removes manual pairing codes and passcodes", () => {
    expect(redactSetupCode("code 34970112332 rejected")).toBe("code [redacted:setup-code] rejected");
    expect(redactSetupCode("passcode 20202021 rejected")).toBe(
      "passcode [redacted:setup-code] rejected",
    );
    expect(redactSetupCode("long 749701123320000000000 x")).toBe(
      "long [redacted:setup-code] x",
    );
  });

  it("leaves digit runs of other lengths alone", () => {
    expect(redactSetupCode("node 18 on port 5580")).toBe("node 18 on port 5580");
    expect(redactSetupCode("took 1234567 ms")).toBe("took 1234567 ms");
  });

  it("does not leave a readable fragment of a code behind", () => {
    // The QR rule must run before the digit rule, or it would redact only part of a payload.
    const redacted = redactSetupCode("MT:Y.K9042C00KA0648G00");
    expect(redacted).toBe("[redacted:setup-code]");
    expect(redacted).not.toMatch(/\d{4}/);
  });

  it("is idempotent", () => {
    const once = redactSetupCode("code 34970112332");
    expect(redactSetupCode(once)).toBe(once);
  });

  it("classifies a code without revealing it", () => {
    expect(setupCodeKind("MT:Y.K9042C00KA0648G00")).toBe("qr_payload");
    expect(setupCodeKind("3497-011-2332")).toBe("pairing_code");
    expect(setupCodeKind("20202021")).toBe("passcode");
    expect(setupCodeKind("nonsense")).toBe("unknown");
  });
});

describe("error description", () => {
  it("renders the whole cause chain, not just the outermost layer", () => {
    const cause = new Error("no usable network interface");
    const wrapped = new Error("discovery of node discovery failed", { cause });

    const described = describeError(wrapped);
    expect(described).toContain("no usable network interface");
    expect(described).toContain("discovery of node");
  });

  it("does not repeat a message already embedded in its wrapper", () => {
    const cause = new Error("EMSGSIZE");
    expect(describeError(new Error("EMSGSIZE", { cause }))).toBe("EMSGSIZE");
  });

  it("survives a cyclic chain", () => {
    const a = new Error("a") as Error & { cause?: unknown };
    const b = new Error("b", { cause: a }) as Error & { cause?: unknown };
    a.cause = b;
    expect(describeError(b)).toBe("b: a");
  });

  it("redacts the chain, not only the outermost message", () => {
    const cause = new Error("PASE failed for MT:Y.K9042C00KA0648G00");
    const described = describeError(new Error("commissioning failed", { cause }));
    expect(described).not.toContain("MT:");
  });

  it("always says something", () => {
    expect(describeError(new Error(""))).toBe("an error with no message");
  });
});
