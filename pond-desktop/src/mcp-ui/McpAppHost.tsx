/**
 * Host side of the MCP Apps protocol (JSON-RPC 2.0 over postMessage), in a sandboxed iframe.
 * Spec: https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx
 */

import { useEffect, useRef, useCallback } from "react";
import { X } from "lucide-react";

// ── Types ──────────────────────────────────────────────────────

export interface McpAppHostProps {
  /** Self-contained HTML content of the MCP App */
  html: string;
  /** Tool result to push on mount (from the tool_call that triggered this app) */
  toolResult?: McpToolResult;
  /** Tool input arguments (pushed via ui/notifications/tool-input) */
  toolInput?: Record<string, unknown>;
  /** Tool name that triggered this app */
  toolName?: string;
  theme?: "light" | "dark";
  /** Callback when the app calls a server tool */
  onToolCall?: (name: string, args: Record<string, unknown>) => Promise<McpToolResult>;
  /** Callback when the app updates the model context */
  onUpdateContext?: (content: unknown) => void;
  onOpenUrl?: (url: string) => void;
  /** Callback when the app sends a message to the chat */
  onMessage?: (text: string) => void;
  onClose?: () => void;
  width?: number;
  /** Container height (auto if not set) */
  height?: number;
}

export interface McpToolResult {
  content: Array<{ type: string; text?: string; blob?: string }>;
}

interface JsonRpcRequest {
  jsonrpc: "2.0";
  id?: number | string;
  method: string;
  params?: Record<string, unknown>;
}

// ── Constants ──────────────────────────────────────────────────

const PROTOCOL_VERSION = "2026-01-26";
const EXTENSION_ID = "io.modelcontextprotocol/ui";

/**
 * targetOrigin for messages into the frame: its sandbox makes it opaque, serialising as "null".
 * Inbound, `event.origin` is "null" for every such frame, so `handleMessage` checks window identity.
 */
const APP_FRAME_ORIGIN = "null";

// ── Component ──────────────────────────────────────────────────

export function McpAppHost({
  html,
  toolResult,
  toolInput,
  toolName,
  theme = "light",
  onToolCall,
  onUpdateContext,
  onOpenUrl,
  onMessage,
  onClose,
  width,
  height,
}: McpAppHostProps) {
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const initializedRef = useRef(false);

  const sendResponse = useCallback((id: number | string, result: unknown) => {
    iframeRef.current?.contentWindow?.postMessage(
      { jsonrpc: "2.0", id, result },
      APP_FRAME_ORIGIN,
    );
  }, []);

  const sendError = useCallback((id: number | string, code: number, message: string) => {
    iframeRef.current?.contentWindow?.postMessage(
      { jsonrpc: "2.0", id, error: { code, message } },
      APP_FRAME_ORIGIN,
    );
  }, []);

  const sendNotification = useCallback((method: string, params?: unknown) => {
    iframeRef.current?.contentWindow?.postMessage(
      { jsonrpc: "2.0", method, params },
      APP_FRAME_ORIGIN,
    );
  }, []);

  const handleMessage = useCallback(
    async (event: MessageEvent) => {
      // Only accept messages from our iframe
      if (!iframeRef.current || event.source !== iframeRef.current.contentWindow) return;

      const msg = event.data as JsonRpcRequest;
      if (!msg || msg.jsonrpc !== "2.0" || !msg.method) return;

      switch (msg.method) {
        // ── Initialization handshake ──
        case "ui/initialize": {
          const hostContext = {
            theme,
            locale: navigator.language,
            timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
            platform: "desktop" as const,
            displayMode: "inline" as const,
            availableDisplayModes: ["inline", "fullscreen"],
            ...(toolName ? { toolInfo: { tool: { name: toolName } } } : {}),
            styles: {
              variables: {
                "--primary": "#8C4BFF",
                "--bg": theme === "dark" ? "#1C1C1C" : "#FFFFFF",
                "--fg": theme === "dark" ? "#FFFFFF" : "#1C1C1C",
                "--border": theme === "dark" ? "#3D3D3D" : "#EDEDED",
                "--radius": "10px",
              },
            },
            ...(width || height ? {
              containerDimensions: { width, height },
            } : {}),
          };

          sendResponse(msg.id!, {
            protocolVersion: PROTOCOL_VERSION,
            capabilities: {
              extensions: {
                [EXTENSION_ID]: {
                  mimeTypes: ["text/html;profile=mcp-app"],
                },
              },
            },
            hostContext,
          });

          initializedRef.current = true;

          // Push tool input and result after initialization
          if (toolInput) {
            sendNotification("ui/notifications/tool-input", { arguments: toolInput });
          }
          if (toolResult) {
            sendNotification("ui/notifications/tool-result", toolResult);
          }
          break;
        }

        // ── Tool call from app ──
        case "tools/call": {
          if (!onToolCall || !msg.params) {
            sendError(msg.id!, -32601, "Tool calls not supported");
            break;
          }
          try {
            const result = await onToolCall(
              msg.params.name as string,
              (msg.params.arguments as Record<string, unknown>) ?? {},
            );
            sendResponse(msg.id!, result);
          } catch (err) {
            sendError(msg.id!, -32000, String(err));
          }
          break;
        }

        // ── Open external URL ──
        case "ui/open-link": {
          const url = msg.params?.url as string;
          if (url && onOpenUrl) {
            onOpenUrl(url);
          } else if (url) {
            window.open(url, "_blank", "noopener,noreferrer");
          }
          if (msg.id) sendResponse(msg.id, {});
          break;
        }

        // ── Update model context ──
        case "ui/update-model-context": {
          if (onUpdateContext && msg.params) {
            onUpdateContext(msg.params);
          }
          if (msg.id) sendResponse(msg.id, {});
          break;
        }

        // ── Send message to chat ──
        case "ui/message": {
          const text = (msg.params?.content as { text?: string })?.text;
          if (text && onMessage) {
            onMessage(text);
          }
          if (msg.id) sendResponse(msg.id, {});
          break;
        }

        // ── Display mode request ──
        case "ui/request-display-mode": {
          // For now, acknowledge but stay inline
          if (msg.id) sendResponse(msg.id, { mode: "inline" });
          break;
        }

        // ── Logging from app ──
        case "ui/log": {
          const level = msg.params?.level ?? "info";
          const message = msg.params?.message ?? "";
          if (level === "error") console.error(`[MCP App] ${message}`);
          else console.log(`[MCP App] ${message}`);
          break;
        }

        default: {
          if (msg.id) {
            sendError(msg.id, -32601, `Method not found: ${msg.method}`);
          }
          break;
        }
      }
    },
    [theme, toolName, toolInput, toolResult, width, height, onToolCall, onUpdateContext, onOpenUrl, onMessage, sendResponse, sendError, sendNotification],
  );

  useEffect(() => {
    window.addEventListener("message", handleMessage);
    return () => window.removeEventListener("message", handleMessage);
  }, [handleMessage]);

  // Send teardown on unmount
  useEffect(() => {
    return () => {
      if (initializedRef.current) {
        iframeRef.current?.contentWindow?.postMessage(
          { jsonrpc: "2.0", method: "ui/resource-teardown", params: { reason: "unmount" } },
          APP_FRAME_ORIGIN,
        );
      }
    };
  }, []);

  // Push updated tool results when they change after initialization
  useEffect(() => {
    if (initializedRef.current && toolResult) {
      sendNotification("ui/notifications/tool-result", toolResult);
    }
  }, [toolResult, sendNotification]);

  return (
    <div className="mcp-app-host">
      <div className="mcp-app-host__chrome">
        <span className="mcp-app-host__label">
          {toolName ?? "MCP App"}
        </span>
        {onClose && (
          <button className="mcp-app-host__close" onClick={onClose} aria-label="Close app">
            <X size={14} />
          </button>
        )}
      </div>
      <iframe
        ref={iframeRef}
        srcDoc={html}
        /*
         * `allow-scripts` ONLY: with `allow-same-origin` a `srcdoc` frame gets the host's origin, letting
         * untrusted MCP-server HTML read the tokens in `parent.localStorage` or strip this sandbox. The
         * opaque origin is also the distinct origin MCP Apps (SEP-1865) requires.
         */
        sandbox="allow-scripts"
        className="mcp-app-host__frame"
        style={{
          width: width ?? "100%",
          height: height ?? 400,
          border: "none",
        }}
        title={toolName ?? "MCP App"}
      />
    </div>
  );
}
