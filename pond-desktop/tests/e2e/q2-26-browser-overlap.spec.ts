/**
 * Q2-26 verification — confirms the browser fallback's recordWithVad()
 * fires the transcribe request the moment silence starts, overlapping it
 * with the rest of the silence-confirmation wait, instead of waiting for
 * confirmation (silenceTimeoutMs) before starting it. Also confirms:
 *   - exactly one transcribe call (speculative result reused by runPipeline)
 *   - chat/stream fires before silence confirmation (speculative LLM overlap)
 *   - TTFT console log emitted with "speculative" marker
 */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";

const MOCK_SETTINGS = {
  assistant_name: "Pond",
  user_name: "Jerry",
  chat_provider: "llamafile",
  chat_model: "llama3.2",
  agent_memory_inject: false,
  prompt_style: "balanced",
  voice_wake_word: "", // no wake word -> idle state shows "Start Listening"
  active_whisper_model: "ggml-base.bin",
  voice_tts_voice: "en_US-lessac-medium.onnx",
};

const SILENCE_TIMEOUT_MS = 400; // matches DEFAULT_VAD_CONFIG.silenceTimeoutMs

async function setupRoutes(
  page: Page,
  onTranscribe: () => void,
  onChatStream: () => void = () => {},
) {
  await page.route("**/api/v1/health", (r) => r.fulfill({ json: { status: "ok", version: "test" } }));
  await page.route("**/api/v1/handshake", (r) => r.fulfill({ json: { token: "e2e-test-token", session_id: "e2e-session" } }));
  await page.route("**/api/v1/onboard/status", (r) =>
    r.fulfill({ json: { onboarded: true, current_step: "Completed", steps_completed: 9, total_steps: 9 } }),
  );
  await page.route("**/api/v1/sessions", (r) => r.fulfill({ json: { sessions: [] } }));
  await page.route("**/api/v1/sessions/*/messages", (r) => r.fulfill({ json: { messages: [] } }));
  await page.route("**/api/v1/devices", (r) => r.fulfill({ json: { devices: [] } }));
  await page.route("**/api/v1/memories", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/schedules", (r) => r.fulfill({ json: [] }));
  await page.route("**/api/v1/settings", (r) => r.fulfill({ json: MOCK_SETTINGS }));
  await page.route("**/api/v1/tts", (r) => r.fulfill({ status: 503, json: { error: "off" } }));

  // Chat stream mock — returns one text token so TTFT console.info fires
  await page.route("**/api/v1/chat/stream", (r) => {
    onChatStream();
    return r.fulfill({
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
      body: 'data: {"type":"text","content":"ok"}\ndata: {"done":true}\n\n',
    });
  });

  await page.route("**/api/v1/transcribe", async (r) => {
    onTranscribe();
    await new Promise((res) => setTimeout(res, 200)); // simulate realistic whisper latency
    return r.fulfill({ json: { text: "hello goose" } });
  });
}

async function mockMic(page: Page) {
  await page.addInitScript(() => {
    const realGetUserMedia = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    let activeGain: GainNode | null = null;
    (window as unknown as { __setMicGain: (v: number) => void }).__setMicGain = (v: number) => {
      if (activeGain) activeGain.gain.value = v;
    };
    navigator.mediaDevices.getUserMedia = async (constraints?: MediaStreamConstraints) => {
      if (!constraints?.audio) return realGetUserMedia(constraints);
      const ctx = new AudioContext();
      const osc = ctx.createOscillator();
      osc.frequency.value = 440;
      const gain = ctx.createGain();
      gain.gain.value = 0;
      activeGain = gain;
      osc.connect(gain);
      const dest = ctx.createMediaStreamDestination();
      gain.connect(dest);
      osc.start();
      await ctx.resume();
      return dest.stream;
    };
  });
}

test.describe("Q2-26 — browser fallback speculative overlap", () => {
  test("transcribe fires on first silence dip, not after confirmation; exactly once", async ({ page }) => {
    const transcribeTimestamps: number[] = [];
    const chatStreamTimestamps: number[] = [];

    // Capture [Q2-26 TTFT] console.info lines emitted by WebVoiceBackend.streamChat
    const ttftLogs: string[] = [];
    page.on("console", (msg) => {
      const t = msg.text();
      if (t.includes("[Q2-26 TTFT]")) ttftLogs.push(t);
    });

    await setupRoutes(
      page,
      () => transcribeTimestamps.push(Date.now()),
      () => chatStreamTimestamps.push(Date.now()),
    );
    await mockMic(page);
    await page.addInitScript(() => localStorage.setItem("giap-mode", "voice"));

    await page.goto("/");
    const startBtn = page.getByRole("button", { name: /Start Listening/i });
    await expect(startBtn).toBeVisible({ timeout: 10_000 });
    await startBtn.click();

    // Speak for 300ms (well past the 60ms onset confirmation).
    await page.evaluate(() => (window as unknown as { __setMicGain: (v: number) => void }).__setMicGain(0.05));
    await page.waitForTimeout(300);

    const silenceStart = Date.now();
    await page.evaluate(() => (window as unknown as { __setMicGain: (v: number) => void }).__setMicGain(0));

    // Give the pipeline time to fully resolve (silence confirmation + mocked
    // chat/TTS), then check what happened.
    await expect(page.getByText("hello goose")).toBeVisible({ timeout: 5_000 });

    // ── ASR speculation assertions ─────────────────────────────────────────
    expect(transcribeTimestamps, "exactly one transcribe call — speculative result must be reused, not refetched").toHaveLength(1);

    const asrFireDelay = transcribeTimestamps[0] - silenceStart;
    console.log(`transcribe fired ${asrFireDelay}ms after silence started (confirmation window is ${SILENCE_TIMEOUT_MS}ms)`);
    expect(asrFireDelay, "transcribe must fire near silence onset, not after the confirmation wait").toBeLessThan(SILENCE_TIMEOUT_MS - 150);

    // ── LLM speculation assertions ─────────────────────────────────────────
    // The speculative LLM fires in the browser when ASR resolves (mid-window).
    // The chat/stream mock captures the timestamp, which should be well before
    // the full silence-confirmation window closes.
    expect(chatStreamTimestamps, "exactly one chat/stream call").toHaveLength(1);

    const llmFireDelay = chatStreamTimestamps[0] - silenceStart;
    console.log(`chat/stream fired ${llmFireDelay}ms after silence started (confirmation window is ${SILENCE_TIMEOUT_MS}ms)`);
    expect(llmFireDelay, "speculative LLM must fire before silence confirmation window closes").toBeLessThan(SILENCE_TIMEOUT_MS);

    // ── TTFT console log ────────────────────────────────────────────────────
    expect(ttftLogs.length, "TTFT log must be emitted at least once").toBeGreaterThan(0);
    expect(ttftLogs[0], "TTFT log must carry speculative marker").toContain("speculative");
  });
});
