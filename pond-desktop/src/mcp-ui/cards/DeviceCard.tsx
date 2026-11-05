import { Monitor, Wifi, WifiOff, Router, Loader } from "lucide-react";
import { Chip } from "@heroui/react";
import { registerMcpCard, type McpCardProps } from "../registry";
import { isOpenWhenUsed, openStateLabel } from "../../sections/clientPresence";

interface Device {
  name: string;
  is_online: boolean;
  device_type?: string;
  room?: string;
  ip?: string;
}

function deviceTypeIcon(type?: string) {
  const t = (type ?? "").toLowerCase();
  if (t.includes("router") || t.includes("gateway") || t.includes("access")) return <Router size={14} />;
  if (t.includes("phone") || t.includes("tablet") || t.includes("mobile")) return <Wifi size={14} />;
  return <Monitor size={14} />;
}

export function DeviceCard({ data, variant }: McpCardProps) {
  const isCompact = variant === "compact";
  const devices = (data.devices ?? []) as Device[];

  if (devices.length === 0 && Object.keys(data).length === 0) {
    return (
      <div className="ui-card ui-device">
        <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 0" }}>
          <Loader size={16} style={{ animation: "spin 1.5s linear infinite", color: "#8C4BFF" }} />
          <span style={{ fontSize: 13, color: "#8A8A8A" }}>Loading devices...</span>
        </div>
        <style>{`@keyframes spin { to { transform: rotate(360deg); } }`}</style>
      </div>
    );
  }

  if (devices.length === 0) {
    return (
      <div className="ui-card ui-device">
        <div className="ui-device__empty">
          <Router size={20} style={{ color: "#D9D9D9" }} />
          <span>No devices registered</span>
        </div>
      </div>
    );
  }

  const onlineCount = devices.filter((d) => d.is_online).length;
  // A closed dashboard or CLI is not a device that dropped off.
  const offlineCount = devices.filter((d) => !d.is_online && !isOpenWhenUsed(d)).length;
  const visible = devices.slice(0, isCompact ? 3 : 8);

  return (
    <div className="ui-card ui-device">
      <div className="ui-device__header">
        <Router size={14} style={{ color: "#5F5F5F" }} />
        <span className="ui-device__header-title">Devices</span>
        <Chip size="sm" variant="soft" style={{ background: "#DCFCE7", color: "#16A34A" }}>
          {onlineCount} online
        </Chip>
        {offlineCount > 0 && (
          <Chip size="sm" variant="soft" style={{ background: "#F5F5F5", color: "#8A8A8A" }}>
            {offlineCount} offline
          </Chip>
        )}
      </div>

      <div className="ui-device__list">
        {visible.map((d, i) => (
          <div key={i} className="ui-device__item">
            <span
              className={`ui-device__dot ${d.is_online ? "ui-device__dot--online" : "ui-device__dot--offline"}`}
              title={openStateLabel(d) ?? (d.is_online ? "Online" : "Offline")}
            />
            <div className="ui-device__icon" style={{ color: d.is_online ? "#5F5F5F" : "#B5B5B5" }}>
              {d.is_online ? <Wifi size={14} /> : <WifiOff size={14} />}
            </div>
            <div className="ui-device__body">
              <span className="ui-device__name">{d.name}</span>
              {d.room && <span className="ui-device__room">{d.room}</span>}
            </div>
            <div className="ui-device__right">
              {d.device_type && (
                <Chip size="sm" variant="soft" style={{ background: "#F5F5F5", color: "#5F5F5F" }}>
                  {deviceTypeIcon(d.device_type)}
                  <span style={{ marginLeft: 3 }}>{d.device_type}</span>
                </Chip>
              )}
              {d.ip && !isCompact && (
                <span className="ui-device__ip">{d.ip}</span>
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

registerMcpCard({
  key: "devices",
  label: "Devices",
  icon: "Router",
  toolPattern: /device|list_registered_device/,
  component: DeviceCard,
  mockTool: "giap-device__list_registered_devices",
  mockData: {
    devices: [
      { name: "Jerry's MacBook", is_online: true, device_type: "laptop", room: "Office", ip: "192.168.1.10" },
      { name: "Jetson Orin Nano", is_online: true, device_type: "server", room: "Office", ip: "192.168.1.20" },
      { name: "iPhone 15 Pro", is_online: true, device_type: "phone", room: "Pocket", ip: "192.168.1.31" },
      { name: "Smart TV", is_online: false, device_type: "tv", room: "Living room" },
      { name: "Home Router", is_online: true, device_type: "router", room: "Hallway", ip: "192.168.1.1" },
    ],
  },
});
