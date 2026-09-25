/**
 * The plain form `controller.ts` projects a matter.js `ClientNode` onto once, so every mapping
 * is a pure function testable without a fabric.
 */

/** Behavior id (e.g. `onOff`) → attribute → value as matter.js decoded it; callers must check types. */
export type ClusterState = Record<string, Record<string, unknown>>;

/**
 * A manufacturer-specific cluster, id only: matter.js gives it a behavior with zero attributes and
 * Matter publishes no names. Recorded so a description does not imply the control is absent.
 */
export interface VendorCluster {
  /** The 32-bit Matter cluster id, e.g. 0xfff1fc01. */
  id: number;
}

export interface EndpointSnapshot {
  /** The Matter endpoint number. Endpoint 0 is the root and never the application. */
  number: number;
  /** Device type ids from the Descriptor cluster's DeviceTypeList. */
  deviceTypes: number[];
  clusters: ClusterState;
  /** Manufacturer-specific clusters here. Empty for all but a handful of devices. */
  vendorClusters: VendorCluster[];
  /**
   * Child endpoints from matter.js's resolved tree, NOT Descriptor `partsList` (full-family for an
   * Aggregator, may name endpoint 0 or cycle). Empty until the structure is read.
   */
  parts: number[];
}

/**
 * Manufacturer-specific: a non-zero vendor code in the id's upper 16 bits. Not "unnamed in the
 * snapshot", which also drops standard utility clusters (`identify`, `groups`, …).
 */
export function isVendorCluster(id: number): boolean {
  return (id >>> 16) !== 0;
}

export interface NodeSnapshot {
  nodeId: bigint;
  online: boolean;
  endpoints: EndpointSnapshot[];
  /** The endpoint this slice is about, for a bridged device (see `deviceSlices`); absent for a plain node. */
  rootEndpoint?: number;
}

/**
 * Application endpoints (not 0): this slice's own endpoint first, then ascending. Root-first is
 * load-bearing: callers take the first match, and a hub numbers endpoints in discovery order.
 */
export function applicationEndpoints(node: NodeSnapshot): EndpointSnapshot[] {
  const application = node.endpoints
    .filter(e => e.number !== 0)
    .sort((a, b) => a.number - b.number);
  if (node.rootEndpoint === undefined) return application;

  const own = application.findIndex(e => e.number === node.rootEndpoint);
  if (own <= 0) return application;
  return [application[own]!, ...application.slice(0, own), ...application.slice(own + 1)];
}

/** The first application endpoint carrying `behaviorId`, if any. */
export function endpointWith(node: NodeSnapshot, behaviorId: string): EndpointSnapshot | undefined {
  return applicationEndpoints(node).find(e => behaviorId in e.clusters);
}

export function hasCluster(node: NodeSnapshot, behaviorId: string): boolean {
  return endpointWith(node, behaviorId) !== undefined;
}

/** An attribute on the root endpoint, where Basic Information lives. */
export function rootAttribute(
  node: NodeSnapshot,
  behaviorId: string,
  attribute: string,
): unknown {
  return node.endpoints.find(e => e.number === 0)?.clusters[behaviorId]?.[attribute];
}

// ── Bridges ──────────────────────────────────────────────────────────────────

/** Matter's Aggregator: the endpoint that says "this node speaks for others". */
export const DEVICE_TYPE_AGGREGATOR = 0x000e;
/** Matter's Bridged Node: the endpoint that IS one of those others. */
export const DEVICE_TYPE_BRIDGED_NODE = 0x0013;

/**
 * One snapshot per GIAP device. Sliced on Bridged Node: its descriptor and the Aggregator's arrive
 * in separate reports. The hub is always one: the first, type-less snapshot yields a single slice,
 * which must be the hub or it is registered and never removed (`peers.deleted` never fires).
 */
export function deviceSlices(node: NodeSnapshot): NodeSnapshot[] {
  const application = applicationEndpoints(node);
  const bridged = application.filter(e => e.deviceTypes.includes(DEVICE_TYPE_BRIDGED_NODE));
  // Not a bridge, or not yet known to be one.
  if (bridged.length === 0) return [node];

  const byNumber = new Map(node.endpoints.map(e => [e.number, e]));
  const root = node.endpoints.find(e => e.number === 0);
  const claimed = new Set<number>();

  const children = bridged.map(child => ({
    nodeId: node.nodeId,
    online: node.online,
    endpoints: withRoot(root, subtreeOf(child, byNumber, claimed)),
    rootEndpoint: child.number,
  }));

  // The hub: endpoint 0 plus every endpoint no child claimed (e.g. a thermostat hub's own).
  const hub: NodeSnapshot = {
    nodeId: node.nodeId,
    online: node.online,
    endpoints: withRoot(
      root,
      application.filter(e => !claimed.has(e.number)),
    ),
  };

  return [hub, ...children];
}

/** Endpoint 0 stays on every slice: the hub's Basic Information is a silent child's name fallback. */
function withRoot(
  root: EndpointSnapshot | undefined,
  endpoints: EndpointSnapshot[],
): EndpointSnapshot[] {
  return root === undefined ? endpoints : [root, ...endpoints];
}

/**
 * A bridged device and its parts. The claimed set makes cyclic `parts` terminate (this runs in
 * `subscribe`); descent stops at a nested Bridged Node, which is a device of its own.
 */
function subtreeOf(
  start: EndpointSnapshot,
  byNumber: ReadonlyMap<number, EndpointSnapshot>,
  claimed: Set<number>,
): EndpointSnapshot[] {
  const subtree: EndpointSnapshot[] = [];
  const queue: EndpointSnapshot[] = [start];

  while (queue.length > 0) {
    const endpoint = queue.shift()!;
    // Endpoint 0 is the hub's, never a child's, however a `parts` list names it.
    if (endpoint.number === 0 || claimed.has(endpoint.number)) continue;
    claimed.add(endpoint.number);
    subtree.push(endpoint);

    for (const part of endpoint.parts) {
      const child = byNumber.get(part);
      if (child === undefined || child.number === 0) continue;
      if (child.deviceTypes.includes(DEVICE_TYPE_BRIDGED_NODE)) continue;
      queue.push(child);
    }
  }
  return subtree;
}

/** Carries an Aggregator: tells an emptied hub (still has one) from momentarily unreadable descriptors. */
export function hasAggregator(node: NodeSnapshot): boolean {
  return applicationEndpoints(node).some(e => e.deviceTypes.includes(DEVICE_TYPE_AGGREGATOR));
}

/** The slice an endpoint belongs to; endpoint 0, on every slice, resolves to the hub. */
export function sliceForEndpoint(
  slices: readonly NodeSnapshot[],
  endpointNumber: number,
): NodeSnapshot | undefined {
  if (endpointNumber === 0) return slices.find(slice => slice.rootEndpoint === undefined);
  return slices.find(slice =>
    slice.endpoints.some(e => e.number !== 0 && e.number === endpointNumber),
  );
}
