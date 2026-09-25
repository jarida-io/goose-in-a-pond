import { describe, expect, it } from "vitest";
import type { ClientNode } from "@matter/main";

import { availabilityReports } from "../src/controller.js";

/** A fake `ClientNode` with just the two fields `availabilityReports` reads. */
function peer(nodeId: number | undefined, online: boolean): ClientNode {
  return {
    peerAddress: nodeId === undefined ? undefined : { nodeId },
    lifecycle: { isOnline: online },
  } as unknown as ClientNode;
}

describe("availability reports", () => {
  it("names every peer, whether or not its reachability changed", () => {
    // A level, not an edge: a node online before the controller connects never fires `online`.
    const reports = availabilityReports([peer(2, true), peer(3, false)]);

    expect(reports).toEqual([
      { deviceId: "matter-2", online: true },
      { deviceId: "matter-3", online: false },
    ]);
  });

  it("repeats an unchanged report rather than falling silent", () => {
    const peers = [peer(2, true)];

    expect(availabilityReports(peers)).toEqual(availabilityReports(peers));
    expect(availabilityReports(peers)).toHaveLength(1);
  });

  it("leaves out a node that has only been discovered, not commissioned", () => {
    // Discovery adds commissionable nodes to the same collection, with no peer address.
    expect(availabilityReports([peer(undefined, true), peer(4, true)])).toEqual([
      { deviceId: "matter-4", online: true },
    ]);
  });
});
