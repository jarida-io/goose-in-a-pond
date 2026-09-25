//! Command-chaining harness: multi-intent voice utterances through the REAL GooseAdapter loop,
//! with builtins wired to recording fakes. Ignored; set GIAP_OLLAMA_URL=http://127.0.0.1:11434.

use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use pond_adapters_goose::giap_registration::register_giap_extensions;
use pond_adapters_goose::GooseAdapter;
use pond_core::shared::domain::agent::{AgentRequest, AgentStreamEvent};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::schedule::{Schedule, ScheduleRun};
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::device_control::{
    DeviceControlOutcome, DeviceControlPort, DeviceStatePatch,
};
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::scheduler::{
    CreateScheduleRequest, SchedulerPort, UpdateScheduleRequest,
};
use pond_core::user_data::ports::settings::SettingsRepository;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

// ── Voice-tuned settings ─────────────────────────────────────────────────────

fn chaining_settings() -> Settings {
    let mut s = Settings::default();
    s.chat_provider = "ollama".to_string();
    s.chat_model = std::env::var("GIAP_CHAINING_MODEL").unwrap_or_else(|_| "llama3.2".to_string());
    // The production voice budget: a chained command must complete inside it.
    s.agent_max_turns = Settings::default().agent_max_turns;
    s.voice_max_turns = Settings::default().voice_max_turns;
    // Only the extensions the scenarios use, to keep 3B-class models deterministic.
    s.ext_device_enabled = true;
    s.ext_schedule_enabled = true;
    s.ext_memory_enabled = false;
    s.ext_weather_enabled = false;
    s.ext_knowledge_enabled = false;
    s.ext_system_enabled = false;
    s
}

struct ChainingSettingsRepo;

#[async_trait]
impl SettingsRepository for ChainingSettingsRepo {
    async fn get(&self) -> Result<Settings> {
        Ok(chaining_settings())
    }
    async fn update(&self, _settings: &Settings) -> Result<()> {
        Ok(())
    }
    async fn get_key(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }
    async fn set_key(&self, _key: &str, _value: String) -> Result<()> {
        Ok(())
    }
}

// ── Recording fakes behind the real MCP tools ────────────────────────────────

/// One smart light, so `set_device_state` has a real target.
struct OneLightRegistry;

fn living_room_light() -> Device {
    Device {
        id: "living-room-light".to_string(),
        name: "Living Room Light".to_string(),
        device_type: "light".to_string(),
        hostname: None,
        ip_address: None,
        capabilities: vec!["power".to_string(), "brightness".to_string()],
        registered_at: "2026-01-01T00:00:00Z".to_string(),
        last_seen: None,
        is_online: true,
        room: Some("Living Room".to_string()),
    }
}

#[async_trait]
impl DeviceRegistry for OneLightRegistry {
    async fn register(&self, _: RegisterDeviceRequest) -> Result<Device> {
        anyhow::bail!("read-only test registry")
    }
    async fn list_devices(&self) -> Result<Vec<Device>> {
        Ok(vec![living_room_light()])
    }
    async fn get_device(&self, id: &str) -> Result<Option<Device>> {
        Ok((id == "living-room-light").then(living_room_light))
    }
    async fn unregister(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

/// Records EVERY control call; pond-core's `RecordingDeviceControl` keeps only the last.
#[derive(Default)]
struct CountingDeviceControl {
    calls: Mutex<Vec<String>>,
}

impl CountingDeviceControl {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn record(&self, call: String) -> DeviceControlOutcome {
        let device_id = call
            .split('(')
            .nth(1)
            .and_then(|s| s.split([',', ')']).next())
            .unwrap_or_default()
            .to_string();
        self.calls.lock().unwrap().push(call);
        DeviceControlOutcome::new(&device_id, DeviceStatePatch::default())
    }
}

#[async_trait]
impl DeviceControlPort for CountingDeviceControl {
    async fn set_power(&self, device_id: &str, on: bool) -> Result<DeviceControlOutcome> {
        Ok(self.record(format!("set_power({device_id}, on={on})")))
    }
    async fn set_brightness(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        Ok(self.record(format!("set_brightness({device_id}, {percent})")))
    }
    async fn set_target_temp(&self, device_id: &str, celsius: f32) -> Result<DeviceControlOutcome> {
        Ok(self.record(format!("set_target_temp({device_id}, {celsius})")))
    }
    async fn set_locked(&self, device_id: &str, locked: bool) -> Result<DeviceControlOutcome> {
        Ok(self.record(format!("set_locked({device_id}, {locked})")))
    }
}

/// In-memory scheduler recording created tasks.
#[derive(Default)]
struct RecordingScheduler {
    created: Mutex<Vec<Schedule>>,
}

impl RecordingScheduler {
    fn created(&self) -> Vec<Schedule> {
        self.created.lock().unwrap().clone()
    }
}

#[async_trait]
impl SchedulerPort for RecordingScheduler {
    async fn create_task(&self, req: CreateScheduleRequest) -> Result<Schedule> {
        let schedule = Schedule {
            id: req.id,
            label: req.label,
            cron: req.cron,
            fire_at: req.fire_at,
            timezone: req.timezone,
            kind: req.kind,
            paused: false,
            currently_running: false,
            last_run: None,
            next_run: None,
            created_at: chrono::Utc::now(),
        };
        self.created.lock().unwrap().push(schedule.clone());
        Ok(schedule)
    }
    async fn list_tasks(&self) -> Result<Vec<Schedule>> {
        Ok(self.created())
    }
    async fn delete_task(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn pause_task(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn resume_task(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn run_now(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn update_task(&self, _id: &str, _req: UpdateScheduleRequest) -> Result<Schedule> {
        anyhow::bail!("not used by the chaining scenarios")
    }
    async fn get_runs(&self, _schedule_id: &str, _limit: u32) -> Result<Vec<ScheduleRun>> {
        Ok(vec![])
    }
    async fn list_upcoming(&self, _limit: u32) -> Result<Vec<Schedule>> {
        Ok(vec![])
    }
    async fn set_executor(
        &self,
        _executor: Arc<dyn pond_core::user_data::ports::schedule_execution::ScheduleExecutor>,
    ) -> Result<()> {
        Ok(())
    }
}

// ── Harness plumbing ─────────────────────────────────────────────────────────

/// Fakes the MCP tools dispatch into, registered once per binary (Goose's registry is global).
struct Recorders {
    device_control: Arc<CountingDeviceControl>,
    scheduler: Arc<RecordingScheduler>,
}

static RECORDERS: OnceLock<Recorders> = OnceLock::new();

fn recorders() -> &'static Recorders {
    RECORDERS.get_or_init(|| {
        let device_control = Arc::new(CountingDeviceControl::default());
        let scheduler = Arc::new(RecordingScheduler::default());
        register_giap_extensions(
            &chaining_settings(),
            Arc::new(pond_core::user_data::mocks::mock_memory::MockMemoryRepository::default()),
            None,
            Some(scheduler.clone()),
            None,
            Arc::new(ChainingSettingsRepo),
            Arc::new(OneLightRegistry),
            Arc::new(pond_core::user_data::mocks::mock_skill::MockSkillRepository::default()),
            device_control.clone(),
            None,
        )
        .expect("register giap extensions for the chaining harness");
        Recorders {
            device_control,
            scheduler,
        }
    })
}

/// Everything observed while draining one scripted utterance through the loop.
struct ChainRun {
    tool_calls: Vec<String>,
    spoken_summary: String,
    elapsed_secs: f64,
}

async fn run_utterance(session_id: &str, utterance: &str) -> ChainRun {
    let url =
        std::env::var("GIAP_OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    std::env::set_var("OLLAMA_HOST", &url);

    let adapter = GooseAdapter::new(
        Arc::new(ChainingSettingsRepo),
        Arc::new(
            pond_core::user_data::mocks::mock_prompt_template::MockPromptTemplateRepository::default(),
        ),
        Arc::new(pond_core::user_data::mocks::mock_prompt_extra::MockPromptExtraRepository::default()),
        Arc::new(pond_core::user_data::mocks::mock_skill::MockSkillRepository::default()),
        Arc::new(pond_core::user_data::mocks::mock_memory::MockMemoryRepository::default()),
        url,
        None,
        None,
    )
    .await
    .expect("construct GooseAdapter");

    let request = AgentRequest {
        message: utterance.to_string(),
        session_id: session_id.to_string(),
        model_role: "task".to_string(),
        images: Vec::new(),
        // A VOICE request, so the loop runs under the tuned `voice_max_turns` cap.
        voice_mode: true,
        canvas_mode: false,
        // Household is what a test with no speaker means.
        profile_scope: ProfileScope::Household,
        profile_context: None,
        tool_group_allowlist: None,
        warmup: false,
    };

    let started = Instant::now();
    let mut stream = adapter.chat_stream(request).await.expect("agent stream");

    let mut tool_calls = Vec::new();
    let mut spoken_summary = String::new();
    while let Some(event_result) = stream.next().await {
        match event_result.expect("stream event") {
            AgentStreamEvent::ToolCall { tool, .. } => tool_calls.push(tool),
            AgentStreamEvent::Text { content } => spoken_summary.push_str(&content),
            AgentStreamEvent::Error { content } => panic!("agent loop error: {content}"),
            _ => {}
        }
    }

    let run = ChainRun {
        tool_calls,
        spoken_summary,
        elapsed_secs: started.elapsed().as_secs_f64(),
    };
    println!(
        "[chaining] utterance={utterance:?}\n[chaining]   tool_calls={:?}\n[chaining]   \
         latency={:.1}s (voice_max_turns={})\n[chaining]   summary={:?}",
        run.tool_calls,
        run.elapsed_secs,
        chaining_settings().voice_max_turns,
        run.spoken_summary,
    );
    run
}

fn summary_mentions(summary: &str, any_of: &[&str]) -> bool {
    let lower = summary.to_lowercase();
    any_of.iter().any(|needle| lower.contains(needle))
}

// ── Scenarios ────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires live Ollama agent at GIAP_OLLAMA_URL"]
async fn chained_light_and_alarm_executes_both_and_summarises() {
    let recorders = recorders();
    let run = run_utterance(
        "chaining-light-alarm",
        "Turn off the living room light and set an alarm for 7am tomorrow.",
    )
    .await;

    // 1. One tool call per sub-intent.
    assert!(
        run.tool_calls.len() >= 2,
        "expected at least 2 tool calls (device + schedule), got {:?}",
        run.tool_calls
    );
    assert!(
        run.tool_calls
            .iter()
            .any(|t| t.contains("set_device_state")),
        "no device-control call in {:?}",
        run.tool_calls
    );
    assert!(
        run.tool_calls.iter().any(|t| t.contains("create_schedule")),
        "no schedule creation in {:?}",
        run.tool_calls
    );

    // 2. The side effects really happened.
    assert!(
        recorders
            .device_control
            .calls()
            .iter()
            .any(|c| c.contains("set_power") && c.contains("on=false")),
        "light was never powered off: {:?}",
        recorders.device_control.calls()
    );
    assert!(
        !recorders.scheduler.created().is_empty(),
        "no schedule was created"
    );

    // 3. The spoken summary covers BOTH sub-actions.
    assert!(
        summary_mentions(&run.spoken_summary, &["light"]),
        "summary never mentions the light: {:?}",
        run.spoken_summary
    );
    assert!(
        summary_mentions(&run.spoken_summary, &["alarm", "7", "seven"]),
        "summary never mentions the alarm: {:?}",
        run.spoken_summary
    );
}

#[tokio::test]
#[ignore = "requires live Ollama agent at GIAP_OLLAMA_URL"]
async fn chained_dim_and_reminder_executes_both_and_summarises() {
    let recorders = recorders();
    let run = run_utterance(
        "chaining-dim-reminder",
        "Dim the living room light to 20 percent and remind me to water the plants at 6pm.",
    )
    .await;

    assert!(
        run.tool_calls.len() >= 2,
        "expected at least 2 tool calls, got {:?}",
        run.tool_calls
    );
    assert!(
        recorders
            .device_control
            .calls()
            .iter()
            .any(|c| c.contains("set_brightness")),
        "brightness was never set: {:?}",
        recorders.device_control.calls()
    );
    assert!(
        !recorders.scheduler.created().is_empty(),
        "no reminder schedule was created"
    );
    assert!(
        summary_mentions(&run.spoken_summary, &["light", "dim", "20"]),
        "summary never mentions the light: {:?}",
        run.spoken_summary
    );
    assert!(
        summary_mentions(&run.spoken_summary, &["remind", "water", "plant", "6"]),
        "summary never mentions the reminder: {:?}",
        run.spoken_summary
    );
}
