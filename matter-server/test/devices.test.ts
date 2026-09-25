import { describe, expect, it } from "vitest";

import { isSnapshotCluster } from "../src/controller.js";
import { deviceTypeFromDescriptor, nodeToDevice } from "../src/mapping/devices.js";
import {
  describedNode,
  endpoint,
  extendedColorLightNode,
  fanNode,
  genericSwitchNode,
  lightNode,
  named,
  node,
  tunableWhiteNode,
} from "./fixtures.js";

describe("device typing", () => {
  it("keeps a fan with no On/Off cluster powerable, and types it as a fan", () => {
    const device = nodeToDevice(fanNode());
    expect(device.device_type).toBe("fan");
    // Power comes from FanMode here; fan_speed from PercentSetting.
    expect(device.capabilities).toEqual(["power", "fan_speed"]);
    expect(device.name).toBe("Living Room Fan");
    expect(device.id).toBe("matter-18");
  });

  it("reports power once when a fan does implement On/Off", () => {
    const withSwitch = node(18, [
      named("Living Room Fan"),
      endpoint(1, { fanControl: { fanMode: 0 }, onOff: { onOff: false } }),
    ]);
    const device = nodeToDevice(withSwitch);
    expect(device.device_type).toBe("fan");
    expect(device.capabilities).toEqual(["power", "fan_speed"]);
  });

  it("tells a plug from a bulb, which the clusters alone cannot", () => {
    const plug = describedNode(30, 0x010a, { onOff: { onOff: false } });
    expect(nodeToDevice(plug).device_type).toBe("plug");

    const bulb = describedNode(31, 0x0100, { onOff: { onOff: false } });
    expect(nodeToDevice(bulb).device_type).toBe("light");
  });

  it("gives appliances, alarms and coverings their own types", () => {
    const cases: [number, string][] = [
      [0x0075, "appliance"], // Dishwasher
      [0x0073, "appliance"], // Laundry Washer
      [0x0303, "pump"], // Pump
      [0x0028, "media"], // Basic Video Player
      [0x0074, "vacuum"], // Robotic Vacuum
      [0x0076, "alarm"], // Smoke/CO Alarm
      [0x0202, "covering"], // Window Covering
      [0x002c, "air"], // Air Purifier
      [0x002d, "sensor"], // Air Quality Sensor
    ];
    for (const [id, want] of cases) {
      expect(nodeToDevice(describedNode(40, id)).device_type, `device type 0x${id.toString(16)}`)
        .toBe(want);
    }
  });

  it("types the rest of the Matter device library, not just the ones MVD offers", () => {
    // Ids read off matter.js's own device definitions, not transcribed from the spec.
    const cases: [number, string][] = [
      [0x010f, "plug"], // Mounted On/Off Control
      [0x0110, "plug"], // Mounted Dimmable Load Control
      [0x0309, "thermostat"], // Heat Pump
      [0x050f, "thermostat"], // Water Heater
      [0x0041, "sensor"], // Water Freeze Detector
      [0x0043, "sensor"], // Water Leak Detector
      [0x0044, "sensor"], // Rain Sensor
      [0x007c, "appliance"], // Laundry Dryer
      [0x0079, "appliance"], // Microwave Oven
      [0x0078, "appliance"], // Cooktop
      [0x007b, "appliance"], // Oven
      [0x0070, "appliance"], // Refrigerator
      [0x0071, "appliance"], // Temperature Controlled Cabinet
      [0x0077, "appliance"], // Cook Surface
      [0x007a, "fan"], // Extractor Hood
      [0x0022, "media"], // Speaker
    ];
    for (const [id, want] of cases) {
      expect(nodeToDevice(describedNode(44, id)).device_type, `device type 0x${id.toString(16)}`)
        .toBe(want);
    }
  });

  it("leaves the client device types untyped, deliberately", () => {
    // Remotes hold no server cluster GIAP could read or drive; don't type them from the spec.
    const remotes = [
      0x0103, // On/Off Light Switch
      0x0104, // Dimmer Switch
      0x0105, // Colour Dimmer Switch
      0x000b, // Door Lock Controller
      0x0203, // Window Covering Controller
      0x0304, // Pump Controller
      0x030a, // Thermostat Controller
      0x0840, // Control Bridge
      0x0850, // On/Off Sensor
      0x0029, // Casting Video Client
      0x002a, // Video Remote Control
    ];
    for (const id of remotes) {
      expect(deviceTypeFromDescriptor(describedNode(45, id)), `device type 0x${id.toString(16)}`)
        .toBeUndefined();
    }
  });

  it("does not mistake the root endpoint for the device", () => {
    // Endpoint 0 is the Root Node (0x0016) on every device.
    expect(deviceTypeFromDescriptor(describedNode(41, 0x0075))).toBe("appliance");
    const rootOnly = node(42, [endpoint(0, {}, [0x0016])]);
    expect(deviceTypeFromDescriptor(rootOnly)).toBeUndefined();
  });

  it("falls back to cluster inference without a usable descriptor", () => {
    expect(deviceTypeFromDescriptor(lightNode())).toBeUndefined();
    expect(nodeToDevice(lightNode()).device_type).toBe("light");

    // An unknown device type id falls through to the clusters too.
    const unknown = describedNode(43, 0xbeef, { onOff: { onOff: false } });
    expect(nodeToDevice(unknown).device_type).toBe("light");
  });

  it("prefers the user's name, then the vendor's, then a speakable fallback", () => {
    const both = node(7, [
      endpoint(0, { basicInformation: { nodeLabel: "Hallway", productName: "Hue color lamp" } }),
      endpoint(1, { onOff: { onOff: false } }),
    ]);
    expect(nodeToDevice(both).name).toBe("Hallway");

    const vendorOnly = node(7, [
      endpoint(0, { basicInformation: { productName: "Hue color lamp" } }),
      endpoint(1, { onOff: { onOff: false } }),
    ]);
    expect(nodeToDevice(vendorOnly).name).toBe("Hue color lamp");

    expect(nodeToDevice(lightNode()).name).toBe("Light 2");
  });

  it("treats a blank name as no name at all", () => {
    // Devices ship with an empty nodeLabel.
    const blank = node(9, [
      endpoint(0, { basicInformation: { nodeLabel: "   ", productName: "Acme Plug" } }),
      endpoint(1, { onOff: { onOff: false } }),
    ]);
    expect(nodeToDevice(blank).name).toBe("Acme Plug");
  });

  it("types a Generic Switch as a switch, so it stops wearing a monitor", () => {
    const device = nodeToDevice(genericSwitchNode());

    expect(device.device_type).toBe("switch");
    // Still nothing to drive. A switch reports; it does not take orders.
    expect(device.capabilities).toEqual([]);
  });

  it("admits the switch cluster into the snapshot", () => {
    // `readClusters` skips clusters outside the allowlist, so a mapping over one never runs.
    expect(isSnapshotCluster("switch")).toBe(true);
  });

  it("leaves an unmappable device typed matter with no capabilities", () => {
    // The signal that a mapping is missing, not that the device is broken.
    const opaque = node(50, [endpoint(0, {}), endpoint(1, {})]);
    const device = nodeToDevice(opaque);
    expect(device.device_type).toBe("matter");
    expect(device.capabilities).toEqual([]);
  });
  it("lists an appliance's temperature, which is not a thermostat's", () => {
    // A model reads this listing to decide whether to fetch the full description.
    const dishwasher = node(105, [
      named("Dishwasher"),
      endpoint(1, {
        onOff: { onOff: false },
        temperatureControl: { temperatureSetpoint: 4900, minTemperature: 4900, maxTemperature: 8200 },
      }),
    ]);

    expect(nodeToDevice(dishwasher).capabilities).toContain("temperature");

    // Named temperature levels are a mode, not a numeric temperature capability.
    const washer = node(106, [
      named("Washer"),
      endpoint(1, {
        temperatureControl: { supportedTemperatureLevels: ["Cold", "Hot"] },
      }),
    ]);
    expect(nodeToDevice(washer).capabilities).not.toContain("temperature");
    expect(nodeToDevice(washer).capabilities).toContain("mode");
  });
});

describe("colour in the short capability list", () => {
  it("lists the colour controls the device claims", () => {
    const caps = nodeToDevice(extendedColorLightNode()).capabilities;

    expect(caps).toContain("color");
    expect(caps).toContain("color_temp");
  });

  it("does not list a hue for a bulb that only does white", () => {
    // Gated on the same claim `describe` reads, so the two cannot disagree.
    const caps = nodeToDevice(tunableWhiteNode()).capabilities;

    expect(caps).toContain("color_temp");
    expect(caps).not.toContain("color");
  });
});
