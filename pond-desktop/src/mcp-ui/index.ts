// Side-effect imports register the built-in cards; add a new card's import here. Each card module
// also exports its component, which is what lets Fast Refresh patch an edited card in place
// instead of reloading the page: a module with no exports is never a refresh boundary.

import "./cards/WeatherCard";
import "./cards/CalendarCard";
import "./cards/NewsCard";
import "./cards/CryptoCard";
import "./cards/SmartHomeCard";
import "./cards/MapCard";
import "./cards/MemoryCard";
import "./cards/ScheduleListCard";
// Before KnowledgeCard: its /knowledge/ pattern also matches "giap-knowledge__search_web" and
// "giap-knowledge__show_pictures".
import "./cards/SearchCard";
import "./cards/PicturesCard";
import "./cards/KnowledgeCard";
import "./cards/WolframCard";
import "./cards/TimeCard";
import "./cards/SystemInfoCard";
import "./cards/DeviceCard";
// GenericCard is NOT auto-registered — used as explicit fallback only

export {
  registerMcpCard,
  findCardRenderer,
  findCardByHint,
  getAllRegistrations,
  type McpCardProps,
  type McpCardRegistration,
} from "./registry";
export { McpCardShell } from "./McpCardShell";
export { GenericCard } from "./cards/GenericCard";
export { McpAppHost, type McpAppHostProps, type McpToolResult } from "./McpAppHost";
