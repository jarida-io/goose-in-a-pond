/** Each tool group, and the recipe `extensions:` name that narrows a run to it
 *  (`recipe_extension_to_tool_group` in crates/pond-api/src/routes.rs). */
export const TOOL_REACH: ReadonlyArray<{ group: string; label: string; recipe?: string }> = [
  { group: "giap-memory", label: "Memories", recipe: "memory" },
  { group: "giap-system", label: "Date, time and notifications" },
  { group: "giap-schedule", label: "Reminders and schedules", recipe: "schedule" },
  { group: "giap-weather", label: "Weather", recipe: "weather" },
  { group: "giap-knowledge", label: "Look things up and search the web" },
  { group: "giap-device", label: "See the devices in the house", recipe: "device" },
  { group: "giap-device-control", label: "Switch devices on and off", recipe: "matter" },
  { group: "giap-sensors", label: "Sensor readings" },
  { group: "giap-context", label: "Personal context" },
  { group: "giap-vision", label: "Cameras", recipe: "vision" },
];
