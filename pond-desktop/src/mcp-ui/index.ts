// Side-effect imports register the built-in cards; add a new card's import here.

import "./cards/WeatherCard";
import "./cards/CalendarCard";
import "./cards/NewsCard";
import "./cards/CryptoCard";
import "./cards/SmartHomeCard";
import "./cards/MapCard";
import "./cards/MemoryCard";
import "./cards/ScheduleListCard";
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
