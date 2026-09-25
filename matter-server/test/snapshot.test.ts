import { describe, expect, it } from "vitest";

import {
  applicationEndpoints,
  deviceSlices,
  isVendorCluster,
  sliceForEndpoint,
} from "../src/mapping/snapshot.js";
import { deviceIdForNode, partsOfDeviceId } from "../src/protocol.js";
import {
  bridgeNode,
  bridgedComposedNode,
  endpoint,
  lightNode,
  named,
  node,
} from "./fixtures.js";

describe("vendor clusters", () => {
  it("reads manufacturer ownership off the cluster id, as the spec defines it", () => {
    // Cluster ids are 32 bits with the vendor code in the upper 16 (zero for standard ones).
    expect(isVendorCluster(0x0006)).toBe(false); // OnOff
    expect(isVendorCluster(0x0008)).toBe(false); // LevelControl
    expect(isVendorCluster(0x0051)).toBe(false); // LaundryWasherMode, high but standard
    expect(isVendorCluster(0xfff1fc01)).toBe(true); // a test vendor's own
  });

  it("does not treat a standard cluster GIAP simply ignores as the maker's own", () => {
    // Listing ignored standard clusters (identify, groups...) would bury the real vendor entry.
    for (const standard of [0x0003, 0x0004, 0x002f, 0x0038]) {
      expect(isVendorCluster(standard)).toBe(false);
    }
  });
});

describe("a slice's endpoint order", () => {
  it("puts the slice's own endpoint first, not the lowest-numbered one", () => {
    // A hub allocates endpoint numbers, so a bridged device can sit above its own parts,
    // and every downstream lookup breaks ties by taking the first endpoint.
    const bridged = {
      ...node(90, [
        endpoint(0, {}),
        endpoint(3, { levelControl: { currentLevel: 10 } }, [0x0022]),
        endpoint(7, { onOff: { onOff: true } }, [0x0013, 0x0028]),
      ]),
      rootEndpoint: 7,
    };

    expect(applicationEndpoints(bridged).map(e => e.number)).toEqual([7, 3]);
  });

  it("is plain ascending order for a node that is not a slice", () => {
    const plain = node(2, [endpoint(0, {}), endpoint(13, {}), endpoint(4, {})]);

    expect(applicationEndpoints(plain).map(e => e.number)).toEqual([4, 13]);
  });

  it("does not reorder when the slice's own endpoint is already first", () => {
    const bridged = {
      ...node(90, [endpoint(0, {}), endpoint(2, {}), endpoint(9, {})]),
      rootEndpoint: 2,
    };

    expect(applicationEndpoints(bridged).map(e => e.number)).toEqual([2, 9]);
  });
});

describe("device ids", () => {
  it("leaves an ordinary node's id exactly as it was", () => {
    // Registry rows and fabric state key off this string, so it must never change.
    expect(deviceIdForNode(18n)).toBe("matter-18");
    expect(partsOfDeviceId("matter-18")).toEqual({ nodeId: 18n });
  });

  it("names one bridged device of a hub, and reads it back", () => {
    expect(deviceIdForNode(90n, 2)).toBe("matter-90-2");
    expect(partsOfDeviceId("matter-90-2")).toEqual({ nodeId: 90n, rootEndpoint: 2 });
  });

  it("accepts only the canonical spelling", () => {
    // `BigInt("01")` is `1n`, so a non-canonical spelling would be a second id for one
    // device. The Rust side refuses the same spellings.
    expect(partsOfDeviceId("matter-01")).toBeUndefined();
    expect(partsOfDeviceId("matter-1-02")).toBeUndefined();
    expect(partsOfDeviceId("matter-1-")).toBeUndefined();
    expect(partsOfDeviceId("matter-1-2-3")).toBeUndefined();
    // An endpoint is a u16.
    expect(partsOfDeviceId("matter-1-70000")).toBeUndefined();
    // And not a Matter id at all.
    expect(partsOfDeviceId("mqtt-lamp")).toBeUndefined();
  });
});

/** The endpoint numbers a slice covers, ignoring endpoint 0 which every slice has. */
function covered(slice: { endpoints: { number: number }[] }): number[] {
  return slice.endpoints.map(e => e.number).filter(n => n !== 0).sort((a, b) => a - b);
}

describe("cutting a node into devices", () => {
  it("leaves an ordinary node whole, and identical", () => {
    const light = lightNode();
    expect(deviceSlices(light)).toEqual([light]);
  });

  it("gives a hub one device per bridged child, plus the hub itself", () => {
    const slices = deviceSlices(bridgeNode());

    expect(slices.map(s => s.rootEndpoint)).toEqual([undefined, 3, 4, 5]);
    // The hub keeps its Aggregator; each child gets only its own endpoint.
    expect(covered(slices[0]!)).toEqual([1]);
    expect(covered(slices[1]!)).toEqual([3]);
    expect(covered(slices[2]!)).toEqual([4]);
    expect(covered(slices[3]!)).toEqual([5]);
  });

  it("keeps a bridged device's own parts with it", () => {
    // The speaker at endpoint 3 is the player's; without it the TV would have no volume.
    const slices = deviceSlices(bridgedComposedNode());

    expect(slices.map(s => s.rootEndpoint)).toEqual([undefined, 7]);
    expect(covered(slices[1]!)).toEqual([3, 7]);
    // The device's own endpoint leads, whatever the hub numbered it.
    expect(applicationEndpoints(slices[1]!).map(e => e.number)).toEqual([7, 3]);
  });

  it("keeps the hub's own endpoints on the hub", () => {
    // A node can bridge and expose something itself, e.g. a thermostat hub bridging valves.
    const hybrid = node(92, [
      named("Thermostat Hub"),
      endpoint(1, {}, [0x000e], [], [4]),
      endpoint(2, { thermostat: { localTemperature: 2000 } }, [0x0301]),
      endpoint(4, { onOff: { onOff: true }, bridgedDeviceBasicInformation: {} }, [0x0013, 0x0100]),
    ]);

    const slices = deviceSlices(hybrid);
    expect(covered(slices[0]!)).toEqual([1, 2]);
    expect(covered(slices[1]!)).toEqual([4]);
  });

  it("stays one device for a hub with nothing paired to it", () => {
    // Slicing a childless Aggregator would yield zero devices after a successful pairing.
    const empty = node(93, [named("New Hub"), endpoint(1, {}, [0x000e])]);

    expect(deviceSlices(empty)).toEqual([empty]);
  });

  it("does not recurse forever on a parts list that points at itself", () => {
    // This runs inside `subscribe`: a stack overflow would reconnect-loop the bridge forever.
    const cyclic = node(94, [
      named("Odd Hub"),
      endpoint(1, {}, [0x000e], [], [2]),
      endpoint(2, { onOff: {}, bridgedDeviceBasicInformation: {} }, [0x0013, 0x0100], [], [3]),
      endpoint(3, { levelControl: {} }, [], [], [2]),
    ]);

    const slices = deviceSlices(cyclic);
    expect(covered(slices[1]!)).toEqual([2, 3]);
  });

  it("never lets a parts list drag endpoint 0 into a child", () => {
    // Lazy hubs list it; every child would then inherit the hub's Basic Information.
    const greedy = node(95, [
      endpoint(0, { basicInformation: { nodeLabel: "The Hub" } }, [0x0016]),
      endpoint(1, {}, [0x000e], [], [2]),
      endpoint(2, { onOff: {}, bridgedDeviceBasicInformation: {} }, [0x0013, 0x0100], [], [0]),
    ]);

    expect(covered(deviceSlices(greedy)[1]!)).toEqual([2]);
  });

  it("does not let one bridged device swallow another", () => {
    // PartsList is full-family (every descendant), so a hub may list a sibling under a child.
    const family = node(96, [
      named("Full Family Hub"),
      endpoint(1, {}, [0x000e], [], [2, 3]),
      endpoint(2, { onOff: {}, bridgedDeviceBasicInformation: {} }, [0x0013, 0x0100], [], [3]),
      endpoint(3, { onOff: {}, bridgedDeviceBasicInformation: {} }, [0x0013, 0x0100]),
    ]);

    const slices = deviceSlices(family);
    expect(slices.map(s => s.rootEndpoint)).toEqual([undefined, 2, 3]);
    expect(covered(slices[1]!)).toEqual([2]);
    expect(covered(slices[2]!)).toEqual([3]);
  });

  it("ignores a parts list naming an endpoint the node does not have", () => {
    const phantom = node(97, [
      named("Hub"),
      endpoint(1, {}, [0x000e], [], [2]),
      endpoint(2, { onOff: {}, bridgedDeviceBasicInformation: {} }, [0x0013, 0x0100], [], [99]),
    ]);

    expect(covered(deviceSlices(phantom)[1]!)).toEqual([2]);
  });
});

describe("attributing an endpoint to a device", () => {
  it("finds the device a published attribute belongs to", () => {
    const slices = deviceSlices(bridgeNode());

    expect(sliceForEndpoint(slices, 4)?.rootEndpoint).toBe(4);
    expect(sliceForEndpoint(slices, 5)?.rootEndpoint).toBe(5);
    // A part reports as the device it is part of.
    expect(sliceForEndpoint(deviceSlices(bridgedComposedNode()), 3)?.rootEndpoint).toBe(7);
  });

  it("gives endpoint 0 to the hub, whose Basic Information it is", () => {
    expect(sliceForEndpoint(deviceSlices(bridgeNode()), 0)?.rootEndpoint).toBeUndefined();
  });

  it("has no device for an endpoint the node does not have", () => {
    expect(sliceForEndpoint(deviceSlices(bridgeNode()), 99)).toBeUndefined();
  });
});
