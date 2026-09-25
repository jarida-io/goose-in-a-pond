// Minimal MCP Apps (io.modelcontextprotocol/ui) client: JSON-RPC 2.0 over postMessage. Spec:
// https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx

const PROTOCOL_VERSION = "2026-01-26";

class McpApp {
  constructor({ name, version }) {
    this._name = name;
    this._version = version || "1.0.0";
    this._nextId = 1;
    this._pending = new Map(); // id -> { resolve, reject }
    this._hostContext = null;
    this._connected = false;

    // Event handlers — set by the card implementation
    this.ontoolresult = null;   // (result) => void
    this.ontoolinput = null;    // (input) => void
    this.oncontextchanged = null; // (context) => void
    this.onteardown = null;     // (reason) => void
  }

  /** Start the connection — sends ui/initialize to the host */
  connect() {
    window.addEventListener("message", (ev) => this._handleMessage(ev));

    const id = this._nextId++;
    const initPromise = new Promise((resolve, reject) => {
      this._pending.set(id, { resolve, reject });
    });

    window.parent.postMessage({
      jsonrpc: "2.0",
      id,
      method: "ui/initialize",
      params: {
        appCapabilities: {
          availableDisplayModes: ["inline"],
          tools: { listChanged: false },
        },
        clientInfo: { name: this._name, version: this._version },
        protocolVersion: PROTOCOL_VERSION,
      },
    }, "*");

    initPromise.then((result) => {
      this._hostContext = result.hostContext || {};
      this._connected = true;
      if (this._hostContext.styles?.variables) {
        const root = document.documentElement;
        for (const [key, val] of Object.entries(this._hostContext.styles.variables)) {
          root.style.setProperty(key, val);
        }
      }
    }).catch((err) => {
      console.error("[MCP App] Init failed:", err);
    });

    return initPromise;
  }

  async callServerTool(name, args) {
    const id = this._nextId++;
    return new Promise((resolve, reject) => {
      this._pending.set(id, { resolve, reject });
      window.parent.postMessage({
        jsonrpc: "2.0",
        id,
        method: "tools/call",
        params: { name, arguments: args || {} },
      }, "*");
    });
  }

  /** Open a URL in the host browser */
  openLink(url) {
    window.parent.postMessage({
      jsonrpc: "2.0",
      method: "ui/open-link",
      params: { url },
    }, "*");
  }

  /** Send a chat message to the host */
  sendMessage(text) {
    window.parent.postMessage({
      jsonrpc: "2.0",
      method: "ui/message",
      params: { content: { text } },
    }, "*");
  }

  reportSize(width, height) {
    window.parent.postMessage({
      jsonrpc: "2.0",
      method: "ui/notifications/size-changed",
      params: { width, height },
    }, "*");
  }

  get hostContext() { return this._hostContext; }
  get theme() { return this._hostContext?.theme || "light"; }
  get locale() { return this._hostContext?.locale || "en"; }
  get timeZone() { return this._hostContext?.timeZone || "UTC"; }

  _handleMessage(event) {
    const msg = event.data;
    if (!msg || msg.jsonrpc !== "2.0") return;

    if (msg.id != null && this._pending.has(msg.id)) {
      const { resolve, reject } = this._pending.get(msg.id);
      this._pending.delete(msg.id);
      if (msg.error) reject(msg.error);
      else resolve(msg.result);
      return;
    }

    switch (msg.method) {
      case "ui/notifications/tool-result":
        if (this.ontoolresult) this.ontoolresult(msg.params);
        break;
      case "ui/notifications/tool-input":
        if (this.ontoolinput) this.ontoolinput(msg.params);
        break;
      case "ui/notifications/host-context-changed":
        this._hostContext = { ...this._hostContext, ...msg.params };
        if (this.oncontextchanged) this.oncontextchanged(this._hostContext);
        break;
      case "ui/resource-teardown":
        if (this.onteardown) this.onteardown(msg.params?.reason);
        break;
    }
  }
}

// Export to window for inline script use
window.McpApp = McpApp;
