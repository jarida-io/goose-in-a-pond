// Launch the real Electron main process and assert the bridge it publishes.
//
// This is what replaces `cargo test --manifest-path pond-desktop/src-tauri/...`
// -- a command the docs told people to run by hand and which no CI job ever
// ran. It proves the things a unit test structurally cannot: that the app
// boots without an unhandled main-process exception, that the window opens,
// that the app:// protocol handler actually serves the built bundle, and that
// the preload exposes exactly the surface the contract declares and nothing
// else.
//
// Deliberately NOT proven here, and left to the manual runbook: the macOS menu
// and therefore the clipboard, any TCC prompt, a real microphone, a real
// sidecar, the .dmg, and whether the tray icon is visible as opposed to merely
// constructed without throwing.

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
    // Node gained a global WebSocket in 22. Electron 44 requires >= 22.12.0
    // anyway, so this only fires on a runtime too old to run the app at all —
    // but a named error beats a bare ReferenceError from inside the harness.
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

// POND_SERVER_BIN points at a path that does not exist, which the resolver
// correctly IGNORES rather than treating as an error -- so on a dev machine
// this still finds target/{release,debug}/pond-server and starts it, while in
// CI there is no such binary and the shell comes up against nothing. Both are
// fine: what is being asserted is that the window renders either way.
//
// `detached` puts Electron in its own process group so the cleanup below can
// signal the WHOLE tree. Without it, SIGKILLing the main process orphans any
// pond-server it spawned: the shell takes its children down on before-quit,
// will-quit, SIGINT/SIGTERM and the exit hook, and SIGKILL is the one signal
// that reaches none of them. That is a genuine property of the app, not a
// harness quirk, and it is why the children carry pidfile reapers.
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

  // The renderer really came from the protocol handler, not from a blank page.
  // React mounts a tick or two after the document loads, so poll rather than
  // sampling once -- a single check here is a flake generator.
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

  // A real IPC round-trip. False is the right answer with no server running;
  // what is being checked is that the call completes at all.
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
