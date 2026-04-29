import { useState, useEffect, useCallback } from "react";
import { Button, Card, CardContent, Chip, Input, Switch, Tabs } from "@heroui/react";
import { RefreshCw, Download, Search } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { LogEntry } from "../api/types";

type LevelFilter = "ALL" | "INFO" | "WARN" | "ERROR";

export function Logs() {
  const [entries, setEntries]       = useState<LogEntry[]>([]);
  const [level, setLevel]           = useState<LevelFilter>("ALL");
  const [search, setSearch]         = useState("");
  const [autoscroll, setAutoscroll] = useState(true);
  const [loading, setLoading]       = useState(true);
  const [error, setError]           = useState<string | null>(null);

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    api.listLogs({ limit: 200, level: level === "ALL" ? undefined : level })
      .then(setEntries)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [level]);

  useEffect(() => { load(); }, [load]);

  // Auto-refresh every 10s
  useEffect(() => {
    const id = setInterval(load, 10_000);
    return () => clearInterval(id);
  }, [load]);

  function downloadCsv() {
    const url = api.exportLogsUrl();
    const a = document.createElement("a");
    a.href = url;
    a.download = "pond-logs.csv";
    a.click();
  }

  const filtered = entries.filter((e) => {
    if (search && !(e.message + e.source).toLowerCase().includes(search.toLowerCase())) return false;
    return true;
  });

  function levelColor(l: string): "danger" | "warning" | "default" {
    switch (l.toUpperCase()) {
      case "ERROR": return "danger";
      case "WARN":  return "warning";
      default:      return "default";
    }
  }

  return (
    <div className="screen screen--logs">
      <div className="page-header">
        <h1 className="page-header__title">Logs</h1>
      </div>

      {/* Toolbar */}
      <div className="logs-toolbar">
        <Tabs
          selectedKey={level}
          onSelectionChange={(k) => setLevel(String(k) as LevelFilter)}
        >
          <Tabs.ListContainer>
            <Tabs.List aria-label="Log level filter">
              {(["ALL", "INFO", "WARN", "ERROR"] as const).map((l) => (
                <Tabs.Tab key={l} id={l} onClick={() => setLevel(l)}>
                  <Tabs.Indicator />
                  {l === "ALL" ? "All" : l.charAt(0) + l.slice(1).toLowerCase()}
                </Tabs.Tab>
              ))}
            </Tabs.List>
          </Tabs.ListContainer>
        </Tabs>

        <div className="logs-toolbar__right">
          <Input
            size="sm"
            radius="md"
            variant="bordered"
            placeholder="Search messages…"
            value={search}
            onValueChange={setSearch}
            startContent={<Search size={14} />}
            className="logs-toolbar__search"
          />
          <Switch
            size="sm"
            color="secondary"
            isSelected={autoscroll}
            onValueChange={setAutoscroll}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            <span className="muted-12">Autoscroll</span>
          </Switch>
          <Button size="sm" variant="light" onPress={load} isDisabled={loading} startContent={<RefreshCw size={14} />}>
            Refresh
          </Button>
          <Button size="sm" variant="bordered" radius="md" onPress={downloadCsv} startContent={<Download size={14} />}>
            Download CSV
          </Button>
        </div>
      </div>

      {error && <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 }}>{error}</p>}

      {/* Log table */}
      <Card shadow="none" className="giap-card logs-table">
        <CardContent className="card-body--flush">
          <div className="logs-table__head">
            <div>Timestamp</div>
            <div>Level</div>
            <div>Source</div>
            <div>Message</div>
          </div>
          <div>
            {loading && entries.length === 0 && (
              <div className="empty-state empty-state--inline">
                <span>Loading logs\u2026</span>
              </div>
            )}
            {!loading && filtered.length === 0 && (
              <div className="empty-state empty-state--inline">
                <span>No log entries {search ? "match" : "found"}.</span>
              </div>
            )}
            {filtered.map((e) => (
              <div className="logs-table__row" key={e.id}>
                <div className="logs-table__ts"><code>{formatTs(e.timestamp)}</code></div>
                <div>
                  <Chip size="sm" variant="flat" color={levelColor(e.level)}>
                    {e.level}
                  </Chip>
                </div>
                <div className="logs-table__src"><code>{e.source}</code></div>
                <div className="logs-table__msg">
                  {e.message}
                  {e.metadata && (
                    <code style={{ display: "block", marginTop: 2, fontSize: "10px", color: "var(--grey-500)", wordBreak: "break-all" }}>
                      {truncate(e.metadata, 120)}
                    </code>
                  )}
                </div>
              </div>
            ))}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

// ── Helpers ───────────────────────────────────────────────────

function formatTs(ts: string): string {
  try {
    const d = new Date(ts);
    return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  } catch {
    return ts;
  }
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n) + "\u2026" : s;
}
