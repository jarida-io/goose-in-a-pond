import { describe, expect, it } from "vitest";
import {
  all, atLeast, hhmm, ianaTimezone, integer, latitude, longitude,
  oneOf, optional, range, required, retentionMap, speechCategories, url,
} from "./validation";

const ok = (v: string | null) => expect(v).toBeNull();
const bad = (v: string | null) => expect(v).toBeTypeOf("string");

describe("hhmm", () => {
  it("accepts a 24-hour time", () => {
    ok(hhmm("22:00"));
    ok(hhmm("07:30"));
    ok(hhmm("0:00"));
    ok(hhmm("23:59"));
  });

  it("rejects what would silence the pond all day", () => {
    // Shapes people actually type.
    bad(hhmm("10pm"));
    bad(hhmm("22"));
    bad(hhmm("22:0"));
    bad(hhmm("24:00"));
    bad(hhmm("22:60"));
    bad(hhmm(""));
  });
});

describe("url", () => {
  const ws = url(["ws://", "wss://"], "ws://127.0.0.1:5580/giap");

  it("accepts the schemes the field can open", () => {
    ok(ws("ws://127.0.0.1:5580/giap"));
    ok(ws("wss://matter.local/ws"));
  });

  it("rejects the wrong scheme and the incomplete address", () => {
    bad(ws("http://127.0.0.1:5580"));
    bad(ws("127.0.0.1:5580"));
    bad(ws("ws://"));
  });
});

describe("numbers", () => {
  it("range checks both ends", () => {
    ok(range(0, 1)(0));
    ok(range(0, 1)(1));
    ok(range(0, 1)(0.5));
    bad(range(0, 1)(-0.1));
    bad(range(0, 1)(1.1));
    bad(range(0, 1)("not a number"));
  });

  it("atLeast has no ceiling", () => {
    ok(atLeast(0)(0));
    ok(atLeast(0)(1e9));
    bad(atLeast(1)(0));
  });

  it("integer rejects fractions", () => {
    ok(integer(3));
    bad(integer(3.5));
  });

  it("bounds coordinates to the globe", () => {
    ok(latitude(-1.286));
    ok(longitude(36.817));
    bad(latitude(91));
    bad(longitude(-181));
  });
});

describe("composition", () => {
  it("all returns the first complaint", () => {
    const v = all(integer, range(1, 5));
    ok(v(3));
    bad(v(3.5));
    bad(v(9));
  });

  it("optional passes empties through but still checks values", () => {
    const v = optional(url(["http://"], "http://x"));
    ok(v(""));
    ok(v(null));
    ok(v("   "));
    bad(v("nope"));
  });

  it("required rejects only the empties", () => {
    bad(required()(""));
    bad(required()(null));
    ok(required()("x"));
  });

  it("oneOf matches the offered set", () => {
    ok(oneOf(["a", "b"])("a"));
    bad(oneOf(["a", "b"])("c"));
  });
});

describe("retentionMap", () => {
  it("accepts parsed category pairs", () => {
    ok(retentionMap({ network: 14, sensor: 7 }));
    ok(retentionMap({}));
    ok(retentionMap(null));
  });

  it("rejects a bad category or a fractional day count", () => {
    bad(retentionMap({ "Net Work": 14 }));
    bad(retentionMap({ network: 1.5 }));
    bad(retentionMap({ network: -1 }));
    bad(retentionMap("network 14"));
  });
});

describe("speechCategories", () => {
  it("accepts one or more lower-case names", () => {
    ok(speechCategories("alert"));
    ok(speechCategories("alert, info"));
  });

  it("rejects empty, because empty means permanent silence", () => {
    bad(speechCategories(""));
    bad(speechCategories("   "));
    bad(speechCategories(","));
  });

  it("rejects a name that is not a category", () => {
    bad(speechCategories("Alert!"));
    bad(speechCategories("alert, 42"));
  });
});

describe("timezone", () => {
  it("accepts a zone this device can resolve", () => {
    ok(ianaTimezone("UTC"));
    ok(ianaTimezone("Africa/Nairobi"));
  });

  it("rejects a zone it cannot", () => {
    bad(ianaTimezone("Mars/Olympus_Mons"));
    bad(ianaTimezone(""));
  });

});
