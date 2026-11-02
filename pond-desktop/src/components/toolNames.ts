// Lets a chip and a history note name a tool the same way.

const NAME_MAP: Record<string, string> = {
  // weather
  get_current_weather: "Weather",
  get_weather_forecast: "Forecast",
  // wikipedia
  get_wikipedia_article: "Wikipedia",
  // computation
  compute_answer: "Computed",
  // web
  search_web: "Web search",
  read_web_page: "Web page",
  // schedule
  list_schedules: "Schedules",
  create_schedule: "Create Schedule",
  delete_schedule: "Delete Schedule",
  pause_schedule: "Pause Schedule",
  resume_schedule: "Resume Schedule",
  run_schedule_now: "Run Schedule",
  // memory
  save_memory: "Save Memory",
  recall_memories: "Recall Memory",
  forget_memory: "Forget Memory",
  // system
  get_system_info: "System Info",
  get_current_time: "Current Time",
  send_notification: "Notification",
  // devices
  list_registered_devices: "Devices",
  get_current_profile: "Profile",
  get_model_config: "Model Config",
  // music
  play: "Play Music",
  status: "Now Playing",
  control: "Playback Control",
};

export function friendlyToolName(raw: string): string {
  // Strip namespace prefix: "giap-knowledge__get_wikipedia_article" → "get_wikipedia_article"
  const bare = raw.includes("__") ? raw.split("__").pop()! : raw;
  if (NAME_MAP[bare]) return NAME_MAP[bare];
  // Fallback: title-case the snake_case function name
  return bare
    .replace(/_/g, " ")
    .replace(/\b\w/g, (c) => c.toUpperCase());
}
