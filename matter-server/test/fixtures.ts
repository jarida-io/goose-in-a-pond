/** Recorded devices the cluster mappings were calibrated against. */

import type {
  ClusterState,
  EndpointSnapshot,
  NodeSnapshot,
  VendorCluster,
} from "../src/mapping/snapshot.js";

export function endpoint(
  number: number,
  clusters: ClusterState,
  deviceTypes: number[] = [],
  vendorClusters: VendorCluster[] = [],
  parts: number[] = [],
): EndpointSnapshot {
  return { number, deviceTypes, clusters, vendorClusters, parts };
}

export function node(nodeId: number, endpoints: EndpointSnapshot[], online = true): NodeSnapshot {
  return { nodeId: BigInt(nodeId), online, endpoints };
}

/** The root endpoint carrying a user-assigned name, as every real device has. */
export function named(name: string): EndpointSnapshot {
  return endpoint(0, { basicInformation: { nodeLabel: name } }, [0x0016]);
}

/** The Matter Virtual Device's fan: Fan Control on endpoint 1 and no On/Off cluster. */
export function fanNode(): NodeSnapshot {
  return node(18, [
    named("Living Room Fan"),
    endpoint(1, { fanControl: { fanMode: 0, percentSetting: 0 } }),
  ]);
}

/** A real bulb: OnOff + LevelControl on endpoint 13. */
export function lightNode(): NodeSnapshot {
  return node(2, [
    endpoint(0, {}),
    endpoint(13, { onOff: { onOff: false }, levelControl: { currentLevel: 128 } }),
  ]);
}

/** Matter Virtual Device 1.7.0 light plus a vendor cluster; the wire carries only its id. */
export function customLightNode(): NodeSnapshot {
  return node(31, [
    named("Virtual Custom OnOff Light"),
    endpoint(1, { onOff: { onOff: false }, levelControl: { currentLevel: 0 } }, [0x0101], [
      { id: 0xfff1fc01 },
    ]),
  ]);
}

/** Google's Virtual Door Lock: lock state plus optional door state and PIN requirement. */
export function doorLockNode(): NodeSnapshot {
  return node(44, [
    named("Virtual Door Lock"),
    endpoint(1, {
      doorLock: { lockState: 1, doorState: 0, requirePinForRemoteOperation: false },
    }, [0x000a]),
  ]);
}

/** A lock with neither optional feature: lockState and nothing else. */
export function bareLockNode(): NodeSnapshot {
  return node(45, [named("Deadbolt"), endpoint(1, { doorLock: { lockState: 1 } }, [0x000a])]);
}

/** Google's Virtual Extended Color Light (`colorCapabilities` bits 0 HueSat, 3 Xy, 4 ColorTemp). */
export function extendedColorLightNode(): NodeSnapshot {
  return node(51, [
    named("Virtual Extended Color Light"),
    endpoint(1, {
      onOff: { onOff: true },
      levelControl: { currentLevel: 254 },
      colorControl: {
        colorCapabilities: 0x19,
        colorMode: 0,
        currentHue: 0,
        currentSaturation: 0,
        colorTemperatureMireds: 250,
        colorTempPhysicalMinMireds: 153,
        colorTempPhysicalMaxMireds: 500,
      },
    }, [0x010d]),
  ]);
}

/** Google's Extended Color Light as shipped: colour modes in its UI, `colorCapabilities` of none. */
export function mvdColorLightNode(): NodeSnapshot {
  return node(56, [
    named("Virtual Extended Color Light"),
    endpoint(1, {
      onOff: { onOff: true },
      levelControl: { currentLevel: 254 },
      colorControl: {
        colorCapabilities: {
          hueSaturation: false,
          enhancedHue: false,
          colorLoop: false,
          xy: false,
          colorTemperature: false,
        },
        colorMode: 0,
        currentHue: 0,
        currentSaturation: 0,
        currentX: 24939,
        currentY: 24701,
        colorTemperatureMireds: 250,
      },
    }, [0x010d]),
  ]);
}

/** A tunable-white bulb: ColorControl with colour temperature only (`colorCapabilities` bit 4). */
export function tunableWhiteNode(): NodeSnapshot {
  return node(52, [
    named("Reading Lamp"),
    endpoint(1, {
      onOff: { onOff: true },
      colorControl: {
        colorCapabilities: 0x10,
        colorMode: 2,
        colorTemperatureMireds: 370,
        colorTempPhysicalMinMireds: 200,
        colorTempPhysicalMaxMireds: 454,
      },
    }, [0x010c]),
  ]);
}

/** A Room Air Conditioner as one really reports: cooling only, and `systemMode` as an enum name. */
export function airConditionerNode(): NodeSnapshot {
  return node(61, [
    named("Room Air Conditioner"),
    endpoint(1, {
      onOff: { onOff: false },
      thermostat: {
        controlSequenceOfOperation: 0,
        systemMode: "Cool",
        localTemperature: 2500,
        occupiedCoolingSetpoint: 2400,
        absMinCoolSetpointLimit: 1600,
        absMaxCoolSetpointLimit: 3200,
        absMinHeatSetpointLimit: 700,
        absMaxHeatSetpointLimit: 3000,
      },
    }, [0x0072]),
  ]);
}

/** A Smoke CO Alarm as one really reports, sounding for CO while its smoke reading is Critical. */
export function smokeCoAlarmNode(): NodeSnapshot {
  return node(71, [
    named("Smoke CO Alarm"),
    endpoint(1, {
      smokeCoAlarm: {
        featureMap: { smokeAlarm: true, coAlarm: true },
        expressedState: 2,
        smokeState: 2,
        coState: 2,
        batteryAlert: 0,
        endOfServiceAlert: 0,
        hardwareFaultAlert: false,
      },
    }, [0x0076]),
  ]);
}

/** Google's Matter Virtual Device Generic Switch: latching, with two stated positions. */
export function genericSwitchNode(): NodeSnapshot {
  return node(6, [
    named("Generic Switch"),
    endpoint(1, {
      switch: {
        featureMap: { latchingSwitch: true, momentarySwitch: false },
        numberOfPositions: 2,
        currentPosition: 1,
      },
    }, [0x000f]),
  ]);
}

/** A momentary pushbutton that states no `numberOfPositions`. */
export function momentarySwitchNode(): NodeSnapshot {
  return node(7, [
    named("Button"),
    endpoint(1, {
      switch: {
        featureMap: { latchingSwitch: false, momentarySwitch: true },
        currentPosition: 0,
      },
    }, [0x000f]),
  ]);
}

/** A CO-only alarm: no smoke feature, so no smoke reading exists at all. */
export function coOnlyAlarmNode(): NodeSnapshot {
  return node(72, [
    named("CO Alarm"),
    endpoint(1, {
      smokeCoAlarm: {
        // The feature map, not value presence, says smoke is absent (unreported is undefined too).
        featureMap: { smokeAlarm: false, coAlarm: true },
        expressedState: 0,
        coState: 0,
        batteryAlert: 0,
      },
    }, [0x0076]),
  ]);
}

/** A Basic Video Player (endpoint 1) whose Level Control is on its speaker endpoint (2). */
export function videoPlayerNode(): NodeSnapshot {
  return node(81, [
    named("Basic Video Player"),
    endpoint(1, {
      onOff: { onOff: true },
      mediaPlayback: { currentState: 0 },
      mediaInput: {
        currentInput: 1,
        inputList: [
          { index: 1, inputType: 4, name: "HDMI 1" },
          { index: 2, inputType: 4, name: "HDMI 2" },
        ],
      },
      audioOutput: {
        currentOutput: 1,
        outputList: [
          { index: 1, outputType: 0, name: "TV Speaker" },
          { index: 2, outputType: 3, name: "Soundbar" },
        ],
      },
    }, [0x0028]),
    endpoint(2, { onOff: { onOff: true }, levelControl: { currentLevel: 127 } }, [0x0022]),
  ]);
}

/** A node typed by a Descriptor DeviceTypeList on its application endpoint, like real ones. */
export function describedNode(
  nodeId: number,
  deviceType: number,
  clusters: ClusterState = {},
): NodeSnapshot {
  return node(nodeId, [
    endpoint(0, {}, [0x0016]),
    endpoint(1, clusters, [deviceType]),
  ]);
}

/** The Matter Virtual Device's Laundry Washer. */
export function laundryWasherNode(): NodeSnapshot {
  return node(50, [
    named("Virtual Laundry Washer"),
    endpoint(
      1,
      {
        onOff: { onOff: false },
        laundryWasherMode: {
          currentMode: 0,
          supportedModes: [
            { label: "Normal", mode: 0 },
            { label: "Heavy", mode: 1 },
            { label: "Delicate", mode: 2 },
            { label: "Whites", mode: 3 },
          ],
        },
        temperatureControl: {
          selectedTemperatureLevel: 0,
          supportedTemperatureLevels: ["Cold", "Warm", "Hot"],
        },
        laundryWasherControls: {
          spinSpeeds: ["Low", "Medium", "High"],
          spinSpeedCurrent: 0,
          numberOfRinses: 1,
          supportedRinses: ["None", "Normal", "Extra", "Max"],
        },
        operationalState: {
          operationalState: 0,
          // The spec ties the published states to the commands a washer accepts.
          operationalStateList: [
            { operationalStateId: 0, operationalStateLabel: "Stopped" },
            { operationalStateId: 1, operationalStateLabel: "Running" },
            { operationalStateId: 2, operationalStateLabel: "Paused" },
            { operationalStateId: 3, operationalStateLabel: "Error" },
          ],
        },
      },
      [0x0073],
    ),
  ]);
}

/** A Hue-shaped hub: an Aggregator with three bridged devices, on hub-allocated endpoints. */
export function bridgeNode(): NodeSnapshot {
  return node(90, [
    endpoint(0, { basicInformation: { nodeLabel: "Living Room Hub" } }, [0x0016]),
    endpoint(1, {}, [0x000e], [], [3, 4, 5]),
    endpoint(
      3,
      {
        onOff: { onOff: true },
        levelControl: { currentLevel: 254 },
        bridgedDeviceBasicInformation: { nodeLabel: "Kitchen Lamp", reachable: true },
      },
      [0x0013, 0x0101],
    ),
    endpoint(
      4,
      {
        onOff: { onOff: false },
        levelControl: { currentLevel: 10 },
        bridgedDeviceBasicInformation: { nodeLabel: "Hall Lamp", reachable: true },
      },
      [0x0013, 0x0101],
    ),
    endpoint(
      5,
      {
        doorLock: { lockState: 1, doorState: 0 },
        bridgedDeviceBasicInformation: { nodeLabel: "Front Door", reachable: true },
      },
      [0x0013, 0x000a],
    ),
  ]);
}

/** A bridged video player at endpoint 7 with its Speaker at 3: lowest-endpoint-wins picks wrong. */
export function bridgedComposedNode(): NodeSnapshot {
  return node(91, [
    named("Media Hub"),
    endpoint(1, {}, [0x000e], [], [7]),
    endpoint(
      7,
      {
        mediaPlayback: { currentState: 0 },
        bridgedDeviceBasicInformation: { nodeLabel: "Telly", reachable: true },
      },
      [0x0013, 0x0028],
      [],
      [3],
    ),
    endpoint(3, { levelControl: { currentLevel: 60 } }, [0x0022]),
  ]);
}


/** A water valve with the optional LVL feature: open, shut, and how far. */
export function levelValveNode(): NodeSnapshot {
  return node(60, [
    named("Garden Valve"),
    endpoint(
      1,
      {
        valveConfigurationAndControl: {
          featureMap: { level: true, timeSync: false },
          currentState: 1,
          targetState: 1,
          currentLevel: 40,
          valveFault: {},
        },
      },
      [0x0042],
    ),
  ]);
}

/** A plain solenoid: open or shut, with nothing in between and no level to set. */
export function plainValveNode(): NodeSnapshot {
  return node(61, [
    named("Mains Shutoff"),
    endpoint(
      1,
      {
        valveConfigurationAndControl: {
          featureMap: { level: false, timeSync: false },
          currentState: 0,
          targetState: 0,
        },
      },
      [0x0042],
    ),
  ]);
}
