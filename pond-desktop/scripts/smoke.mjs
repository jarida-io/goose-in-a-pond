// Boots the real Electron app: window opens, app:// serves the bundle, preload surface is exact.
// Not covered (manual runbook): macOS menu/clipboard, TCC, mic, real sidecar, .dmg, tray visibility.

import { spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import electron from "electron";

const HERE = dirname(fileURLToPath(import.meta.url));
const APP_DIR = join(HERE, "..");
const PORT = 9333;

/** Expected preload surface. A change here should be a deliberate edit. */
const EXPECTED_BRIDGE_KEYS = ["serverUrl", "invoke", "listen"];

function fail(message) {
  console.error(`SMOKE FAIL: ${message}`);
  process.exitCode = 1;
}

/** Poll the devtools endpoint until the page target appears. */
async function waitForPage(timeoutMs = 45_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${PORT}/json`);
      const targets = await res.json();
      const page = targets.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
      if (page) return page;
    } catch {
      // Not listening yet.
    }
    await delay(500);
  }
  return null;
}

/** Minimal CDP client: enough to evaluate expressions in the page. */
async function connect(page) {
  if (typeof WebSocket === "undefined") {
    // Only a Node too old to run the app hits this; a named error beats a bare ReferenceError.
    throw new Error(
      `this script needs a global WebSocket, which Node gained in 22 (running ${process.version}). ` +
        "electron@44 declares engines >= 22.12.0, so upgrade rather than polyfill.",
    );
  }
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  const pending = new Map();
  let id = 0;
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) {
      pending.get(msg.id)(msg.result);
      pending.delete(msg.id);
    }
  };
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
  });
  return {
    async evaluate(expression) {
      const result = await new Promise((resolve) => {
        const i = ++id;
        pending.set(i, resolve);
        ws.send(
          JSON.stringify({
            id: i,
            method: "Runtime.evaluate",
            params: { expression, awaitPromise: true, returnByValue: true },
          }),
        );
      });
      if (result?.exceptionDetails) throw new Error(result.exceptionDetails.text);
      return result?.result?.value;
    },
    close: () => ws.close(),
  };
}

// A nonexistent POND_SERVER_BIN is ignored: dev finds target/*/pond-server, CI runs serverless.
// `detached` = own process group, so the final SIGKILL also reaches any spawned pond-server.
const child = spawn(electron, [APP_DIR, `--remote-debugging-port=${PORT}`], {
  env: { ...process.env, POND_SERVER_BIN: "/nonexistent-on-purpose" },
  stdio: ["ignore", "pipe", "pipe"],
  detached: true,
});

let mainOutput = "";
child.stdout.on("data", (d) => (mainOutput += d));
child.stderr.on("data", (d) => (mainOutput += d));
child.on("exit", (code) => {
  if (code !== 0 && code !== null) fail(`the main process exited early with code ${code}`);
});

try {
  const page = await waitForPage();
  if (!page) {
    console.error(mainOutput);
    fail("no page target appeared; the app did not open a window");
    process.exit(1);
  }

  if (!page.url.startsWith("app://")) {
    fail(`the window loaded ${page.url}, expected an app:// URL`);
  }
  console.log(`ok  window open at ${page.url}`);

  const cdp = await connect(page);

  // React mounts a tick or two after load, so poll; one sample is flaky.
  const title = await cdp.evaluate("document.title");
  let rootMounted = false;
  for (let i = 0; i < 40 && !rootMounted; i++) {
    rootMounted = await cdp.evaluate("!!document.querySelector('#root')?.children.length");
    if (!rootMounted) await delay(250);
  }
  if (!rootMounted) fail("the page has no rendered content; app:// served nothing useful");
  else console.log(`ok  renderer mounted${title ? ` (title: ${title})` : ""}`);

  const origin = await cdp.evaluate("window.location.origin");
  console.log(`ok  origin is ${origin}`);

  const keys = await cdp.evaluate("Object.keys(window.giap ?? {})");
  if (JSON.stringify(keys) !== JSON.stringify(EXPECTED_BRIDGE_KEYS)) {
    fail(`bridge surface is ${JSON.stringify(keys)}, expected ${JSON.stringify(EXPECTED_BRIDGE_KEYS)}`);
  } else {
    console.log(`ok  bridge exposes exactly ${keys.join(", ")}`);
  }

  const serverUrl = await cdp.evaluate("window.__GIAP_SERVER_URL__");
  if (typeof serverUrl !== "string" || !serverUrl.startsWith("http")) {
    fail(`__GIAP_SERVER_URL__ is ${JSON.stringify(serverUrl)}; PondApiClient reads it at module load`);
  } else {
    console.log(`ok  __GIAP_SERVER_URL__ injected (${serverUrl})`);
  }

  // Node must not be reachable from the renderer.
  const isolated = await cdp.evaluate(
    "typeof require === 'undefined' && typeof process === 'undefined'",
  );
  if (!isolated) fail("node is reachable from the renderer; context isolation is not in effect");
  else console.log("ok  renderer is isolated from node");

  // The preload's allowlists are the security boundary, so prove they bite.
  const badCommand = await cdp.evaluate(
    "window.giap.invoke('definitely_not_a_command').then(() => 'ACCEPTED').catch((e) => e.message)",
  );
  if (!String(badCommand).includes("unknown shell command")) {
    fail(`an unknown command was not refused: ${badCommand}`);
  } else {
    console.log("ok  unknown command refused by the preload");
  }

  const badEvent = await cdp.evaluate(
    "(() => { try { window.giap.listen('definitely_not_an_event', () => {}); return 'ACCEPTED'; } catch (e) { return e.message; } })()",
  );
  if (!String(badEvent).includes("unknown shell event")) {
    fail(`an unknown event was not refused: ${badEvent}`);
  } else {
    console.log("ok  unknown event refused by the preload");
  }

  // Real IPC round-trip; false is fine with no server, only completion is checked.
  const health = await cdp.evaluate("window.giap.invoke('server_health')");
  if (typeof health !== "boolean") {
    fail(`server_health returned ${JSON.stringify(health)}, expected a boolean`);
  } else {
    console.log(`ok  IPC round-trip works (server_health -> ${health})`);
  }

  cdp.close();
} finally {
  // Negative pid signals the process group, so the sidecar goes with the shell.
  try {
    if (child.pid) process.kill(-child.pid, "SIGKILL");
  } catch {
    // Already gone.
  }
}

if (process.exitCode) {
  console.error("\n--- main process output ---\n" + mainOutput);
} else {
  console.log("\nsmoke passed");
}
