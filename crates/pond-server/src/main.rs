//! Goose In A Pond — Server Entry Point
//!
//! Usage:
//!   pond-server setup [--model tiny|base|small]
//!   pond-server serve [--port PORT] [--open]
//!   pond-server chat  [--provider mock|llamafile|ollama] [--model MODEL] [--input stdin|whisper] [--whisper-url URL]
//!   pond-server status
//!
//! # TODO — Setup Script
//! - [ ] Create a setup script (`scripts/setup.sh`) that:
//!   1. Detects whether the device is dedicated (sole GIAP) or shared
//!   2. If dedicated: configures `http://pond.local/{route}` (port 80)
//!   3. If shared:    configures `http://pond.<HOSTNAME>.local:<PORT>/{route}`
//!   4. Preferred port order: 80 → 8080 → 4000 → 5000
//!   5. Sets up mDNS/Avahi for `.local` hostname resolution
//!   6. Creates systemd service for auto-start on boot
//!   7. Initializes databases at a configurable data directory
//!   8. Prompts for initial onboarding if not yet done

mod filesystem_model_storage;
mod composite_model_catalog_provider;
mod http_model_downloader;
mod llamafile_process;
mod model_download;
mod piper_http;
mod piper_process;
mod ports;
mod reqwest_model_downloader;
mod startup;
mod system_deps;
mod whisper_process;

use anyhow::Result;
use clap::{Parser, Subcommand};
use pond_adapters_llamafile::LlamafileProvider;
use pond_adapters_ollama::OllamaProvider;
use pond_adapters_piper::PiperOutput;
use pond_adapters_speaker_embed::OnnxSpeakerAdapter;
use pond_adapters_whisper::{self, WhisperInput, WhisperKeywordDetector};
use pond_core::ports::speaker_id::SpeakerIdentification;
use pond_core::ports::wake_word::WakeWordDetector;
use pond_core::ports::voice_input::VoiceInput;
use pond_core::ports::voice_output::VoiceOutput;
use pond_core::services::instant_activation::InstantActivation;
use pond_core::services::print_output::PrintOutput;
use pond_api::{AppState, LlamafileManager};
use pond_core::ports::agent::Agent;
use pond_core::ports::provider::LlmProvider;
use pond_core::ports::session_storage::SessionStorage;
use pond_core::ports::mcp_server::McpServerRepository as _;
use pond_core::ports::settings::SettingsRepository as _;
use pond_core::prompts::build_system_prompt;
use pond_core::services::chat::ChatService;
use pond_core::services::model_router::ModelRouter;
use pond_core::services::mock_agent::MockAgent;
use pond_core::services::stdin_input::StdinInput;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra_scheduler::{CronSchedulerAdapter, WebhookTaskExecutor};
use pond_adapters_weather::{OpenMeteoWeatherAdapter, WeatherProvider};
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_memory::SqliteMemoryRepository;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_sensor::{SqliteCameraStorage, SqliteSensorStorage};
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_mcp_servers::SqliteMcpServerRepository;
use pond_infra::sqlite_model_repository::SqliteModelRepository;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use pond_infra::sqlite_event_log::SqliteEventLogRepository;
use pond_core::domain::model_record::{ModelCategory, ModelRecord};
use pond_core::ports::model_repository::ModelRepository;
use std::sync::Arc;
use pond_core::services::onboarding::OnboardingService;
use pond_core::domain::onboarding::OnboardingStep;
use pond_infra::onboarding::SqlxOnboardingRepository;
use futures::StreamExt as _;
use std::io::{self, Write};
use std::collections::HashMap;
use std::net::UdpSocket;

#[derive(Parser)]
#[command(name = "pond")]
#[command(about = "🦆 Goose In A Pond — Local AI Home Assistant")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// First-time setup: initialize databases and download the Whisper ASR model
    Setup {
        /// Whisper model size: tiny (~39 MB), base (~74 MB, default), small (~244 MB)
        #[arg(long, default_value = "base")]
        model: String,
    },

    /// Start the HTTP server (REST API + web dashboard)
    Serve {
        /// Path to the built web dashboard assets (run `cd web && npm run build` first)
        #[arg(long, default_value = "web/dist")]
        static_dir: std::path::PathBuf,

        /// Open the dashboard in the browser
        #[arg(long)]
        open: bool,

        /// Enable debug logging
        #[arg(long)]
        debug: bool,

        /// Agent backend: goose (default, Block's Goose with MCP tool calls) or mock (fast, no LLM).
        /// Override: cargo run -p pond-server -- serve --agent mock
        #[arg(long, default_value = "goose")]
        agent: String,

        /// Also launch the native Tauri desktop app after the server starts.
        /// Searches for the binary in pond-desktop/src-tauri/target/debug/ and
        /// pond-desktop/src-tauri/target/release/bundle/macos/.
        #[arg(long)]
        native: bool,
    },

    /// Interactive CLI chat (Wait→Listen→Think→Speak loop)
    Chat {
        /// LLM provider: mock, llamafile, ollama, or local (GGUF in-process, requires --features local-inference).
        /// Defaults to the value stored in Settings (llm_provider field).
        #[arg(short = 'P', long)]
        provider: Option<String>,

        /// Model name (only used when --provider ollama, e.g. "llama3.2", "gemma2")
        #[arg(short = 'M', long)]
        model: Option<String>,

        /// Input source: stdin (text) or whisper (microphone → ASR)
        #[arg(short = 'I', long, default_value = "stdin")]
        input: String,

        /// Enable voice-based wake word detection (requires --input whisper).
        /// Say the trigger phrase to activate the assistant before each turn.
        /// Defaults to the wake word stored in Settings.
        #[arg(long)]
        wake_word: Option<String>,

        /// Disable wake word detection (jump straight to listen on each turn).
        #[arg(long)]
        no_wake_word: bool,

        /// Text-to-speech engine: piper, or none (print only).
        /// Defaults to the active TTS model stored in Settings.
        #[arg(long)]
        tts: Option<String>,

        /// Path to the Piper voice model (.onnx file). Defaults to $DATA_DIR/models/tts/en_US-lessac-medium.onnx
        #[arg(long)]
        tts_model: Option<std::path::PathBuf>,

        /// Path to the x-vector speaker ONNX model for speaker identification.
        /// Defaults to $DATA_DIR/models/speaker.onnx if that file exists.
        #[arg(long)]
        speaker_model: Option<std::path::PathBuf>,
    },

    /// Enrol your voice for a profile — records 3 microphone samples automatically
    Enroll {
        /// Profile ID to link the voice embedding to
        #[arg(short, long)]
        profile: String,

        /// How many seconds to record per sample (default: 10)
        #[arg(long, default_value = "10")]
        duration: u32,

        /// Path to the x-vector speaker ONNX model.
        /// Defaults to $DATA_DIR/models/speaker.onnx
        #[arg(long)]
        speaker_model: Option<std::path::PathBuf>,
    },

    /// Show system status
    Status,

    /// Run interactive onboarding wizard
    Onboard {
        /// Reset and restart onboarding from scratch
        #[arg(long)]
        reset: bool,
    },

    /// Browse and manage AI models
    Models {
        #[command(subcommand)]
        action: ModelAction,
    },

    /// One-shot agent chat via the Goose agentic loop (no voice I/O)
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },

    /// Manage system prompt templates (DB-stored, editable at runtime)
    Prompts {
        #[command(subcommand)]
        action: PromptAction,
    },

    /// Manage user skills injected into the agent system prompt
    Skills {
        #[command(subcommand)]
        action: SkillAction,
    },

    /// Manage agent recipes (Goose YAML automations stored in DB)
    Recipes {
        #[command(subcommand)]
        action: RecipeAction,
    },

    /// Manage persistent memory fragments
    Memories {
        #[command(subcommand)]
        action: MemoryAction,
    },

    /// Calibrate the wake-word detector by recording samples of your activation phrase.
    ///
    /// Records several short clips of you saying your wake-word phrase and stores
    /// Whisper's transcriptions as calibration variants. The detector will then match
    /// against any of those variants, making it robust to Whisper's inconsistent output.
    ///
    /// Example:
    ///   pond-server calibrate
    ///   pond-server calibrate --phrase "hey pond" --samples 3
    Calibrate {
        /// The wake-word phrase to calibrate (defaults to voice_wake_word from settings)
        #[arg(long)]
        phrase: Option<String>,

        /// Number of recording samples to collect (default: 5)
        #[arg(long, default_value = "5")]
        samples: usize,

        /// Whisper server URL (defaults to voice_whisper_url from settings)
        #[arg(long)]
        whisper_url: Option<String>,

        /// Clear any existing calibration data before starting
        #[arg(long)]
        reset: bool,
    },
}

#[derive(Subcommand)]
enum ModelAction {
    /// List all models in the catalog (grouped by category)
    List {
        /// Filter by category: gguf | llamafile | whisper | tts | ollama
        #[arg(long)]
        category: Option<String>,
    },
    /// Download a model to disk
    Download { category: String, name: String },
    /// Delete a model file from disk (catalog record kept)
    Delete { category: String, name: String },
    /// Assign a model to a role
    Activate {
        category: String,
        name: String,
        /// Role to assign: chat | think | task | asr | tts
        #[arg(long)]
        role: String,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    /// Send a message through the Goose agent and stream the response
    Chat {
        /// The message to send to the agent
        message: String,
        /// Session ID for conversation continuity across calls
        #[arg(long, default_value = "cli-agent")]
        session: String,
        /// Model role: auto (classify from message), chat, think, task
        #[arg(long, default_value = "auto")]
        role: String,
    },
    /// Interactive multi-turn conversation REPL
    Repl {
        /// Session ID — persists history across the REPL session
        #[arg(long, default_value = "cli-repl")]
        session: String,
        /// Model role: auto (classify each message), chat, think, task
        #[arg(long, default_value = "auto")]
        role: String,
    },
    /// List MCP tool extensions currently known to the agent
    Tools,
    /// List all system prompt extras from the database
    Extras,
}

#[derive(Subcommand)]
enum PromptAction {
    /// List all prompt templates (built-in and user-defined)
    List,
    /// Show the full content of a named template
    Show {
        /// Template name: balanced | concise | technical | warm | <custom>
        name: String,
    },
    /// Re-seed a built-in template to its factory default (overwrites DB record)
    Reset {
        /// Built-in template name: balanced | concise | technical | warm
        name: String,
    },
}

#[derive(Subcommand)]
enum SkillAction {
    /// List skills (active only by default; use --all for inactive too)
    List {
        /// Include inactive skills in the output
        #[arg(long)]
        all: bool,
    },
    /// Add a new skill (reads content from --content or stdin)
    Add {
        /// Unique skill slug (e.g. "morning_brief", "light_control")
        name: String,
        /// Markdown instruction content. Omit to read from stdin.
        #[arg(long)]
        content: Option<String>,
    },
    /// Toggle a skill's active state by UUID
    Toggle {
        /// Skill UUID (from `pond skills list --all`)
        id: String,
    },
    /// Permanently delete a skill by UUID
    Remove {
        /// Skill UUID
        id: String,
    },
}

#[derive(Subcommand)]
enum RecipeAction {
    /// List all recipes
    List,
    /// Show a recipe's YAML content
    Show {
        /// Recipe slug name
        name: String,
    },
    /// Import a recipe from a local YAML file
    Import {
        /// Recipe slug (e.g. "morning_brief")
        name: String,
        /// Path to the Goose recipe YAML file
        file: std::path::PathBuf,
        /// Short one-sentence description
        #[arg(long, default_value = "")]
        description: String,
    },
    /// Delete a recipe by slug name
    Remove {
        /// Recipe slug name
        name: String,
    },
}

#[derive(Subcommand)]
enum MemoryAction {
    /// List recent memory fragments (newest last)
    List {
        /// Maximum number of fragments to show
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Save a new memory fragment
    Add {
        /// The text content of the memory
        content: String,
    },
    /// Delete a memory fragment by UUID
    Remove {
        /// Memory fragment UUID
        id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Setup { model }) => {
            run_setup(&model).await
        }
        Some(Commands::Serve { static_dir, open, debug, agent, native }) => {
            init_tracing(debug);
            run_server(static_dir, open, debug, &agent, native).await
        }
        Some(Commands::Chat { provider, model, input, wake_word, no_wake_word, tts, tts_model, speaker_model }) => {
            init_tracing(false);
            run_chat(provider.as_deref(), model.as_deref(), &input, wake_word.as_deref(), no_wake_word, tts.as_deref(), tts_model, speaker_model).await
        }
        Some(Commands::Enroll { profile, duration, speaker_model }) => {
            init_tracing(false);
            run_enroll(&profile, duration, speaker_model).await
        }
        Some(Commands::Status) => {
            run_status().await
        }
        Some(Commands::Onboard { reset }) => {
            if let Err(err) = run_onboard(reset).await {
                eprintln!("Error: {:?}", err);
            }
            Ok(())
        }
        Some(Commands::Models { action }) => {
            run_models(action).await
        }
        Some(Commands::Agent { action }) => {
            init_tracing(false);
            run_agent_cmd(action).await
        }
        Some(Commands::Prompts { action }) => {
            run_prompts_cmd(action).await
        }
        Some(Commands::Skills { action }) => {
            run_skills_cmd(action).await
        }
        Some(Commands::Recipes { action }) => {
            run_recipes_cmd(action).await
        }
        Some(Commands::Memories { action }) => {
            run_memories_cmd(action).await
        }
        Some(Commands::Calibrate { phrase, samples, whisper_url, reset }) => {
            run_calibrate(phrase.as_deref(), samples, whisper_url.as_deref(), reset).await
        }
        None => {
            // Default: run interactive chat (backward compat) — provider comes from Settings
            init_tracing(false);
            run_chat(None, None, "stdin", None, true, Some("none"), None).await
        }
    }
}

fn init_tracing(debug: bool) {
    // In debug mode, our own crates run at DEBUG while noisy third-party crates
    // (sqlx, hyper, tower, reqwest) are capped at WARN so their internal query
    // and connection tracing does not drown out the useful output.
    //
    // RUST_LOG always takes priority, so a developer can still override any
    // target at runtime:
    //   RUST_LOG=sqlx=debug cargo run -p pond-server -- serve --debug
    let filter = if debug {
        "debug,sqlx=warn,hyper=warn,tower=warn,reqwest=warn,hyper_util=warn,rustls=warn"
    } else {
        "info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| filter.into()),
        )
        .init();
}

async fn run_setup(model: &str) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose In A Pond — Setup         ║");
    println!("  ╚═══════════════════════════════════════╝");

    let data_dir = default_data_dir();

    println!("\n  📂 Data directory: {}", data_dir.display());

    // Step 1: Check + auto-install system dependencies (Linux/macOS only)
    println!("\n  [1/6] Checking system dependencies...");
    if system_deps::ensure_system_deps().await {
        println!("  ✅ System dependencies OK");
    } else {
        println!("  ⚠  Some system deps could not be installed — see docs/developer/linux-setup.md");
        println!("     Continuing setup; some features may not work until deps are installed.");
    }

    // Step 2: Initialize databases + seed model catalog
    println!("\n  [2/6] Initializing databases...");
    let db_setup = Database::init(&data_dir).await?;
    println!("  ✅ Databases ready");

    let setup_model_repo = SqliteModelRepository::new(db_setup.system.clone());
    println!("  📋 Fetching model catalog from upstream sources...");
    seed_model_catalog(&setup_model_repo, &data_dir).await;

    // Seed built-in prompt templates (INSERT OR IGNORE — never overwrites user edits)
    {
        use pond_core::domain::prompt_template::PromptTemplate;
        #[allow(unused_imports)]
        use pond_core::ports::prompt_template::PromptTemplateRepository;
        use pond_core::prompts::{PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM};

        let template_repo = SqlitePromptTemplateRepository::new(db_setup.system.clone());
        let built_ins = [
            ("balanced",  PROMPT_BALANCED,  "Warm, practical, complete behaviour rules. Default for most households."),
            ("concise",   PROMPT_CONCISE,   "Minimal, action-first. For power users who want brevity."),
            ("technical", PROMPT_TECHNICAL, "Verbose, tool-aware, narrates reasoning. For developers."),
            ("warm",      PROMPT_WARM,      "Conversational, family-friendly, personality-forward."),
        ];
        for (name, content, description) in built_ins {
            let t = PromptTemplate {
                name:        name.to_string(),
                content:     content.to_string(),
                description: description.to_string(),
                is_system:   true,
                updated_at:  String::new(),
            };
            if let Err(e) = template_repo.insert_if_absent(&t).await {
                println!("  ⚠  Failed to seed prompt template '{name}': {e}");
            }
        }
        println!("  ✅ Prompt templates seeded");
    }

    // Step 3: Download Whisper ASR model from catalog URL
    let effective_model = if model.is_empty() { "base" } else { model };
    let (expected_path, whisper_dl_url, whisper_dl_mb) = {
        use crate::filesystem_model_storage::FilesystemModelStorage;
        use pond_core::ports::model_storage::ModelStorage as _;
        let storage = FilesystemModelStorage::new(&data_dir);
        let model_id = format!("whisper/{}", effective_model);
        match setup_model_repo.get_by_id(&model_id).await.ok().flatten() {
            Some(r) => {
                let path = storage.path_for(&r)
                    .unwrap_or_else(|| data_dir.join("models").join(format!("ggml-{}.en.bin", effective_model)));
                (path, r.url.unwrap_or_default(), r.size_mb)
            }
            None => {
                let path = data_dir.join("models").join(format!("ggml-{}.en.bin", effective_model));
                (path, String::new(), 0u64)
            }
        }
    };
    println!("\n  [3/6] Downloading Whisper ASR model ({})...", effective_model);
    println!("  📁 Target: {}", expected_path.display());
    if expected_path.exists() {
        println!("  ✅ Already downloaded: {}", expected_path.display());
    } else if !whisper_dl_url.is_empty() {
        if let Some(parent) = expected_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        model_download::download_file(&whisper_dl_url, &expected_path, whisper_dl_mb).await?;
    } else {
        println!("  ⚠  Model '{}' not found in catalog — skipping download", effective_model);
    }

    // Step 4: Download whisper-server binary
    println!("\n  [4/6] Downloading whisper-server binary...");
    let _ = model_download::download_whisper_binary(&data_dir).await;

    // Step 5: Piper TTS binary — voice model is selected via the web Settings page
    println!("\n  [5/6] Setting up Piper TTS...");
    let piper_bin_ok = model_download::download_piper_binary(&data_dir).await.is_ok();
    if !piper_bin_ok {
        println!("  ⚠  Piper binary unavailable — voice output will be text-only.");
        println!("     Install piper manually or retry setup.");
    } else {
        println!("  ✅ Piper binary ready — select a voice model in the web Settings page.");
    }

    // Speaker identification model
    println!("\n  Setting up speaker identification model...");
    let output_path = model_download::speaker_model_path(&data_dir);
    let speaker_ready = if output_path.exists() {
        println!("  ✅ Speaker model already present: {}", output_path.display());
        true
    } else {
        println!("  📦 Installing Python dependencies (speechbrain, onnx, torch)...");
        let pip = tokio::process::Command::new("pip3")
            .args(["install", "--quiet", "speechbrain", "onnx", "torch"])
            .status().await;
        match pip {
            Ok(s) if s.success() => {
                println!("  ✅ Python dependencies ready");
                println!("  🔄 Exporting x-vector ONNX model (this may take a few minutes)...");
                let export = tokio::process::Command::new("python3")
                    .args(["scripts/export_xvector.py", "--output",
                           output_path.to_str().unwrap_or("speaker.onnx")])
                    .status().await;
                match export {
                    Ok(s) if s.success() && output_path.exists() => {
                        println!("  ✅ Speaker model ready: {}", output_path.display());
                        true
                    }
                    Ok(_) => { println!("  ⚠  Export script failed."); false }
                    Err(e) => { println!("  ⚠  Could not run python3: {}", e); false }
                }
            }
            Ok(_) => { println!("  ⚠  pip3 install failed."); false }
            Err(e) => { println!("  ⚠  Could not run pip3: {}", e); false }
        }
    };
    if !speaker_ready {
        println!("     Speaker ID disabled. To enable later:");
        println!("       pip3 install speechbrain onnx torch");
        println!("       python3 scripts/export_xvector.py");
    }

    println!();
    println!("  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  ✅ Setup complete!  Next steps:");
    println!();
    println!("  1. Run the server:");
    println!("       pond-server serve");
    println!();
    println!("  2. Open the web UI and go to Models to download an LLM.");
    println!("     Then go to Settings to configure voice, TTS voice model,");
    println!("     and assign model roles (chat / think / task).");
    println!();
    println!("  Or run interactive CLI chat (configure voice + TTS via Settings first):");
    println!("       pond-server chat --input whisper");
    println!("  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    Ok(())
}

// ── LlamafileManager implementation ──────────────────────────────────────────

/// Manages the llamafile child process lifecycle.
///
/// Implements [`LlamafileManager`] so `pond-api`'s `rebuild_model_router`
/// can start the process when the user switches to the "llamafile" provider
/// without `pond-api` having any process-management knowledge.
struct LlamafileManagerImpl {
    data_dir:      std::path::PathBuf,
    model_service: Arc<pond_core::services::model_service::ModelService>,
    /// Holds the spawned process guard so it stays alive as long as AppState does.
    guard: Arc<tokio::sync::Mutex<Option<llamafile_process::LlamafileProcess>>>,
    /// The port the process actually bound to.
    /// Initialised from the port returned by the startup `try_start` call, or the
    /// base port as a fallback.  Updated by the background spawn task when
    /// `ensure_started` starts a new process on a non-base port.
    actual_port: Arc<std::sync::atomic::AtomicU16>,
}

#[async_trait::async_trait]
impl LlamafileManager for LlamafileManagerImpl {
    async fn ensure_started(&self, model_name: Option<&str>) -> String {
        let effective = self.effective_port();

        // Fast path: already answering requests
        if llamafile_process::is_running(effective).await {
            return llamafile_process::url_for(effective);
        }

        // Spawn in background so the settings-save HTTP response is not delayed
        // by the 5–30 s model-loading time.  The process guard is stored inside
        // `LlamafileManagerImpl` (via the shared `Arc<Mutex<…>>`) so it lives
        // for the lifetime of AppState.
        let data_dir       = self.data_dir.clone();
        let model_service  = self.model_service.clone();
        let model_hint     = model_name.map(|s| s.to_string());
        let guard_arc      = Arc::clone(&self.guard);
        let actual_port_arc = Arc::clone(&self.actual_port);

        tokio::spawn(async move {
            // Double-check under the lock to avoid a race where two concurrent
            // requests both reach the is_running() fast-path as false.
            let mut guard = guard_arc.lock().await;
            let cur = actual_port_arc.load(std::sync::atomic::Ordering::Acquire);
            if llamafile_process::is_running(cur).await {
                return; // someone else already started it
            }
            match llamafile_process::try_start(&data_dir, model_service, model_hint.as_deref()).await {
                Some((proc, port)) => {
                    actual_port_arc.store(port, std::sync::atomic::Ordering::Release);
                    tracing::info!("llamafile started on port {}", port);
                    *guard = Some(proc);
                }
                None => {
                    tracing::warn!("llamafile could not be started (no model found or already running)");
                }
            }
        });

        llamafile_process::url_for(effective)
    }

    async fn is_running(&self) -> bool {
        llamafile_process::is_running(self.effective_port()).await
    }

    async fn ensure_started_and_wait(
        &self,
        model_name: Option<&str>,
        timeout_secs: u64,
    ) -> (String, bool) {
        let port = self.effective_port();
        let url  = llamafile_process::url_for(port);

        // Fast path: already running
        if llamafile_process::is_running(port).await {
            return (url, true);
        }

        // Kick off background startup (reuses existing spawn+lock logic)
        self.ensure_started(model_name).await;

        // Poll until ready or timeout, re-reading actual_port each iteration
        // in case a just-started process chose a different port.
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let cur_port = self.effective_port();
            if llamafile_process::is_running(cur_port).await {
                return (llamafile_process::url_for(cur_port), true);
            }
        }
        (llamafile_process::url_for(self.effective_port()), false)
    }
}

impl LlamafileManagerImpl {
    /// Construct the manager, seeding `actual_port` with the port chosen at
    /// initial startup (or the base port if the process was not started yet).
    fn new(
        data_dir: std::path::PathBuf,
        model_service: Arc<pond_core::services::model_service::ModelService>,
        initial_guard: Option<llamafile_process::LlamafileProcess>,
        initial_port: u16,
    ) -> Self {
        Self {
            data_dir,
            model_service,
            guard: Arc::new(tokio::sync::Mutex::new(initial_guard)),
            actual_port: Arc::new(std::sync::atomic::AtomicU16::new(initial_port)),
        }
    }

    /// Return the port the llamafile process is (or should be) listening on.
    fn effective_port(&self) -> u16 {
        self.actual_port.load(std::sync::atomic::Ordering::Acquire)
    }
}

async fn run_server(static_dir: std::path::PathBuf, open: bool, debug: bool, agent_backend: &str, native: bool) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose In A Pond  v{}         ║", env!("CARGO_PKG_VERSION"));
    println!("  ╚═══════════════════════════════════════╝");

    // Initialize databases
    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;

    // Soft system-dep check (non-fatal — just warn if something looks wrong)
    system_deps::warn_if_missing();

    // ── Load settings early (drives model selection) ─────────────────────────
    let settings_repo_early = SqliteSettingsRepository::new(db.system.clone());
    let settings = settings_repo_early.get().await.unwrap_or_default();

    // ── Component startup: auto-download + wire critical services ────────────
    println!("\n  ── Components ──────────────────────────────────────");

    // STT — whisper.cpp binary + model (only when active_whisper_model is configured)
    // Guard is held for the server lifetime; port is used to build the URL below.
    let (_whisper_guard, whisper_port) = if settings.active_whisper_model.is_empty() {
        println!("  ⏭  STT: whisper skipped (no whisper model configured in Settings)");
        (None, ports::WHISPER)
    } else {
        // Derive filename and download URL from the model catalog DB.
        let (whisper_filename, whisper_url, whisper_mb) = SqliteModelRepository::new(db.system.clone())
            .get_by_id(&format!("whisper/{}", settings.active_whisper_model)).await.ok().flatten()
            .map(|r| (
                r.filename.unwrap_or_else(|| format!("ggml-{}.en.bin", &settings.active_whisper_model)),
                r.url.unwrap_or_default(),
                r.size_mb,
            ))
            .unwrap_or_else(|| (
                format!("ggml-{}.en.bin", &settings.active_whisper_model),
                String::new(),
                0u64,
            ));
        let whisper_model = data_dir.join("models").join(&whisper_filename);
        if !whisper_model.exists() {
            println!("  📥 STT model not found — downloading ({})...", settings.active_whisper_model);
            if !whisper_url.is_empty() {
                if let Some(parent) = whisper_model.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                match model_download::download_file(&whisper_url, &whisper_model, whisper_mb).await {
                    Ok(_) => {}
                    Err(e) => println!("  ⚠  STT model download failed: {}", e),
                }
            } else {
                println!("  ⚠  STT model '{}' not in catalog — cannot download", settings.active_whisper_model);
            }
        }
        if !model_download::whisper_binary_path(&data_dir).exists() {
            println!("  📥 STT binary not found — downloading...");
            match model_download::download_whisper_binary(&data_dir).await {
                Ok(_) => {}
                Err(e) => println!("  ⚠  STT binary download failed: {}", e),
            }
        }
        whisper_process::try_start(&data_dir, &whisper_model).await
    };
    // When the user has set a custom whisper URL (not the default 127.0.0.1:9000),
    // honour it — this lets users point at an external whisper server.
    // Otherwise use the auto-started local process URL.
    const DEFAULT_WHISPER_URL: &str = "http://127.0.0.1:9000";
    let whisper_url = if !settings.voice_whisper_url.is_empty()
        && settings.voice_whisper_url != DEFAULT_WHISPER_URL
    {
        settings.voice_whisper_url.clone()
    } else {
        whisper_process::url_for(whisper_port)
    };

    // Piper voice path — None when no voice is configured (skips all piper startup).
    // When voice_tts_voice is empty the user has not yet picked a piper voice in Settings;
    // do not fall back to a hardcoded default.
    let piper_model: Option<std::path::PathBuf> = if settings.voice_tts_voice.is_empty() {
        None
    } else {
        Some(model_download::tts_models_dir(&data_dir).join(&settings.voice_tts_voice))
    };

    // Only download/install piper components when piper is the configured active TTS
    // AND a specific voice model has been chosen by the user.
    let piper_is_primary = settings.active_tts_model.starts_with("piper");
    if piper_is_primary {
        if let Some(ref piper_model_path) = piper_model {
            if !model_download::piper_binary_path(&data_dir).exists() {
                println!("  📥 Piper binary not found — downloading...");
                let _ = model_download::download_piper_binary(&data_dir).await;
            }
            if !piper_model_path.exists() {
                // Look up the exact voice in the DB to get the correct download URL.
                let voice_filename = &settings.voice_tts_voice;
                let registry_entry = SqliteModelRepository::new(db.system.clone())
                    .list_by_category(&ModelCategory::TtsPiper).await.unwrap_or_default()
                    .into_iter()
                    .find(|m| m.filename.as_deref() == Some(voice_filename.as_str()))
                    .and_then(|m| {
                        let mf = m.filename?;
                        let cf = m.config_filename?;
                        let mu = m.url?;
                        let cu = m.config_url?;
                        Some((mf, cf, mu, cu, m.size_mb))
                    });
                if let Some((mf, cf, mu, cu, sz)) = registry_entry {
                    let _ = model_download::download_piper_model_entry(&data_dir, &mf, &cf, &mu, &cu, sz).await;
                } else {
                    println!("  ⚠  Piper voice '{}' not in model catalog — cannot download", voice_filename);
                }
            }
            model_download::ensure_espeak_ng_data(&data_dir).await;
        } else {
            println!("  ⏭  Piper: active_tts_model=piper but no voice model configured — configure one in Settings");
        }
    }

    // Start piper as a persistent HTTP server so it shows up in the service list.
    // Only starts when both the binary and a configured voice model are present on disk.
    let espeak_data = {
        let p = model_download::piper_espeak_data_path(&data_dir);
        if p.exists() { Some(p) } else { None }
    };
    let mut piper_http_port: Option<u16> = None;
    let piper_tts: Option<Arc<dyn pond_core::ports::voice_output::VoiceOutput>> =
        match (piper_process::find_binary(&data_dir), &piper_model) {
            (Some(bin), Some(model_path)) if model_path.exists() => {
                let ed = espeak_data.clone();
                match piper_http::start(bin.clone(), model_path.clone(), ed).await {
                    Ok(port) => {
                        println!("  ✅ Piper TTS running on port {}", port);
                        piper_http_port = Some(port);
                        let mut out = PiperOutput::new(bin, model_path.clone());
                        if let Some(d) = espeak_data.clone() { out = out.with_espeak_data(d); }
                        Some(Arc::new(out))
                    }
                    Err(e) => {
                        tracing::warn!("piper-http failed to start: {e}");
                        let mut out = PiperOutput::new(bin, model_path.clone());
                        if let Some(d) = espeak_data.clone() { out = out.with_espeak_data(d); }
                        Some(Arc::new(out))
                    }
                }
            }
            _ => None,
        };
    let tts: Option<Arc<dyn pond_core::ports::voice_output::VoiceOutput>> = match piper_tts {
        Some(piper) => {
            println!("  ✅ TTS: piper");
            Some(piper)
        }
        None => {
            println!("  ⚠  TTS: no engine available — responses will be text-only");
            None
        }
    };

    // ── Persistent model catalog & ModelService ────────────────────────────────
    let model_repo: Arc<dyn ModelRepository + Send + Sync> = Arc::new(
        SqliteModelRepository::new(db.system.clone())
    );
    let model_service = Arc::new(pond_core::services::model_service::ModelService::new(
        model_repo.clone(),
        Arc::new(crate::composite_model_catalog_provider::CompositeModelCatalogProvider::new(
            reqwest::Client::builder()
                .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default()
        )),
        Arc::new(crate::http_model_downloader::HttpModelDownloader::new()),
        Arc::new(crate::filesystem_model_storage::FilesystemModelStorage::new(&data_dir)),
    ));

    // Seed catalog from upstream sources (idempotent, safe to call every startup)
    if let Err(e) = model_service.seed_catalog().await {
        tracing::warn!("Failed to seed model catalog: {e}. Starting with existing DB records.");
    }
    // Correct any stale downloaded flags (files added/removed outside of GIAP)
    if let Ok(n) = model_service.sync_disk_flags().await {
        if n > 0 { tracing::info!("sync_disk_flags: corrected {n} stale record(s)"); }
    }
    sync_assignments_to_settings(&*model_repo, &settings_repo_early).await;

    // Autonomous background download: any model assigned to a role but missing from disk.
    // Runs as a detached task so the HTTP server is available immediately.
    {
        use crate::filesystem_model_storage::FilesystemModelStorage;
        use crate::reqwest_model_downloader::ReqwestModelDownloader;
        use crate::startup::auto_download_assigned_models;

        let dl_repo:       Arc<dyn pond_core::ports::model_repository::ModelRepository + Send + Sync> =
            model_repo.clone();
        let dl_storage:    Arc<dyn pond_core::ports::model_storage::ModelStorage + Send + Sync> =
            Arc::new(FilesystemModelStorage::new(&data_dir));
        let dl_downloader: Arc<dyn pond_core::ports::model_downloader::ModelDownloader + Send + Sync> =
            Arc::new(ReqwestModelDownloader);

        tokio::spawn(async move {
            let n = auto_download_assigned_models(dl_repo, dl_storage, dl_downloader).await;
            if n > 0 {
                tracing::info!("auto_download: triggered {n} download(s) for role-assigned models");
            }
        });
    }

    // LLM — only start llamafile when at least one role is configured to use it.
    // ModelService handles downloading autonomously inside try_start.
    let any_role_needs_llamafile = settings.chat_provider == "llamafile"
        || settings.think_provider.as_deref() == Some("llamafile")
        || settings.task_provider.as_deref()  == Some("llamafile");

    let active_llm_name: String = settings.chat_model.clone();
    let (initial_llamafile_guard, llamafile_port) = if any_role_needs_llamafile {
        match llamafile_process::try_start(&data_dir, model_service.clone(), Some(&active_llm_name)).await {
            Some((proc, port)) => (Some(proc), port),
            None => (None, ports::LLAMAFILE),
        }
    } else {
        println!("  ⏭  LLM: llamafile skipped (provider = {})", settings.chat_provider);
        (None, ports::LLAMAFILE)
    };
    let llamafile_url = llamafile_process::url_for(llamafile_port);

    // Build the LlamafileManager — holds the guard so the process stays alive and
    // can start the process on demand when the user switches to the llamafile provider.
    let llamafile_manager: Arc<dyn LlamafileManager> = Arc::new(LlamafileManagerImpl::new(
        data_dir.clone(),
        model_service.clone(),
        initial_llamafile_guard,
        llamafile_port,
    ));

    println!("  ────────────────────────────────────────────────────\n");

    // Build app state
    let onboarding_repo = Arc::new(SqlxOnboardingRepository::new(db.system.clone()));
    let session_storage: Arc<dyn pond_core::ports::session_storage::SessionStorage> =
        Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let settings_repo: Arc<dyn pond_core::ports::settings::SettingsRepository + Send + Sync> =
        Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    let profile_repo: Arc<dyn pond_core::ports::profile::ProfileRepository + Send + Sync> =
        Arc::new(SqliteProfileRepository::new(db.system.clone()));
    let device_registry: Arc<dyn pond_core::ports::device_registry::DeviceRegistry + Send + Sync> =
        Arc::new(SqliteDeviceRegistry::new(db.system.clone()));
    let memory_repo: Arc<dyn pond_core::ports::memory_repository::MemoryRepository + Send + Sync> =
        Arc::new(SqliteMemoryRepository::new(db.system.clone()));
    let sensor_storage: Arc<dyn pond_core::ports::sensor_storage::SensorStorage + Send + Sync> =
        Arc::new(SqliteSensorStorage::new(db.logs.clone()));
    let camera_storage: Arc<dyn pond_core::ports::camera_storage::CameraStorage + Send + Sync> =
        Arc::new(SqliteCameraStorage::new(db.logs.clone()));

    let prompt_template_repo: Arc<dyn pond_core::ports::prompt_template::PromptTemplateRepository + Send + Sync> =
        Arc::new(SqlitePromptTemplateRepository::new(db.system.clone()));
    let prompt_extra_repo: Arc<dyn pond_core::ports::prompt_extra::PromptExtraRepository + Send + Sync> =
        Arc::new(SqlitePromptExtraRepository::new(db.system.clone()));
    let skill_repo: Arc<dyn pond_core::ports::skill::UserSkillRepository + Send + Sync> =
        Arc::new(SqliteSkillRepository::new(db.system.clone()));
    let recipe_repo: Arc<dyn pond_core::ports::recipe::AgentRecipeRepository + Send + Sync> =
        Arc::new(SqliteRecipeRepository::new(db.system.clone()));

    // Reseed built-in prompt templates with latest Jinja2 general-purpose content.
    {
        use pond_core::domain::prompt_template::PromptTemplate;
        #[allow(unused_imports)]
        use pond_core::ports::prompt_template::PromptTemplateRepository;
        use pond_core::prompts::{PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM};
        let built_ins = [
            ("balanced",  PROMPT_BALANCED,  "Warm, practical, general-purpose. Default."),
            ("concise",   PROMPT_CONCISE,   "Minimal, action-first. For power users."),
            ("technical", PROMPT_TECHNICAL, "Verbose, tool-aware, narrates reasoning. For developers."),
            ("warm",      PROMPT_WARM,      "Conversational, family-friendly, personality-forward."),
        ];
        for (name, content, description) in built_ins {
            let t = PromptTemplate {
                name:        name.to_string(),
                content:     content.to_string(),
                description: description.to_string(),
                is_system:   true,
                updated_at:  String::new(),
            };
            if let Err(e) = prompt_template_repo.upsert(&t).await {
                tracing::warn!("Failed to reseed built-in prompt template '{name}': {e}");
            }
        }
        tracing::info!("Built-in prompt templates reseeded (Jinja2 general-purpose copilot)");
    }

    let effective_chat_provider = settings.chat_provider.clone();
    let effective_chat_model    = settings.chat_model.clone();

    // ── Build per-role LLM providers ────────────────────────────────────────
    // Each role (Chat / Think / Task) may use a different provider + model.
    // Token budget and temperature are baked in at startup.
    //
    // Async helper so we can await LocalInferenceLlmAdapter::new() for the
    // "local" (in-process GGUF) provider without blocking the Tokio runtime.
    async fn build_provider(
        provider: &str,
        model: &str,
        llamafile_url: &str,
        data_dir: Option<&std::path::Path>,
        max_tokens: u32,
        temperature: f32,
    ) -> Arc<dyn LlmProvider> {
        match provider {
            "ollama" => Arc::new(
                OllamaProvider::new(None, Some(model))
                    .with_max_tokens(max_tokens)
                    .with_temperature(temperature),
            ) as Arc<dyn LlmProvider>,

            #[cfg(feature = "local-inference")]
            "local" | "gguf" => {
                use pond_adapters_local_inference::LocalInferenceLlmAdapter;
                tracing::info!("Building LocalInferenceLlmAdapter for model: {}", model);
                let result = match data_dir {
                    Some(dir) => LocalInferenceLlmAdapter::new_with_data_dir(model, dir).await,
                    None      => LocalInferenceLlmAdapter::new(model).await,
                };
                match result {
                    Ok(adapter) => {
                        tracing::info!("LocalInferenceLlmAdapter ready for '{}'", model);
                        Arc::new(adapter) as Arc<dyn LlmProvider>
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to build LocalInferenceLlmAdapter for '{}': {}; \
                             falling back to llamafile",
                            model, e
                        );
                        Arc::new(LlamafileProvider::new(Some(llamafile_url))
                            .with_max_tokens(max_tokens)
                            .with_temperature(temperature)) as Arc<dyn LlmProvider>
                    }
                }
            }

            _ => Arc::new(
                // Default: llamafile (covers "llamafile" and unknown provider values)
                LlamafileProvider::new(Some(llamafile_url))
                    .with_max_tokens(max_tokens)
                    .with_temperature(temperature),
            ) as Arc<dyn LlmProvider>,
        }
    }

    let data_dir_ref = Some(data_dir.as_path());
    let max_tokens   = settings.llm_max_tokens;
    let temperature  = settings.llm_temperature;

    let chat_provider_arc = build_provider(
        &effective_chat_provider, &effective_chat_model,
        &llamafile_url, data_dir_ref, max_tokens, temperature,
    ).await;

    // Think role: reuse chat Arc if not separately configured.
    let think_provider_arc: Arc<dyn LlmProvider> =
        if let (Some(tp), Some(tm)) = (&settings.think_provider, &settings.think_model) {
            build_provider(tp, tm, &llamafile_url, data_dir_ref, max_tokens, temperature).await
        } else {
            chat_provider_arc.clone()
        };

    // Task role: reuse chat Arc if not separately configured.
    let task_provider_arc: Arc<dyn LlmProvider> =
        if let (Some(tp), Some(tm)) = (&settings.task_provider, &settings.task_model) {
            build_provider(tp, tm, &llamafile_url, data_dir_ref, max_tokens, temperature).await
        } else {
            chat_provider_arc.clone()
        };

    let llm_provider = Arc::new(tokio::sync::RwLock::new(Some(
        Arc::new(ModelRouter::new(chat_provider_arc, think_provider_arc, task_provider_arc))
            as Arc<dyn LlmProvider>
    )));

    let db = Arc::new(db);

    // Spawn background TTL pruning task (runs every 6 hours)
    {
        let logs = db.logs.clone();
        let system = db.system.clone();
        tokio::spawn(async move {
            pond_infra::pruning::run_pruning(logs, system, Default::default()).await;
        });
    }

    // Debug mode: tail pond_logs.db so new event_log rows are printed to the
    // terminal in real time. Polls every second and only surfaces rows added
    // after startup, so existing history is not replayed.
    if debug {
        let logs_pool = db.logs.clone();
        tokio::spawn(async move {
            tail_event_log(logs_pool).await;
        });
    }

    // Weather — fed into GiapServiceHandles (MCP tool), not AppState.
    // The LLM calls giap__get_current_weather when it needs weather data.
    let weather: Option<Arc<dyn WeatherProvider>> = {
        if settings.weather_enabled
            && (settings.weather_latitude != 0.0 || settings.weather_longitude != 0.0)
        {
            let loc = if settings.weather_location_name.is_empty() {
                format!("{:.3}, {:.3}", settings.weather_latitude, settings.weather_longitude)
            } else {
                settings.weather_location_name.clone()
            };
            tracing::info!(
                "weather enabled: {} ({}, {})",
                loc, settings.weather_latitude, settings.weather_longitude
            );
            Some(Arc::new(OpenMeteoWeatherAdapter::new(
                settings.weather_latitude,
                settings.weather_longitude,
                loc,
            )))
        } else {
            tracing::info!("weather disabled — enable via PUT /api/v1/settings (weather_enabled + lat/lon)");
            None
        }
    };

    // Scheduler — persist task list next to the databases
    let scheduler: Option<Arc<dyn pond_core::ports::scheduler::SchedulerPort>> = {
        let exec = Arc::new(WebhookTaskExecutor::new());
        match CronSchedulerAdapter::new(data_dir.join("schedules.json"), exec).await {
            Ok(s) => {
                tracing::info!("scheduler ready ({})", data_dir.join("schedules.json").display());
                Some(Arc::new(s))
            }
            Err(e) => {
                tracing::warn!("scheduler init failed: {e} — schedule endpoints will return 503");
                None
            }
        }
    };

    // MCP Memory — enabled when --features mcp-memory is passed at build time.
    #[cfg(feature = "mcp-memory")]
    let mcp_memory: Option<Arc<dyn pond_core::ports::mcp_memory::McpMemoryPort + Send + Sync>> = {
        use pond_adapters_mcp_memory::GooseMcpMemoryAdapter;
        let adapter = GooseMcpMemoryAdapter::new(data_dir.join("memory"));
        tracing::info!("MCP memory enabled ({})", data_dir.join("memory").display());
        Some(Arc::new(adapter))
    };
    #[cfg(not(feature = "mcp-memory"))]
    let mcp_memory: Option<Arc<dyn pond_core::ports::mcp_memory::McpMemoryPort + Send + Sync>> = None;

    // ── Agent backend ────────────────────────────────────────────────────────────
    #[cfg(feature = "goose-agent")]
    let (agent, extension_manager) = build_goose_backend(
        agent_backend,
        &llamafile_url,
        &data_dir,
        weather.clone(),
        device_registry.clone(),
        scheduler.clone(),
        settings_repo.clone(),
        memory_repo.clone(),
        skill_repo.clone(),
        recipe_repo.clone(),
        prompt_template_repo.clone(),
        prompt_extra_repo.clone(),
        false, // voice_mode — server mode, not voice
    ).await;

    #[cfg(not(feature = "goose-agent"))]
    let (agent, extension_manager): (Arc<dyn Agent>, Option<Arc<dyn pond_core::ports::extension_manager::ExtensionManagerPort>>) = {
        if agent_backend == "goose" {
            tracing::warn!(
                "Goose agent backend requested but this binary was compiled without the `goose-agent` feature. \
                 Rebuild with: cargo run -p pond-server -- serve  (goose-agent is a default feature). \
                 Falling back to mock agent."
            );
        }
        (Arc::new(MockAgent::new()), None)
    };

    // MCP client — load persisted server configs and auto-connect enabled ones.
    let mcp_server_repo: Option<Arc<dyn pond_core::ports::mcp_server::McpServerRepository>> = {
        let repo = Arc::new(SqliteMcpServerRepository::new(db.system.clone()));
        // Auto-connect saved external MCP servers if the extension manager is available.
        if let Some(mgr) = &extension_manager {
            match repo.list().await {
                Ok(servers) => {
                    for srv in servers.into_iter().filter(|s: &pond_core::ports::mcp_server::McpServerConfig| s.enabled) {
                        use pond_core::ports::extension_manager::AddExtensionRequest;
                        let req = AddExtensionRequest {
                            name:        srv.name.clone(),
                            kind:        srv.kind.clone(),
                            description: srv.description.clone(),
                            command:     srv.command.clone(),
                            args:        srv.args.clone(),
                            env:         srv.env.clone(),
                            uri:         srv.uri.clone(),
                        };
                        match mgr.add_extension(req).await {
                            Ok(_) => tracing::info!("auto-connected MCP server '{}'", srv.name),
                            Err(e) => tracing::warn!("failed to auto-connect MCP server '{}': {e}", srv.name),
                        }
                    }
                }
                Err(e) => tracing::warn!("failed to load saved MCP servers: {e}"),
            }
        }
        Some(repo)
    };

    // Memory-aware model scheduler — only meaningful for in-process GGUF inference.
    // When the local-inference feature is compiled in, create a ResourceAwareModelScheduler
    // and spawn a background pre-loader that warms the model slot on wake-word detection.
    // For llamafile / Ollama those backends manage their own memory; use None there.
    #[cfg(feature = "local-inference")]
    let model_scheduler: Option<Arc<dyn pond_core::ports::model_scheduler::ModelScheduler>> = {
        use pond_adapters_local_inference::ResourceAwareModelScheduler;
        let (sched, mut wake_rx) = ResourceAwareModelScheduler::new();
        let sched_arc = Arc::new(sched);
        tokio::spawn(async move {
            while wake_rx.changed().await.is_ok() {
                if *wake_rx.borrow() {
                    // GooseAdapter's LocalInferenceProvider loads on first complete() call.
                    // Future: trigger a no-op inference call here to warm the model slot
                    // before the user finishes speaking.
                    tracing::info!("model scheduler: wake word detected — model warm-up hint");
                }
            }
        });
        Some(sched_arc as Arc<dyn pond_core::ports::model_scheduler::ModelScheduler>)
    };
    #[cfg(not(feature = "local-inference"))]
    let model_scheduler: Option<Arc<dyn pond_core::ports::model_scheduler::ModelScheduler>> = None;

    // Capture logs pool before `db` is moved into AppState
    let event_log_repo: Option<Arc<dyn pond_core::ports::event_log::EventLogRepository>> =
        Some(Arc::new(SqliteEventLogRepository::new(db.logs.clone())));

    let state = Arc::new(AppState {
        db,
        onboarding_repo,
        handshake: Arc::new(MockHandshake::new()),
        whisper_url: whisper_url.clone(),
        session_storage,
        http_client: reqwest::Client::new(),
        agent,
        llm_provider,
        llamafile_url: llamafile_url.clone(),
        tts,
        settings_repo,
        profile_repo,
        device_registry,
        memory_repo,
        embedding_provider: None,
        sensor_storage,
        camera_storage,
        prompt_template_dir: Some(data_dir.join("prompts")),
        model_repo: Some(model_repo),
        data_dir: Some(data_dir.clone()),
        skip_onboarding: false,
        scheduler,
        model_scheduler,
        mcp_memory,
        extension_manager,
        mcp_server_repo,
        download_tracker: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port,
        model_catalog_provider: Some(Arc::new(
            crate::composite_model_catalog_provider::CompositeModelCatalogProvider::new(
                reqwest::Client::builder()
                    .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .unwrap_or_default()
            )
        )),
        model_storage_dir: Some(data_dir.clone()),
        prompt_template_repo: Some(prompt_template_repo),
        prompt_extra_repo: Some(prompt_extra_repo),
        skill_repo: Some(skill_repo.clone()),
        recipe_repo: Some(recipe_repo.clone()),
        llamafile_manager: Some(llamafile_manager),
        event_log_repo: event_log_repo,
        speaker_id: {
            let model_path = data_dir.join("models").join("speaker.onnx");
            if model_path.exists() {
                match OnnxSpeakerAdapter::new(&model_path, db.system.clone(), db.logs.clone()) {
                    Ok(a) => {
                        println!("  ✅ Speaker ID: x-vector model loaded");
                        Some(Arc::new(a) as Arc<dyn SpeakerIdentification + Send + Sync>)
                    }
                    Err(e) => { tracing::warn!("Speaker ID failed to load: {}", e); None }
                }
            } else { None }
        },
    });

    // Warn if static assets haven't been built yet
    if !static_dir.exists() {
        tracing::warn!(
            "Static dir {:?} not found — web dashboard will not be served. \
             Run `cd web && npm run build` to build it.",
            static_dir
        );
    }

    // Build router
    let app = pond_api::build_router(state, static_dir);

    // Resolve hostname — strip trailing ".local" if the OS already appended it
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "localhost".to_string());
    let hostname = hostname.strip_suffix(".local").unwrap_or(&hostname).to_string();

    let (listener, api_port) = ports::bind_with_fallback("0.0.0.0", ports::API_SERVER).await?;
    let display_url = if api_port == 80 {
        format!("http://pond.{}.local", hostname)
    } else {
        format!("http://pond.{}.local:{}", hostname, api_port)
    };

    println!("  🌐 Listening on 0.0.0.0:{}", api_port);
    println!("  📡 Dashboard: {}", display_url);
    println!("  📡 API:       {}/api/v1/health", display_url);
    println!();

    // Open the browser when:
    //   - `--open` is explicitly passed, OR
    //   - `--debug` is passed and the host has a graphical display.
    // On Linux a display requires DISPLAY (X11) or WAYLAND_DISPLAY to be set.
    // On macOS and Windows a display is always assumed to be present.
    if open || (debug && has_display()) {
        let url = format!("http://localhost:{}", api_port);
        if webbrowser::open(&url).is_err() {
            tracing::warn!("Could not open browser — no display available or xdg-open missing");
        }
    }

    // --native: spawn the Tauri desktop app binary after the server is ready.
    if native {
        spawn_desktop_app(api_port);
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// Locate and spawn the pond-desktop Tauri binary.
///
/// Search order (relative to the workspace root, i.e. where the server binary
/// is invoked from):
///   1. `pond-desktop/src-tauri/target/debug/pond-desktop`          — `cargo tauri dev`
///   2. `pond-desktop/src-tauri/target/release/pond-desktop`        — release build
///   3. `pond-desktop/src-tauri/target/release/bundle/macos/Goose In A Pond.app/Contents/MacOS/Goose In A Pond`
///
/// The child process is detached (not joined) so the server keeps running.
fn spawn_desktop_app(server_port: u16) {
    let candidates: &[&str] = &[
        "pond-desktop/src-tauri/target/debug/pond-desktop",
        "pond-desktop/src-tauri/target/release/pond-desktop",
        "pond-desktop/src-tauri/target/release/bundle/macos/Goose In A Pond.app/Contents/MacOS/Goose In A Pond",
    ];

    let found = candidates.iter().find(|p| std::path::Path::new(p).exists());

    match found {
        Some(path) => {
            tracing::info!("Launching native desktop app: {}", path);
            match std::process::Command::new(path)
                .env("GIAP_SERVER_PORT", server_port.to_string())
                .spawn()
            {
                Ok(child) => {
                    tracing::info!("Desktop app started (pid {})", child.id());
                    // Drop child handle — process runs independently.
                }
                Err(e) => {
                    tracing::warn!("Failed to launch desktop app at {}: {}", path, e);
                }
            }
        }
        None => {
            tracing::warn!(
                "--native: desktop binary not found. Build it first:\n  cd pond-desktop && npm run tauri build\nor for dev:\n  cd pond-desktop && npm run tauri dev"
            );
        }
    }
}

async fn run_chat(provider: Option<&str>, model: Option<&str>, input: &str, wake_word: Option<&str>, no_wake_word: bool, tts: Option<&str>, tts_model: Option<std::path::PathBuf>, speaker_model: Option<std::path::PathBuf>) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose-in-a-Pond  v{}       ║", env!("CARGO_PKG_VERSION"));
    println!("  ║   Wait → Listen → Think → Speak      ║");
    println!("  ╚═══════════════════════════════════════╝");

    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;

    // Load settings and model registry early — drives provider, model, TTS, and wake word.
    // Falls back to Settings::default() when the DB has no rows yet (first run).
    let settings_repo_chat = SqliteSettingsRepository::new(db.system.clone());
    let settings = settings_repo_chat.get().await.unwrap_or_default();

    // CLI args override settings; settings provide the defaults from the chat role.
    let settings_provider = settings.chat_provider.clone();
    let effective_provider: &str = provider.unwrap_or(&settings_provider);

    let settings_model = settings.chat_model.clone();
    let effective_model: &str = model.unwrap_or(&settings_model);
    // Resolve TTS engine from CLI flag or settings. Normalise piper-* variants to "piper".
    let effective_tts_owned: String;
    let effective_tts: &str = match tts {
        Some(t) => t,
        None => {
            effective_tts_owned = if settings.active_tts_model.starts_with("piper") {
                "piper".to_string()
            } else {
                settings.active_tts_model.clone()
            };
            &effective_tts_owned
        }
    };
    println!("  Provider: {} (model: {})", effective_provider, effective_model);

    // Auto-start whisper.cpp when voice input is requested.
    let mut whisper_port = ports::WHISPER;
    let _whisper_guard = if input == "whisper" {
        let whisper_model_name = settings.active_whisper_model.as_str();
        // Look up filename and URL from the catalog DB.
        let (whisper_filename, whisper_url, whisper_mb) = SqliteModelRepository::new(db.system.clone())
            .get_by_id(&format!("whisper/{}", whisper_model_name)).await.ok().flatten()
            .map(|r| (
                r.filename.unwrap_or_else(|| format!("ggml-{}.en.bin", whisper_model_name)),
                r.url.unwrap_or_default(),
                r.size_mb,
            ))
            .unwrap_or_else(|| (
                format!("ggml-{}.en.bin", whisper_model_name),
                String::new(),
                0u64,
            ));
        let whisper_model = data_dir.join("models").join(&whisper_filename);
        if !whisper_model.exists() {
            println!("  📥 STT model not found — downloading ({})...", whisper_model_name);
            if !whisper_url.is_empty() {
                if let Some(parent) = whisper_model.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                match model_download::download_file(&whisper_url, &whisper_model, whisper_mb).await {
                    Ok(_)  => {}
                    Err(e) => println!("  ⚠  STT model download failed: {}", e),
                }
            } else {
                println!("  ⚠  STT model '{}' not in catalog — cannot download", whisper_model_name);
            }
        }
        let (guard, port) = whisper_process::try_start(&data_dir, &whisper_model).await;
        whisper_port = port;
        guard
    } else {
        None
    };
    let whisper_url = whisper_process::url_for(whisper_port);

    // ── Model catalog & ModelService (for autonomous downloading) ──────────────
    let chat_model_repo: Arc<dyn ModelRepository + Send + Sync> = Arc::new(
        SqliteModelRepository::new(db.system.clone())
    );
    let chat_model_service = Arc::new(pond_core::services::model_service::ModelService::new(
        chat_model_repo.clone(),
        Arc::new(crate::composite_model_catalog_provider::CompositeModelCatalogProvider::new(
            reqwest::Client::builder()
                .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default()
        )),
        Arc::new(crate::http_model_downloader::HttpModelDownloader::new()),
        Arc::new(crate::filesystem_model_storage::FilesystemModelStorage::new(&data_dir)),
    ));

    // Seed catalog so model records exist for resolution
    if let Err(e) = chat_model_service.seed_catalog().await {
        tracing::warn!("Failed to seed model catalog: {e}");
    }
    if let Ok(n) = chat_model_service.sync_disk_flags().await {
        if n > 0 { tracing::info!("sync_disk_flags: corrected {n} stale record(s)"); }
    }

    // Auto-start llamafile only when the provider is explicitly "llamafile".
    // Other providers (ollama, local, gguf, openai, etc.) manage their own process or need no process.
    let mut llamafile_port = ports::LLAMAFILE;
    let _llamafile_guard = if effective_provider == "llamafile" {
        match llamafile_process::try_start(&data_dir, chat_model_service, Some(effective_model)).await {
            Some((proc, port)) => { llamafile_port = port; Some(proc) }
            None => None,
        }
    } else {
        None
    };
    let llamafile_url = llamafile_process::url_for(llamafile_port);

    let session_id = "default-session".to_string();

    // ── Build repos for GooseAdapter (before db.system is consumed) ───────────────
    let settings_repo_arc: Arc<dyn pond_core::ports::settings::SettingsRepository + Send + Sync> =
        Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    let memory_repo: Arc<dyn pond_core::ports::memory_repository::MemoryRepository + Send + Sync> =
        Arc::new(SqliteMemoryRepository::new(db.system.clone()));
    let skill_repo: Arc<dyn pond_core::ports::skill::UserSkillRepository + Send + Sync> =
        Arc::new(SqliteSkillRepository::new(db.system.clone()));
    let recipe_repo: Arc<dyn pond_core::ports::recipe::AgentRecipeRepository + Send + Sync> =
        Arc::new(SqliteRecipeRepository::new(db.system.clone()));
    let template_repo: Arc<dyn pond_core::ports::prompt_template::PromptTemplateRepository + Send + Sync> =
        Arc::new(SqlitePromptTemplateRepository::new(db.system.clone()));
    let extras_repo: Arc<dyn pond_core::ports::prompt_extra::PromptExtraRepository + Send + Sync> =
        Arc::new(SqlitePromptExtraRepository::new(db.system.clone()));
    let device_registry_arc: Arc<dyn pond_core::ports::device_registry::DeviceRegistry + Send + Sync> =
        Arc::new(SqliteDeviceRegistry::new(db.system.clone()));

    // Reseed built-in prompt templates at startup with the latest Jinja2 general-purpose content.
    // Uses upsert (not insert_if_absent) so existing installs get the updated templates.
    // User-created templates (is_system = false) are never touched.
    {
        use pond_core::domain::prompt_template::PromptTemplate;
        #[allow(unused_imports)]
        use pond_core::ports::prompt_template::PromptTemplateRepository;
        use pond_core::prompts::{PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM};
        let built_ins = [
            ("balanced",  PROMPT_BALANCED,  "Warm, practical, general-purpose. Default."),
            ("concise",   PROMPT_CONCISE,   "Minimal, action-first. For power users."),
            ("technical", PROMPT_TECHNICAL, "Verbose, tool-aware, narrates reasoning. For developers."),
            ("warm",      PROMPT_WARM,      "Conversational, family-friendly, personality-forward."),
        ];
        for (name, content, description) in built_ins {
            let t = PromptTemplate {
                name:        name.to_string(),
                content:     content.to_string(),
                description: description.to_string(),
                is_system:   true,
                updated_at:  String::new(),
            };
            if let Err(e) = template_repo.upsert(&t).await {
                tracing::warn!("Failed to reseed built-in prompt template '{name}': {e}");
            }
        }
        tracing::info!("Built-in prompt templates reseeded (Jinja2 general-purpose copilot)");
    }

    // Wire weather so giap__get_current_weather MCP tool is available in voice mode.
    let weather: Option<Arc<dyn WeatherProvider>> = if settings.weather_enabled
        && (settings.weather_latitude != 0.0 || settings.weather_longitude != 0.0)
    {
        let loc = if settings.weather_location_name.is_empty() {
            format!("{:.3}, {:.3}", settings.weather_latitude, settings.weather_longitude)
        } else {
            settings.weather_location_name.clone()
        };
        Some(Arc::new(OpenMeteoWeatherAdapter::new(
            settings.weather_latitude,
            settings.weather_longitude,
            loc,
        )))
    } else {
        None
    };

    // ── Build the GooseAdapter (MCP tools + model routing) ───────────────────────
    // When goose-agent is compiled in, GooseAdapter is used for all inference —
    // it selects provider/model internally via the settings DB.  CLI --provider
    // and --model flags are persisted to the DB first so GooseAdapter picks them up.
    //
    // Without the feature we fall back to MockAgent and wire a direct LlmProvider
    // via with_provider() later (identical to the old behaviour).
    #[cfg(feature = "goose-agent")]
    let agent: Arc<dyn Agent> = {
        // Persist CLI overrides so GooseAdapter reads the right provider + model.
        if provider.is_some() || model.is_some() {
            let mut s = settings.clone();
            s.chat_provider = effective_provider.to_string();
            s.chat_model    = effective_model.to_string();
            settings_repo_arc.update(&s).await.ok();
        }
        let (a, _ext_mgr) = build_goose_backend(
            "goose",
            &llamafile_url,
            &data_dir,
            weather,
            device_registry_arc,
            None, // scheduler not used in voice mode
            settings_repo_arc,
            memory_repo,
            skill_repo,
            recipe_repo,
            template_repo,
            extras_repo,
            input == "whisper", // voice_mode
        ).await;
        a
    };

    #[cfg(not(feature = "goose-agent"))]
    let agent: Arc<dyn Agent> = Arc::new(MockAgent::new());

    // Resolve the system prompt using the already-loaded settings:
    // 1. File at $DATA_DIR/prompts/system.md (deployment override, rendered with all vars)
    // 2. build_system_prompt(&settings) — honours custom_system_prompt + prompt_style + addendum
    let system_prompt = {
        let prompt_dir    = data_dir.join("prompts");
        let file_template = std::fs::read_to_string(prompt_dir.join("system.md")).ok();
        match file_template {
            Some(tmpl) => {
                println!("  Prompt:   custom ({})", prompt_dir.join("system.md").display());
                let name     = pond_core::prompts::sanitize_field(&settings.assistant_name, 50);
                let user     = pond_core::prompts::sanitize_field(&settings.user_name, 50);
                let persona  = pond_core::prompts::sanitize_field(&settings.assistant_personality, 200);
                let tz       = pond_core::prompts::sanitize_field(&settings.timezone, 50);
                let location = if settings.weather_location_name.is_empty() {
                    String::new()
                } else {
                    format!("\nLocation: {}.", pond_core::prompts::sanitize_field(&settings.weather_location_name, 100))
                };
                let addendum = pond_core::prompts::sanitize_field(&settings.prompt_addendum, 500);
                pond_core::prompts::render_template(&tmpl, &[
                    ("assistant_name",  name.as_str()),
                    ("user_name",       user.as_str()),
                    ("personality",     persona.as_str()),
                    ("timezone",        tz.as_str()),
                    ("location",        location.as_str()),
                    ("prompt_addendum", addendum.as_str()),
                ])
            }
            None => {
                println!(
                    "  Assistant: {} / style: {} / user: {}",
                    settings.assistant_name, settings.prompt_style, settings.user_name
                );
                build_system_prompt(&settings)
            }
        }
    };

    let db_system = db.system.clone();
    let db_logs = db.logs.clone();
    let storage: Arc<dyn SessionStorage> = Arc::new(SqliteSessionStorage::new(db.system));
    // Create session if it doesn't exist; ignore duplicate-key errors from prior runs
    if let Err(e) = storage.create_session(session_id.clone()).await {
        match e {
            pond_core::ports::session_storage::SessionStorageError::StorageError(_) => {
                // Likely a duplicate key — session already exists, which is fine
                tracing::debug!("Session already exists, reusing: {}", session_id);
            }
            other => return Err(other.into()),
        }
    }

    let mut chat_service = ChatService::new(agent, session_id.clone(), storage)
        .with_system_prompt(system_prompt);

    // ── Wire LLM provider (no-goose-agent fallback only) ────────────────────────
    // When GooseAdapter is active it selects the provider internally via the DB.
    // This block runs only in builds without the goose-agent feature.
    #[cfg(not(feature = "goose-agent"))]
    match effective_provider {
        "ollama" => {
            println!(
                "  Model:    {} (ollama @ {}, max_tokens={}, temp={})",
                effective_model,
                pond_adapters_ollama::DEFAULT_HOST,
                settings.llm_max_tokens,
                settings.llm_temperature,
            );
            let llm = Arc::new(
                OllamaProvider::new(None, Some(effective_model))
                    .with_max_tokens(settings.llm_max_tokens)
                    .with_temperature(settings.llm_temperature),
            );
            chat_service = chat_service.with_provider(llm);
        }
        "local" | "gguf" => {
            #[cfg(feature = "local-inference")]
            {
                use pond_adapters_local_inference::LocalInferenceLlmAdapter;
                let hf_model_id = chat_model_repo
                    .get_by_id(&format!("gguf/{}", effective_model))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|r| r.hf_id)
                    .unwrap_or_else(|| effective_model.to_string());
                println!("  Model:    {} (local GGUF in-process)", hf_model_id);
                let llm = Arc::new(
                    LocalInferenceLlmAdapter::new_with_data_dir(&hf_model_id, &data_dir).await?
                );
                chat_service = chat_service.with_provider(llm);
            }
            #[cfg(not(feature = "local-inference"))]
            {
                eprintln!(
                    "  WARN: --provider local requires the `local-inference` feature (not compiled in).\n\
                     Falling back to llamafile. Rebuild with:\n  \
                     cargo run -p pond-server --features local-inference -- chat --provider local"
                );
                println!(
                    "  Model:    {} (llamafile @ {}, max_tokens={}, temp={})",
                    effective_model,
                    llamafile_url,
                    settings.llm_max_tokens,
                    settings.llm_temperature,
                );
                let llm = Arc::new(
                    LlamafileProvider::new(Some(&llamafile_url))
                        .with_max_tokens(settings.llm_max_tokens)
                        .with_temperature(settings.llm_temperature),
                );
                chat_service = chat_service.with_provider(llm);
            }
        }
        _ => {
            // "llamafile" and any unrecognised value — use the llamafile process started above.
            println!(
                "  Model:    {} (llamafile @ {}, max_tokens={}, temp={})",
                effective_model,
                llamafile_url,
                settings.llm_max_tokens,
                settings.llm_temperature,
            );
            let llm = Arc::new(
                LlamafileProvider::new(Some(&llamafile_url))
                    .with_max_tokens(settings.llm_max_tokens)
                    .with_temperature(settings.llm_temperature),
            );
            chat_service = chat_service.with_provider(llm);
        }
    }

    // ── Wire voice input ──
    let voice: Arc<dyn VoiceInput> = match input {
        "whisper" => {
            println!("  Input:    whisper (@ {})", whisper_url);
            Arc::new(WhisperInput::new(Some(&whisper_url)))
        }
        _ => {
            println!("  Input:    stdin");
            Arc::new(StdinInput::new())
        }
    };
    chat_service = chat_service.with_voice_input(voice);

    // ── Wire wake word detector ──
    if no_wake_word || input != "whisper" {
        chat_service = chat_service.with_wake_word_detector(Arc::new(InstantActivation));
    } else {
        let trigger        = wake_word.unwrap_or(settings.voice_wake_word.as_str());
        let transcriptions = settings.voice_wake_word_transcriptions.clone();
        // Tiered model: use a separate (fast, tiny) whisper server for KWS when configured.
        let kws_url = settings.voice_kws_whisper_url
            .as_deref()
            .unwrap_or(&whisper_url);

        if transcriptions.is_empty() {
            println!("  Wake word: \"{}\" (no calibration — using raw phrase)", trigger);
        } else {
            println!("  Wake word: \"{}\" ({} calibrated variants)", trigger, transcriptions.len());
        }
        println!("  KWS whisper:   {}", kws_url);
        println!("  ASR whisper:   {}", whisper_url);
        println!("  Energy gate:   {:.3} RMS  |  cooldown: {}ms  |  VAD silence: {}ms",
            settings.voice_kws_energy_threshold,
            settings.voice_kws_cooldown_ms,
            settings.voice_kws_post_trigger_silence_ms);

        use pond_adapters_whisper::KeywordDetectorConfig;
        let kws_config = KeywordDetectorConfig {
            energy_threshold:        settings.voice_kws_energy_threshold,
            post_trigger_silence_ms: settings.voice_kws_post_trigger_silence_ms,
            cooldown_ms:             settings.voice_kws_cooldown_ms,
            ..KeywordDetectorConfig::default()
        };

        let detector = Arc::new(
            WhisperKeywordDetector::new(Some(kws_url), trigger)
                .with_transcriptions(transcriptions)
                .with_config(kws_config)
        );
        chat_service = chat_service.with_wake_word_detector(detector);
    };

    // ── Wire TTS output ──
    let voice_out: Arc<dyn VoiceOutput> = match effective_tts {
        "piper" => {
            // Resolve model path: CLI arg → settings → warn and fall back to text
            let model_path_opt: Option<std::path::PathBuf> = if let Some(p) = tts_model {
                Some(p)
            } else if !settings.voice_tts_voice.is_empty() {
                Some(model_download::tts_models_dir(&data_dir).join(&settings.voice_tts_voice))
            } else {
                println!("  ⚠  TTS: piper requested but no voice model configured in Settings.");
                println!("     Set a piper voice in the web UI, then restart. Using text output.");
                None
            };
            match model_path_opt {
                None => Arc::new(PrintOutput),
                Some(model_path) => {
                    if piper_process::find_binary(&data_dir).is_none() {
                        println!("  📥 TTS binary not found — downloading...");
                        match model_download::download_piper_binary(&data_dir).await {
                            Ok(_)  => {}
                            Err(e) => println!("  ⚠  TTS binary download failed: {}", e),
                        }
                    }
                    // Piper requires both the .onnx weights AND the .onnx.json config.
                    // Check both — the JSON is often missing even when the onnx was
                    // downloaded in an earlier version that didn't fetch the config.
                    let config_path = std::path::PathBuf::from(
                        format!("{}.json", model_path.display())
                    );
                    if !model_path.exists() || !config_path.exists() {
                        if model_path.exists() {
                            println!("  📥 TTS model config (.json) missing — downloading...");
                        } else {
                            println!("  📥 TTS model not found — downloading configured voice...");
                        }
                        // Look up in DB by filename to get the correct download URL.
                        let voice_filename = settings.voice_tts_voice.as_str();
                        let registry_entry = SqliteModelRepository::new(db_system.clone())
                            .list_by_category(&ModelCategory::TtsPiper).await.unwrap_or_default()
                            .into_iter()
                            .find(|m| m.filename.as_deref() == Some(voice_filename))
                            .and_then(|m| {
                                let mf = m.filename?;
                                let cf = m.config_filename?;
                                let mu = m.url?;
                                let cu = m.config_url?;
                                Some((mf, cf, mu, cu, m.size_mb))
                            });
                        if let Some((mf, cf, mu, cu, sz)) = registry_entry {
                            let _ = model_download::download_piper_model_entry(&data_dir, &mf, &cf, &mu, &cu, sz).await;
                        } else {
                            println!("  ⚠  Piper voice '{}' not in model catalog — cannot download", voice_filename);
                        }
                    }
                    match piper_process::find_binary(&data_dir) {
                        Some(bin) => {
                            println!("  TTS:      piper ({})", model_path.file_name().unwrap_or_default().to_string_lossy());
                            Arc::new(PiperOutput::new(bin, model_path))
                        }
                        None => {
                            println!("  TTS:      piper unavailable (binary not found) — falling back to print");
                            Arc::new(PrintOutput)
                        }
                    }
                }
            }
        }
        _ => {
            println!("  TTS:      print");
            Arc::new(PrintOutput)
        }
    };
    chat_service = chat_service.with_voice_output(voice_out);

    // ── Wire speaker identification (optional) ──
    let speaker_model_path = speaker_model.unwrap_or_else(|| {
        data_dir.join("models").join("speaker.onnx")
    });
    if speaker_model_path.exists() {
        let profile_repo: Arc<dyn pond_core::ports::profile::ProfileRepository + Send + Sync> =
            Arc::new(pond_infra::sqlite_profile::SqliteProfileRepository::new(db_system.clone()));
        match OnnxSpeakerAdapter::new(&speaker_model_path, db_system.clone(), db_logs.clone()) {
            Ok(adapter) => {
                println!("  Speaker ID: enabled");
                let adapter: Arc<dyn SpeakerIdentification> = Arc::new(adapter);
                chat_service = chat_service
                    .with_speaker_id(adapter)
                    .with_profile_repo(profile_repo);
            }
            Err(e) => println!("  ⚠  Speaker ID: failed to load model — {}", e),
        }
    } else {
        println!("  Speaker ID: disabled (no model at {})", speaker_model_path.display());
    }

    chat_service.run_loop().await?;

    Ok(())
}

/// Returns `true` when the process has access to a graphical display.
///
/// On Linux, a display is present when `DISPLAY` (X11) or `WAYLAND_DISPLAY`
/// is set in the environment. On all other platforms (macOS, Windows) a
/// display is unconditionally assumed.
fn has_display() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Background task that tails the `event_log` table in `pond_logs.db`.
///
/// On startup, it records the current maximum row ID so that pre-existing log
/// history is not replayed. It then polls every second and prints any new rows
/// to stdout. This is intentionally a plain `println!` rather than a tracing
/// event so the output is always visible alongside the tracing output, making
/// it easy to correlate API activity with DB-level events in a single terminal.
///
/// Output format:
/// ```text
///   [db] 2024-01-15 12:34:56  INFO [pond-api] request handled
///   [db] 2024-01-15 12:34:57 ERROR [pond-core] something failed — {"key":"val"}
/// ```
async fn tail_event_log(pool: sqlx::Pool<sqlx::Sqlite>) {
    // Anchor to the highest existing ID so we only surface new events.
    let mut cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM event_log")
        .fetch_one(&pool)
        .await
        .unwrap_or(0);

    println!("  [debug] tailing pond_logs.db event_log (cursor = {})...", cursor);

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let rows: Vec<(i64, String, String, String, String, Option<String>)> =
            sqlx::query_as(
                "SELECT id, timestamp, level, source, message, metadata \
                 FROM event_log WHERE id > ? ORDER BY id ASC",
            )
            .bind(cursor)
            .fetch_all(&pool)
            .await
            .unwrap_or_default();

        for (id, timestamp, level, source, message, metadata) in rows {
            match metadata.as_deref().filter(|m| !m.is_empty()) {
                Some(meta) => println!(
                    "  [db] {} {:>5} [{}] {} — {}",
                    timestamp, level, source, message, meta
                ),
                None => println!(
                    "  [db] {} {:>5} [{}] {}",
                    timestamp, level, source, message
                ),
            }
            cursor = id;
        }
    }
}

async fn run_status() -> Result<()> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let hostname = hostname.strip_suffix(".local").unwrap_or(&hostname).to_string();

    println!("  🦆 Goose In A Pond — Status");
    println!("  ─────────────────────────────");
    println!("  Version:   {}", env!("CARGO_PKG_VERSION"));
    println!("  Hostname:  {}", hostname);
    println!("  Platform:  {} / {}", std::env::consts::OS, std::env::consts::ARCH);

    let data_dir = default_data_dir();
    let db_path = data_dir.join("pond_system.db");
    println!("  Database:  {}", db_path.display());
    if db_path.exists() {
        if let Ok(db) = Database::init(&data_dir).await {
            let settings_repo = SqliteSettingsRepository::new(db.system.clone());
            if let Ok(s) = settings_repo.get().await {
                println!("  Assistant: {} | Provider: {} | Model: {}",
                    s.assistant_name, s.chat_provider, s.active_llm_model);
                println!("  Wake word: {}", s.voice_wake_word);
            }
            let onboard_repo = SqlxOnboardingRepository::new(db.system.clone());
            let svc = OnboardingService::new(onboard_repo);
            println!("  Onboarded: {:?}", svc.status().await);
        }
    } else {
        println!("  (not found — run `pond-server setup` first)");
    }

    Ok(())
}

/// `pond-server calibrate` — record N samples of the wake-word phrase and store
/// Whisper's transcriptions as calibration variants in settings.
async fn run_calibrate(
    phrase_arg: Option<&str>,
    target_samples: usize,
    whisper_url_arg: Option<&str>,
    reset: bool,
) -> Result<()> {
    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;
    let settings_repo = SqliteSettingsRepository::new(db.system.clone());
    let mut settings = settings_repo.get().await?;

    // Resolve phrase and whisper URL from args → settings → defaults.
    let phrase = phrase_arg
        .unwrap_or(settings.voice_wake_word.as_str())
        .to_string();
    let whisper_url = whisper_url_arg
        .unwrap_or(settings.voice_whisper_url.as_str())
        .to_string();

    println!();
    println!("  ╔═══════════════════════════════════════════════╗");
    println!("  ║   🎤  Wake-Word Calibration                   ║");
    println!("  ╚═══════════════════════════════════════════════╝");
    println!("  Phrase:       \"{}\"", phrase);
    println!("  Whisper URL:  {}", whisper_url);
    println!("  Samples:      {}", target_samples);
    println!();

    if reset {
        settings.voice_wake_word_transcriptions.clear();
        settings_repo.update(&settings).await?;
        println!("  ✓  Previous calibration data cleared.");
        println!();
    } else if !settings.voice_wake_word_transcriptions.is_empty() {
        println!("  Existing variants ({}):", settings.voice_wake_word_transcriptions.len());
        for v in &settings.voice_wake_word_transcriptions {
            println!("    • {}", v);
        }
        println!("  (add --reset to discard these and start fresh)");
        println!();
    }

    // Save the phrase to settings in case it was provided via --phrase.
    if phrase_arg.is_some() {
        settings.voice_wake_word = phrase.clone();
    }

    let whisper = WhisperInput::new(Some(&whisper_url));
    let mut collected = 0usize;
    let mut attempt  = 0usize;

    while collected < target_samples {
        attempt += 1;
        println!("  ── Sample {} / {} ─────────────────────────────────", collected + 1, target_samples);
        println!("  Press Enter, then say \"{}\"...", phrase);
        {
            // Wait for Enter
            let mut buf = String::new();
            io::stdin().read_line(&mut buf)?;
        }

        print!("  🎤 Recording...");
        io::stdout().flush()?;

        let text = match whisper.listen().await {
            Ok(Some(t)) => t,
            Ok(None) => {
                println!(" (no speech detected — try again)");
                continue;
            }
            Err(e) => {
                println!(" (error: {} — is whisper running at {}?)", e, whisper_url);
                if attempt >= target_samples * 3 {
                    anyhow::bail!("Too many failed attempts — aborting calibration.");
                }
                continue;
            }
        };

        // Normalize: strip punctuation, collapse whitespace, lowercase.
        let normalized: String = text
            .chars()
            .map(|c| if c.is_alphabetic() { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();

        println!(" heard: \"{}\"", text);

        if normalized.is_empty() {
            println!("  (normalized to empty — skipping)");
            continue;
        }

        if settings.voice_wake_word_transcriptions.contains(&normalized) {
            println!("  (already stored as a variant — skipping duplicate)");
            // Still count toward progress so the loop terminates.
            collected += 1;
            continue;
        }

        settings.voice_wake_word_transcriptions.push(normalized.clone());
        settings_repo.update(&settings).await?;
        collected += 1;

        println!("  ✓  Stored: \"{}\"  ({}/{})", normalized, collected, target_samples);
        println!();
    }

    println!("  ╔═══════════════════════════════════════════════╗");
    println!("  ║   ✅  Calibration Complete!                   ║");
    println!("  ╚═══════════════════════════════════════════════╝");
    println!("  Phrase:    \"{}\"", settings.voice_wake_word);
    println!("  Variants ({}):", settings.voice_wake_word_transcriptions.len());
    for v in &settings.voice_wake_word_transcriptions {
        println!("    • {}", v);
    }
    println!();
    println!("  The wake-word detector will now match any of these variants.");
    println!("  Run `pond-server chat --input whisper` to test it.");
    println!();

    Ok(())
}

fn default_data_dir() -> std::path::PathBuf {
    // `POND_DATA_DIR` lets tests (and power users) redirect all DB and model
    // storage to an arbitrary directory without touching the real data store.
    if let Ok(dir) = std::env::var("POND_DATA_DIR") {
        return std::path::PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("goose-in-a-pond")
}

fn get_local_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.168.1.1:80").ok()?;
    let local_addr = socket.local_addr().ok()?;
    Some(local_addr.ip().to_string())
}

fn prompt_nonempty(prompt: &str) -> Result<String> {

    loop {
        print!("{}", prompt);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        let trimmed = input.trim();

        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }

        println!("Input cannot be empty. Try again.");
    }
}
async fn run_main_menu() -> Result<()> {
    loop {
        println!("\n🦆 Goose In A Pond — Main Menu");
        println!("─────────────────────────────");
        println!("  1) Chat        — Interactive AI chat");
        println!("  2) Serve       — Start the HTTP server + API");
        println!("  3) Status      — Show system info");
        println!("  4) Enroll      — Set up voice recognition for a profile");
        println!("  5) Exit");
        println!();

        let choice = prompt_nonempty("Choose an option: ")?;

        match choice.trim() {
            "1" => {
                run_chat(None, None, "stdin", None, true, Some("none"), None, None).await?;
            }
            "2" => {
                run_server(std::path::PathBuf::from("web/dist"), false, false, "goose", false).await?;
            }
            "3" => {
                run_status().await?;
            }
            "4" => {
                run_enroll_menu(&default_data_dir()).await?;
            }
            "5" => {
                println!("Goodbye!");
                break;
            }
            _ => {
                println!("Invalid option. Try again.");
            }
        }
    }

    Ok(())
}
async fn run_enroll_menu(data_dir: &std::path::Path) -> Result<()> {
    let db = Database::init(data_dir).await?;
    let model_path = data_dir.join("models").join("speaker.onnx");
    if !model_path.exists() {
        println!("\n  ⚠  Speaker model not found. Run `pond-server setup` first.");
        return Ok(());
    }
    let adapter = match OnnxSpeakerAdapter::new(&model_path, db.system.clone(), db.logs.clone()) {
        Ok(a) => a,
        Err(e) => { println!("\n  ⚠  Failed to load speaker model: {}", e); return Ok(()); }
    };
    let profile_repo = SqliteProfileRepository::new(db.system.clone());
    let mut profiles = pond_core::ports::profile::ProfileRepository::list(&profile_repo).await?;
    if profiles.is_empty() {
        println!("\n  No profiles found. Let's create one.");
        let name = prompt_nonempty("  Enter your name: ")?;
        let created = pond_core::ports::profile::ProfileRepository::create(
            &profile_repo,
            pond_core::domain::profile::CreateProfileRequest {
                display_name: name.trim().to_string(),
                avatar_emoji: "🦆".to_string(),
            },
        ).await?;
        println!("  ✅ Profile created for {}", created.display_name);
        profiles = vec![created];
    }
    println!("\n  👤 Voice Enrollment");
    println!("  ───────────────────");
    for (i, p) in profiles.iter().enumerate() {
        println!("     {}) {}", i + 1, p.display_name);
    }
    println!();
    let choice = prompt_nonempty(&format!("  Select profile (1–{}): ", profiles.len()))?;
    let idx: usize = choice.trim().parse().unwrap_or(0);
    if idx < 1 || idx > profiles.len() {
        println!("  Invalid selection.");
        return Ok(());
    }
    let profile = &profiles[idx - 1];
    println!("\n  Enrolling voice for: {}", profile.display_name);
    println!("  Speak naturally for 10 seconds each time when prompted.\n");
    for i in 1..=3u32 {
        println!("  Sample {}/3 — press ENTER then start speaking", i);
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        let audio = tokio::task::spawn_blocking(|| pond_adapters_whisper::record_wav_sample(10)).await??;
        match adapter.register_speaker(&profile.id, &audio).await {
            Ok(embedding) => println!("  ✅ Sample {} saved ({})\n", i, embedding.id),
            Err(e) => { println!("  ❌ Failed to save sample: {}", e); return Ok(()); }
        }
    }
    let count = adapter.enrollment_count(&profile.id).await.unwrap_or(0);
    println!("  ✅ Enrollment complete for {}! ({} samples stored)", profile.display_name, count);
    Ok(())
}

async fn run_enroll(profile_id: &str, duration_secs: u32, speaker_model: Option<std::path::PathBuf>) -> Result<()> {
    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;
    let model_path = speaker_model.unwrap_or_else(|| data_dir.join("models").join("speaker.onnx"));
    if !model_path.exists() {
        anyhow::bail!("Speaker model not found at {}.\nRun `pond-server setup` first.", model_path.display());
    }
    let adapter = OnnxSpeakerAdapter::new(&model_path, db.system.clone(), db.logs.clone())
        .map_err(|e| anyhow::anyhow!("Failed to load speaker model: {}", e))?;
    let profile_repo = SqliteProfileRepository::new(db.system.clone());
    let display_name = match pond_core::ports::profile::ProfileRepository::get(&profile_repo, profile_id).await? {
        Some(p) => p.display_name,
        None => anyhow::bail!("Profile '{}' not found", profile_id),
    };
    println!("  Enrolling voice for: {}", display_name);
    println!("  Speak naturally for {} seconds each time when prompted.\n", duration_secs);
    for i in 1..=3u32 {
        println!("  Sample {}/3 — press ENTER then start speaking", i);
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        let dur = duration_secs;
        let audio = tokio::task::spawn_blocking(move || pond_adapters_whisper::record_wav_sample(dur)).await??;
        match adapter.register_speaker(profile_id, &audio).await {
            Ok(embedding) => println!("  ✅ Sample {} saved ({})\n", i, embedding.id),
            Err(e) => { println!("  ❌ Failed: {}", e); return Ok(()); }
        }
    }
    let count = adapter.enrollment_count(profile_id).await.unwrap_or(0);
    println!("  ✅ Enrollment complete for {}! ({} samples stored)", display_name, count);
    Ok(())
}

async fn run_onboard(reset: bool) -> Result<()> {
    println!("🦆 Goose In A Pond — Interactive Onboarding Wizard\n");

    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;
    let repo = SqlxOnboardingRepository::new(db.system.clone());
    let service = OnboardingService::new(repo);
    let settings_repo = SqliteSettingsRepository::new(db.system.clone());

    if reset {
        service.reset().await?;
        println!("Onboarding reset. Starting from scratch...\n");
    }

    let mut user_data: HashMap<String, String> = HashMap::new();

    loop {
        let current_step = service.status().await;

        match current_step {
            None => {
                println!("Starting onboarding from scratch...");
                service.start().await?;
            }

            Some(OnboardingStep::Welcome) => {
                println!("Step: Welcome");

                if let Some(ip) = get_local_ip() {
                    println!("Detected device IP: {}", ip);
                    user_data.insert("device_ip".to_string(), ip);
                } else {
                    println!("Could not detect IP, using default 'unknown'.");
                    user_data.insert("device_ip".to_string(), "unknown".to_string());
                }

                service.advance().await?;
            }

            Some(OnboardingStep::Basics) => {
                println!("Step: Basics");

                let username = prompt_nonempty("Enter your name: ")?;
                user_data.insert("user_name".to_string(), username);

                service.advance().await?;
            }

            Some(OnboardingStep::Location) => {
                println!("Step: Language & Location");
                println!("(Press Enter to skip any field — configure later in Settings)");

                print!("Timezone (e.g. Africa/Nairobi): ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut timezone = String::new();
                let _ = std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut timezone);
                if !timezone.trim().is_empty() {
                    user_data.insert("timezone".to_string(), timezone.trim().to_string());
                }

                service.advance().await?;
            }

            Some(OnboardingStep::Accessibility) => {
                println!("Step: Accessibility");
                println!("(All accessibility options can be configured in Settings later)");
                service.advance().await?;
            }

            Some(OnboardingStep::Personality) => {
                println!("Step: Personality");

                let styles = vec!["balanced", "concise", "technical", "warm"];
                println!("Choose a conversation style:");
                for (i, s) in styles.iter().enumerate() {
                    println!("  {}) {}", i + 1, s);
                }

                let selected = loop {
                    let choice = prompt_nonempty("Enter the number of your choice: ")?;
                    if let Ok(index) = choice.parse::<usize>() {
                        if index >= 1 && index <= styles.len() {
                            break styles[index - 1].to_string();
                        }
                    }
                    println!("Invalid choice. Try again.");
                };

                println!("You selected: {}", selected);
                user_data.insert("prompt_style".to_string(), selected);

                service.advance().await?;
            }

            Some(OnboardingStep::GooseIdentity) => {
                println!("Step: Goose's Identity");

                print!("Assistant name (default: Goose): ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut name = String::new();
                let _ = std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut name);
                let name = if name.trim().is_empty() { "Goose".to_string() } else { name.trim().to_string() };
                user_data.insert("assistant_name".to_string(), name);

                service.advance().await?;
            }

            Some(OnboardingStep::WakeWord) => {
                println!("Step: Wake Word");
                println!("Presets: 1) goose  2) hey goose  3) ok computer  4) custom");

                let wake_word = loop {
                    let choice = prompt_nonempty("Enter number or type a custom phrase: ")?;
                    break match choice.trim() {
                        "1" => "goose".to_string(),
                        "2" => "hey goose".to_string(),
                        "3" => "ok computer".to_string(),
                        "4" => prompt_nonempty("Enter your custom wake phrase: ")?,
                        other => other.to_string(),
                    };
                };

                user_data.insert("voice_wake_word".to_string(), wake_word);
                service.advance().await?;
            }

            Some(OnboardingStep::Model) => {
                println!("Step: AI Model");
                // Show LLM models available in the catalog DB so users can pick by number.
                let model_repo = SqliteModelRepository::new(db.system.clone());
                let catalog_models: Vec<_> = model_repo.list_all().await.unwrap_or_default()
                    .into_iter()
                    .filter(|m| m.category.is_llm())
                    .collect();
                let model_name = if catalog_models.is_empty() {
                    println!("(No models in catalog yet — run `pond setup` to populate it)");
                    prompt_nonempty("Enter a model name (e.g. gemma-2b): ")?
                } else {
                    println!("Available models (✓ = downloaded):");
                    for (i, m) in catalog_models.iter().enumerate() {
                        let dl = if m.downloaded { "✓" } else { " " };
                        println!("  {}) [{}] {} ({}, {} MB)",
                            i + 1, dl, m.name, m.category.as_str(), m.size_mb);
                    }
                    println!();
                    let input = prompt_nonempty("Enter number to select, or type a name directly: ")?;
                    match input.parse::<usize>() {
                        Ok(idx) if idx >= 1 && idx <= catalog_models.len() => {
                            catalog_models[idx - 1].name.clone()
                        }
                        _ => input,
                    }
                };
                user_data.insert("chat_model".to_string(), model_name);
                service.advance().await?;
            }

            Some(OnboardingStep::Extensions) => {
                println!("Step: Extensions");
                println!("(Extensions can be enabled from Settings later)");
                service.advance().await?;
            }

            Some(OnboardingStep::Completed) => {
                if user_data.is_empty() {
                    println!("You are already onboarded!");
                    println!("Run with --reset to start over.");
                } else {
                    println!("\nOnboarding complete! Saving your settings...\n");
                    let mut settings = settings_repo.get().await.unwrap_or_default();
                    if let Some(v) = user_data.get("user_name")       { settings.user_name = v.clone(); }
                    if let Some(v) = user_data.get("timezone")        { settings.timezone = v.clone(); }
                    if let Some(v) = user_data.get("prompt_style")    { settings.prompt_style = v.clone(); }
                    if let Some(v) = user_data.get("assistant_name")  { settings.assistant_name = v.clone(); }
                    if let Some(v) = user_data.get("voice_wake_word") { settings.voice_wake_word = v.clone(); }
                    if let Some(v) = user_data.get("chat_model") {
                        settings.chat_model = v.clone();
                        settings.active_llm_model = v.clone();
                    }
                    settings_repo.update(&settings).await?;
                    println!("  Settings saved to database.");
                }
                run_main_menu().await?;
                break;
            }
        }
    }

    Ok(())
}

// ── Goose agent backend ───────────────────────────────────────────────────────

/// Build a Goose-backed agent + extension manager.
///
/// Only compiled when the `goose-agent` feature is enabled (default).
/// Falls back to MockAgent when `--agent mock` is explicitly passed.
#[cfg(feature = "goose-agent")]
async fn build_goose_backend(
    agent_backend: &str,
    llamafile_url: &str,
    data_dir: &std::path::Path,
    weather: Option<Arc<dyn WeatherProvider>>,
    device_registry: Arc<dyn pond_core::ports::device_registry::DeviceRegistry + Send + Sync>,
    scheduler: Option<Arc<dyn pond_core::ports::scheduler::SchedulerPort>>,
    settings_repo: Arc<dyn pond_core::ports::settings::SettingsRepository + Send + Sync>,
    memory_repo: Arc<dyn pond_core::ports::memory_repository::MemoryRepository + Send + Sync>,
    skill_repo: Arc<dyn pond_core::ports::skill::UserSkillRepository + Send + Sync>,
    recipe_repo: Arc<dyn pond_core::ports::recipe::AgentRecipeRepository + Send + Sync>,
    template_repo: Arc<dyn pond_core::ports::prompt_template::PromptTemplateRepository + Send + Sync>,
    extras_repo: Arc<dyn pond_core::ports::prompt_extra::PromptExtraRepository + Send + Sync>,
    voice_mode: bool,
) -> (
    Arc<dyn Agent>,
    Option<Arc<dyn pond_core::ports::extension_manager::ExtensionManagerPort>>,
) {
    use pond_adapters_goose::{GiapServiceHandles, GooseAdapter, register_giap_extension};
    use pond_core::ports::extension_manager::ExtensionManagerPort;

    if agent_backend != "goose" {
        return (Arc::new(MockAgent::new()), None);
    }

    // Register the GIAP MCP server into Goose's builtin extension registry.
    let handles = Arc::new(GiapServiceHandles {
        weather,
        device_registry: device_registry.clone(),
        scheduler,
        settings_repo: settings_repo.clone(),
        memory_repo: memory_repo.clone(),
        skill_repo: skill_repo.clone(),
        recipe_repo: recipe_repo.clone(),
    });
    if let Err(e) = register_giap_extension(handles) {
        tracing::error!("GIAP MCP registration failed: {e} — falling back to mock agent");
        return (Arc::new(MockAgent::new()), None);
    }

    // Build the adapter with all repos injected.
    match GooseAdapter::new(
        settings_repo,
        template_repo,
        extras_repo,
        skill_repo,
        memory_repo,
        device_registry.clone(),
        llamafile_url.to_string(),
        Some(data_dir.to_path_buf()),
    ).await {
        Ok(adapter) => {
            if voice_mode {
                adapter.set_voice_mode(true);
            }
            let ext_mgr: Arc<dyn ExtensionManagerPort> =
                adapter.extension_manager();
            tracing::info!("Goose agent active — GIAP MCP extension registered");
            let agent: Arc<dyn Agent> = Arc::new(adapter);
            (agent, Some(ext_mgr))
        }
        Err(e) => {
            tracing::error!("GooseAdapter init failed: {e} — falling back to mock agent");
            (Arc::new(MockAgent::new()), None)
        }
    }
}

// ── Model catalog helpers ─────────────────────────────────────────────────────

/// Seed the persistent model catalog from upstream sources (static list + local Ollama).
///
/// Upserts all returned records (preserving `is_custom` rows) and sets `downloaded`
/// by checking the filesystem.  A failure to fetch is non-fatal — the server starts
/// with whatever models are already in the DB.
async fn seed_model_catalog(
    repo: &dyn ModelRepository,
    data_dir: &std::path::Path,
) {
    use crate::composite_model_catalog_provider::CompositeModelCatalogProvider;
    use crate::filesystem_model_storage::FilesystemModelStorage;
    use pond_core::ports::model_catalog_provider::ModelCatalogProvider;
    use pond_core::ports::model_storage::ModelStorage;

    let client = reqwest::Client::builder()
        .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();
    let provider = CompositeModelCatalogProvider::new(client);
    let storage  = FilesystemModelStorage::new(data_dir);

    let (models, _binaries) = match provider.fetch().await {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!("Failed to fetch model catalog: {e}. Starting with existing DB records.");
            return;
        }
    };

    let count = models.len();
    for mut record in models {
        record.downloaded = storage.is_present(&record);
        if let Err(e) = repo.upsert(&record).await {
            tracing::warn!("Failed to seed model '{}': {e}", record.name);
        }
    }

    tracing::info!("model catalog seeded ({count} records)");
}

/// Sync role assignments from the join table to the settings KV hot-cache.
///
/// The join table is the source of truth.  If a role has no assignment row yet,
/// the settings KV value is left unchanged (backward-compat with existing installs
/// that only have the legacy single-model settings fields).
async fn sync_assignments_to_settings(
    repo: &dyn ModelRepository,
    settings_repo: &dyn pond_core::ports::settings::SettingsRepository,
) {
    let assignments = match repo.list_assignments().await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("sync_assignments_to_settings: failed to read assignments: {e}");
            return;
        }
    };

    for a in &assignments {
        // model_id format: "{category}/{name}"
        let model_name = a.model_id.split('/').nth(1).unwrap_or(&a.model_id);
        let category   = a.model_id.split('/').next().unwrap_or("");

        match a.role.as_str() {
            "chat" => {
                let provider = category_to_provider(category);
                let _ = settings_repo.set_key("chat_provider", provider).await;
                let _ = settings_repo.set_key("chat_model",    model_name.to_string()).await;
            }
            "think" => {
                let provider = category_to_provider(category);
                let _ = settings_repo.set_key("think_provider", provider).await;
                let _ = settings_repo.set_key("think_model",    model_name.to_string()).await;
            }
            "task" => {
                let provider = category_to_provider(category);
                let _ = settings_repo.set_key("task_provider", provider).await;
                let _ = settings_repo.set_key("task_model",    model_name.to_string()).await;
            }
            "asr" => {
                let _ = settings_repo.set_key("active_whisper_model", model_name.to_string()).await;
            }
            "tts" => {
                let _ = settings_repo.set_key("active_tts_model", model_name.to_string()).await;
                // For piper models also sync voice_tts_voice to the .onnx filename.
                if category == "tts_piper" {
                    if let Ok(Some(record)) = repo.get_by_id(&a.model_id).await {
                        if let Some(fname) = record.filename {
                            let _ = settings_repo.set_key("voice_tts_voice", fname).await;
                        }
                    }
                }
            }
            other => {
                tracing::debug!("sync_assignments_to_settings: unknown role '{other}', skipping");
            }
        }
    }

    if !assignments.is_empty() {
        tracing::info!("synced {} role assignment(s) to settings KV", assignments.len());
    }
}

fn category_to_provider(category: &str) -> String {
    match category {
        "ollama"    => "ollama".to_string(),
        "gguf"      => "local".to_string(),
        _           => "llamafile".to_string(),
    }
}

/// Direct-SQLite model management CLI — no HTTP server started.
async fn run_models(action: ModelAction) -> Result<()> {
    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;
    let repo = Arc::new(SqliteModelRepository::new(db.system.clone()));

    match action {
        ModelAction::List { category } => {
            let models = if let Some(cat_str) = &category {
                match ModelCategory::from_str(cat_str) {
                    Some(cat) => repo.list_by_category(&cat).await?,
                    None => {
                        eprintln!("Unknown category '{cat_str}'. Valid: gguf, llamafile, whisper, tts, ollama");
                        std::process::exit(1);
                    }
                }
            } else {
                repo.list_all().await?
            };

            let assignments = repo.list_assignments().await.unwrap_or_default();

            println!("{:<12} {:<28} {:>8}  {:>6}  {:>10}  Role",
                "Category", "Name", "Size(MB)", "DL?", "RAM(MB)");
            println!("{}", "─".repeat(78));

            for m in &models {
                let role = assignments.iter()
                    .find(|a| a.model_id == m.id)
                    .map(|a| a.role.as_str())
                    .unwrap_or("—");
                let dl  = if m.downloaded { "✓" } else { "✗" };
                let ram = m.ram_estimate_mb
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "—".to_string());
                println!("{:<12} {:<28} {:>8}  {:>6}  {:>10}  {}",
                    m.category.as_str(), m.name, m.size_mb, dl, ram, role);
            }
        }

        ModelAction::Download { category, name } => {
            let cat = ModelCategory::from_str(&category).ok_or_else(|| {
                anyhow::anyhow!("Unknown category '{category}'")
            })?;
            let id = ModelRecord::id_for(&cat, &name);
            let record = repo.get_by_id(&id).await?
                .ok_or_else(|| anyhow::anyhow!("Model '{id}' not found in catalog"))?;

            let url = record.url.as_deref()
                .ok_or_else(|| anyhow::anyhow!("Model '{id}' has no download URL"))?;
            let filename = record.filename.as_deref()
                .ok_or_else(|| anyhow::anyhow!("Model '{id}' has no filename"))?;

            let subdir = match cat {
                ModelCategory::Whisper  => "models",
                ModelCategory::Llamafile => "models/llm",
                ModelCategory::Gguf     => "models/gguf",
                ModelCategory::TtsPiper => "models/tts",
                _                       => "models",
            };
            let dest = data_dir.join(subdir).join(filename);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }

            println!("Downloading {} → {}", url, dest.display());
            model_download::download_file(url, &dest, record.size_mb).await?;

            // For Piper TTS models also download the companion .onnx.json config file.
            if cat == ModelCategory::TtsPiper {
                if let (Some(cf), Some(cu)) = (&record.config_filename, &record.config_url) {
                    let config_dest = data_dir.join(subdir).join(cf);
                    if !config_dest.exists() {
                        println!("Downloading config {} → {}", cu, config_dest.display());
                        if let Err(e) = model_download::download_file(cu, &config_dest, 0).await {
                            println!("⚠  Config download failed (non-fatal): {e}");
                        } else {
                            println!("✓ Downloaded {}", cf);
                        }
                    }
                }
            }

            repo.set_downloaded(&id, true).await?;
            println!("✓ Downloaded {}", filename);
        }

        ModelAction::Delete { category, name } => {
            let cat = ModelCategory::from_str(&category).ok_or_else(|| {
                anyhow::anyhow!("Unknown category '{category}'")
            })?;
            let id = ModelRecord::id_for(&cat, &name);
            let record = repo.get_by_id(&id).await?
                .ok_or_else(|| anyhow::anyhow!("Model '{id}' not found in catalog"))?;

            let assignments = repo.list_assignments().await?;
            if let Some(a) = assignments.iter().find(|a| a.model_id == id) {
                anyhow::bail!(
                    "Model is assigned to role '{}'. Deactivate it first.", a.role
                );
            }

            if let Some(filename) = &record.filename {
                let subdir = match cat {
                    ModelCategory::Whisper   => "models",
                    ModelCategory::Llamafile => "models/llm",
                    ModelCategory::Gguf      => "models/gguf",
                    ModelCategory::TtsPiper  => "models/tts",
                    _                        => "models",
                };
                let path = data_dir.join(subdir).join(filename);
                if path.exists() {
                    std::fs::remove_file(&path)?;
                    println!("✓ Deleted {}", path.display());
                } else {
                    println!("File not on disk (already absent): {}", path.display());
                }
            }
            repo.set_downloaded(&id, false).await?;
        }

        ModelAction::Activate { category, name, role } => {
            let cat = ModelCategory::from_str(&category).ok_or_else(|| {
                anyhow::anyhow!("Unknown category '{category}'")
            })?;
            let id = ModelRecord::id_for(&cat, &name);
            repo.get_by_id(&id).await?
                .ok_or_else(|| anyhow::anyhow!("Model '{id}' not found in catalog"))?;

            use pond_core::domain::model_record::ModelRoleAssignment;
            if !ModelRoleAssignment::category_matches_role(&cat, &role) {
                anyhow::bail!(
                    "Category '{}' is not compatible with role '{}'. \
                     (whisper→asr, tts_piper/tts_http→tts, gguf/llamafile/ollama→chat|think|task)",
                    cat.as_str(), role
                );
            }

            let settings_repo = SqliteSettingsRepository::new(db.system.clone());
            repo.set_assignment(&role, &id).await?;
            sync_assignments_to_settings(&*repo, &settings_repo).await;
            println!("✓ {} assigned to role '{}'", id, role);
        }
    }

    Ok(())
}

// ── Agent CLI ─────────────────────────────────────────────────────────────────

/// Resolve the model role string from a `--role` flag value and the message text.
fn resolve_role(role_arg: &str, message: &str) -> String {
    use pond_core::services::request_classifier::classify_request;
    match role_arg {
        "auto" => match classify_request(message) {
            pond_core::domain::model_role::ModelRole::Think => "think".to_string(),
            pond_core::domain::model_role::ModelRole::Task  => "task".to_string(),
            pond_core::domain::model_role::ModelRole::Chat  => "chat".to_string(),
        },
        other => other.to_string(),
    }
}

/// Stream a single agent request to stdout, printing tool calls to stderr.
///
/// Text tokens are printed as they arrive. Tool calls and results are shown
/// on stderr so they don't pollute piped output. Returns when the stream ends.
async fn stream_agent_response(agent: &Arc<dyn Agent>, request: pond_core::domain::agent::AgentRequest) -> Result<()> {
    use pond_core::domain::agent::AgentStreamEvent;

    let mut stream = agent.chat_stream(request).await?;
    let mut printed_newline = false;

    while let Some(event) = stream.next().await {
        match event? {
            AgentStreamEvent::Status { content } => {
                eprint!("\r\x1b[K  {content}");
                let _ = io::stderr().flush();
            }
            AgentStreamEvent::ToolCall { tool, input, .. } => {
                // Clear the status line, then show the tool call
                let args = input
                    .as_ref()
                    .map(|v| v.to_string())
                    .filter(|s| s != "{}" && s != "null")
                    .unwrap_or_default();
                eprint!("\r\x1b[K");
                if args.is_empty() {
                    eprintln!("  ⚙  {tool}");
                } else {
                    eprintln!("  ⚙  {tool}  {args}");
                }
            }
            AgentStreamEvent::ToolResult { content, .. } => {
                // Show first line of result so the user sees what came back
                let preview = content.lines().next().unwrap_or("(no output)");
                eprintln!("     ↳ {preview}");
            }
            AgentStreamEvent::Text { content } => {
                eprint!("\r\x1b[K"); // clear any trailing status message
                print!("{content}");
                let _ = io::stdout().flush();
                printed_newline = content.ends_with('\n');
            }
            AgentStreamEvent::Done { .. } => {
                if !printed_newline {
                    println!();
                }
                break;
            }
            AgentStreamEvent::Error { content } => {
                eprintln!("\n  error: {content}");
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// One-shot or interactive Goose agent chat from the CLI.
///
/// Builds the full GooseAdapter + GIAP MCP backend (same as `run_server`),
/// streams the response to stdout, then exits (or loops in REPL mode).
async fn run_agent_cmd(action: AgentAction) -> Result<()> {
    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;

    // Build all repos once — shared across Chat, Tools, and Extras arms.
    let settings_repo: Arc<dyn pond_core::ports::settings::SettingsRepository + Send + Sync> =
        Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    let memory_repo: Arc<dyn pond_core::ports::memory_repository::MemoryRepository + Send + Sync> =
        Arc::new(SqliteMemoryRepository::new(db.system.clone()));
    let skill_repo: Arc<dyn pond_core::ports::skill::UserSkillRepository + Send + Sync> =
        Arc::new(SqliteSkillRepository::new(db.system.clone()));
    let recipe_repo: Arc<dyn pond_core::ports::recipe::AgentRecipeRepository + Send + Sync> =
        Arc::new(SqliteRecipeRepository::new(db.system.clone()));
    let template_repo: Arc<dyn pond_core::ports::prompt_template::PromptTemplateRepository + Send + Sync> =
        Arc::new(SqlitePromptTemplateRepository::new(db.system.clone()));
    let extras_repo: Arc<dyn pond_core::ports::prompt_extra::PromptExtraRepository + Send + Sync> =
        Arc::new(SqlitePromptExtraRepository::new(db.system.clone()));
    let device_registry: Arc<dyn pond_core::ports::device_registry::DeviceRegistry + Send + Sync> =
        Arc::new(SqliteDeviceRegistry::new(db.system.clone()));

    let settings = settings_repo.get().await.unwrap_or_default();
    // Use the configured LLM server URL (llamafile default). GooseAdapter uses this to
    // route requests when chat_provider = "llamafile"; for ollama/local it uses its own logic.
    let llamafile_url = format!("http://127.0.0.1:{}", ports::LLAMAFILE);

    // Wire weather from settings so giap__get_current_weather MCP tool is available.
    let weather: Option<Arc<dyn WeatherProvider>> = if settings.weather_enabled
        && (settings.weather_latitude != 0.0 || settings.weather_longitude != 0.0)
    {
        let loc = if settings.weather_location_name.is_empty() {
            format!("{:.3}, {:.3}", settings.weather_latitude, settings.weather_longitude)
        } else {
            settings.weather_location_name.clone()
        };
        Some(Arc::new(OpenMeteoWeatherAdapter::new(
            settings.weather_latitude,
            settings.weather_longitude,
            loc,
        )))
    } else {
        None
    };

    match action {
        AgentAction::Chat { message, session, role } => {
            use pond_core::domain::agent::AgentRequest;

            let model_role = resolve_role(&role, &message);
            eprintln!("  {} | provider: {}  model: {}  role: {}",
                settings.assistant_name, settings.chat_provider, settings.chat_model, model_role);

            let (agent, _ext_mgr) = build_goose_backend(
                "goose",
                &llamafile_url,
                &data_dir,
                weather,
                device_registry,
                None,
                settings_repo,
                memory_repo,
                skill_repo,
                recipe_repo,
                template_repo,
                extras_repo,
                false,
            ).await;

            let request = AgentRequest {
                message,
                session_id: session,
                model_role,
            };
            stream_agent_response(&agent, request).await?;
        }

        AgentAction::Repl { session, role } => {
            use pond_core::domain::agent::AgentRequest;
            use tokio::io::AsyncBufReadExt as _;

            let (agent, _ext_mgr) = build_goose_backend(
                "goose",
                &llamafile_url,
                &data_dir,
                weather,
                device_registry,
                None,
                settings_repo,
                memory_repo,
                skill_repo,
                recipe_repo,
                template_repo,
                extras_repo,
                false,
            ).await;

            eprintln!("  {} — session: {}  (Ctrl+C or 'exit' to quit)",
                settings.assistant_name, session);

            let stdin = tokio::io::BufReader::new(tokio::io::stdin());
            let mut lines = stdin.lines();

            loop {
                eprint!("\nYou: ");
                let _ = io::stderr().flush();

                let line = match lines.next_line().await {
                    Ok(Some(l)) => l,
                    _ => break,
                };
                let message = line.trim().to_string();
                if message.is_empty() {
                    continue;
                }
                if message == "exit" || message == "quit" {
                    break;
                }

                let model_role = resolve_role(&role, &message);
                eprint!("\nPond [{model_role}]: ");
                let _ = io::stderr().flush();

                let request = AgentRequest {
                    message,
                    session_id: session.clone(),
                    model_role,
                };
                if let Err(e) = stream_agent_response(&agent, request).await {
                    eprintln!("\n  error: {e}");
                }
            }
        }

        AgentAction::Tools => {
            let (_agent, ext_mgr) = build_goose_backend(
                "goose",
                &llamafile_url,
                &data_dir,
                weather,
                device_registry,
                None,
                settings_repo,
                memory_repo,
                skill_repo,
                recipe_repo,
                template_repo,
                extras_repo,
                false,
            ).await;

            match ext_mgr {
                None => println!("No extension manager available (agent backend may be 'mock')."),
                Some(mgr) => {
                    let extensions = mgr.list_extensions().await.unwrap_or_default();
                    if extensions.is_empty() {
                        println!("No extensions loaded yet (start the server to initialise sessions).");
                    } else {
                        println!("{:<20} {}", "Extension", "Tools");
                        println!("{}", "─".repeat(60));
                        for ext in &extensions {
                            let tools = ext.tools.join(", ");
                            println!("{:<20} {}", ext.name, tools);
                        }
                    }
                }
            }
        }

        AgentAction::Extras => {
            let extras = extras_repo.list_all().await?;
            if extras.is_empty() {
                println!("No prompt extras defined. Add via POST /api/v1/agent/extras");
                return Ok(());
            }
            println!("{:<4} {:<20} {:<6} {}", "Ord", "Key", "Active", "Instruction");
            println!("{}", "─".repeat(72));
            for e in &extras {
                let active = if e.active { "✓" } else { "✗" };
                let preview = if e.instruction.len() > 40 {
                    format!("{}…", &e.instruction[..39])
                } else {
                    e.instruction.clone()
                };
                println!("{:<4} {:<20} {:<6} {}", e.sort_order, e.key, active, preview);
            }
        }
    }

    Ok(())
}

// ── Prompts CLI ───────────────────────────────────────────────────────────────

async fn run_prompts_cmd(action: PromptAction) -> Result<()> {
    use pond_core::ports::prompt_template::PromptTemplateRepository as _;

    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;
    let repo = SqlitePromptTemplateRepository::new(db.system.clone());

    match action {
        PromptAction::List => {
            let templates = repo.list().await?;
            if templates.is_empty() {
                println!("No templates found. Run `pond setup` to seed built-ins.");
                return Ok(());
            }
            println!("{:<16} {:<8} {}", "Name", "System", "Description");
            println!("{}", "─".repeat(60));
            for t in &templates {
                let sys = if t.is_system { "✓" } else { "—" };
                println!("{:<16} {:<8} {}", t.name, sys, t.description);
            }
        }

        PromptAction::Show { name } => {
            match repo.get(&name).await? {
                None => {
                    eprintln!("Template '{name}' not found.");
                    std::process::exit(1);
                }
                Some(t) => {
                    println!("─── {} ─── (system={})", t.name, t.is_system);
                    println!("{}", t.content);
                }
            }
        }

        PromptAction::Reset { name } => {
            use pond_core::domain::prompt_template::PromptTemplate;
            use pond_core::prompts::{PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM};

            let (content, description) = match name.as_str() {
                "balanced"  => (PROMPT_BALANCED, "Warm, practical, complete behaviour rules. Default for most households."),
                "concise"   => (PROMPT_CONCISE,  "Minimal, action-first. For power users who want brevity."),
                "technical" => (PROMPT_TECHNICAL,"Verbose, tool-aware, narrates reasoning. For developers."),
                "warm"      => (PROMPT_WARM,     "Conversational, family-friendly, personality-forward."),
                other => {
                    eprintln!("'{other}' is not a built-in template. Only balanced | concise | technical | warm can be reset.");
                    std::process::exit(1);
                }
            };
            let t = PromptTemplate {
                name: name.clone(),
                content: content.to_string(),
                description: description.to_string(),
                is_system: true,
                updated_at: String::new(),
            };
            repo.upsert(&t).await?;
            println!("✓ Template '{name}' reset to factory default.");
        }
    }

    Ok(())
}

// ── Skills CLI ────────────────────────────────────────────────────────────────

async fn run_skills_cmd(action: SkillAction) -> Result<()> {
    use pond_core::ports::skill::UserSkillRepository as _;

    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;
    let repo = SqliteSkillRepository::new(db.system.clone());

    match action {
        SkillAction::List { all } => {
            let skills = if all {
                repo.list_all().await?
            } else {
                repo.list_active().await?
            };
            if skills.is_empty() {
                let hint = if all { "" } else { " (use --all to include inactive)" };
                println!("No skills found{hint}.");
                return Ok(());
            }
            println!("{:<38} {:<6} {}", "ID", "Active", "Name");
            println!("{}", "─".repeat(60));
            for s in &skills {
                let active = if s.active { "✓" } else { "✗" };
                println!("{:<38} {:<6} {}", s.id, active, s.name);
            }
        }

        SkillAction::Add { name, content } => {
            use pond_core::domain::skill::UserSkill;

            let content = match content {
                Some(c) => c,
                None => {
                    eprintln!("Reading skill content from stdin (Ctrl-D to finish)...");
                    let mut buf = String::new();
                    use std::io::Read as _;
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf.trim().to_string()
                }
            };
            if content.is_empty() {
                eprintln!("Skill content cannot be empty.");
                std::process::exit(1);
            }
            let id = uuid::Uuid::new_v4().to_string();
            let skill = UserSkill {
                id: id.clone(),
                name: name.clone(),
                content,
                active: true,
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            repo.create(&skill).await?;
            println!("✓ Skill '{name}' created (id: {id})");
        }

        SkillAction::Toggle { id } => {
            use pond_core::ports::skill::UserSkillRepository as _;

            let skill = repo.get(&id).await?
                .ok_or_else(|| anyhow::anyhow!("Skill '{id}' not found"))?;
            let updated = pond_core::domain::skill::UserSkill {
                active: !skill.active,
                ..skill.clone()
            };
            repo.update(&updated).await?;
            let state = if updated.active { "enabled" } else { "disabled" };
            println!("✓ Skill '{}' {state}", skill.name);
        }

        SkillAction::Remove { id } => {
            repo.delete(&id).await?;
            println!("✓ Skill {id} deleted.");
        }
    }

    Ok(())
}

// ── Recipes CLI ───────────────────────────────────────────────────────────────

async fn run_recipes_cmd(action: RecipeAction) -> Result<()> {
    use pond_core::ports::recipe::AgentRecipeRepository as _;

    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;
    let repo = SqliteRecipeRepository::new(db.system.clone());

    match action {
        RecipeAction::List => {
            let recipes = repo.list().await?;
            if recipes.is_empty() {
                println!("No recipes found. Import one with `pond recipes import <name> <file.yaml>`");
                return Ok(());
            }
            println!("{:<38} {:<6} {:<20} {}", "ID", "Active", "Name", "Description");
            println!("{}", "─".repeat(80));
            for r in &recipes {
                let active = if r.active { "✓" } else { "✗" };
                let desc = if r.description.len() > 30 {
                    format!("{}…", &r.description[..29])
                } else {
                    r.description.clone()
                };
                println!("{:<38} {:<6} {:<20} {}", r.id, active, r.name, desc);
            }
        }

        RecipeAction::Show { name } => {
            match repo.get_by_name(&name).await? {
                None => {
                    eprintln!("Recipe '{name}' not found.");
                    std::process::exit(1);
                }
                Some(r) => {
                    println!("─── {} ─── (active={})", r.name, r.active);
                    if !r.description.is_empty() {
                        println!("# {}\n", r.description);
                    }
                    println!("{}", r.yaml);
                }
            }
        }

        RecipeAction::Import { name, file, description } => {
            use pond_core::domain::recipe::AgentRecipe;

            let yaml = tokio::fs::read_to_string(&file).await
                .map_err(|e| anyhow::anyhow!("Cannot read '{}': {e}", file.display()))?;

            let id = uuid::Uuid::new_v4().to_string();
            let recipe = AgentRecipe {
                id: id.clone(),
                name: name.clone(),
                description,
                yaml,
                active: true,
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            repo.upsert(&recipe).await?;
            println!("✓ Recipe '{name}' imported (id: {id})");
        }

        RecipeAction::Remove { name } => {
            match repo.get_by_name(&name).await? {
                None => {
                    eprintln!("Recipe '{name}' not found.");
                    std::process::exit(1);
                }
                Some(r) => {
                    repo.delete(&r.id).await?;
                    println!("✓ Recipe '{name}' deleted.");
                }
            }
        }
    }

    Ok(())
}

// ── Memories CLI ──────────────────────────────────────────────────────────────

async fn run_memories_cmd(action: MemoryAction) -> Result<()> {
    use pond_core::ports::memory_repository::MemoryRepository as _;

    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;
    let repo = SqliteMemoryRepository::new(db.system.clone());

    match action {
        MemoryAction::List { limit } => {
            let fragments = repo.search_recent(None, limit).await?;
            if fragments.is_empty() {
                println!("No memory fragments found.");
                return Ok(());
            }
            println!("{:<38} {:<24} {}", "ID", "Created", "Content");
            println!("{}", "─".repeat(80));
            for f in &fragments {
                let ts = f.created_at.format("%Y-%m-%d %H:%M").to_string();
                let preview = if f.content.len() > 40 {
                    format!("{}…", &f.content[..39])
                } else {
                    f.content.clone()
                };
                println!("{:<38} {:<24} {}", f.id, ts, preview);
            }
        }

        MemoryAction::Add { content } => {
            use pond_core::domain::memory::MemoryFragment;

            let id = uuid::Uuid::new_v4().to_string();
            let fragment = MemoryFragment {
                id: id.clone(),
                profile_id: None,
                session_id: None,
                content: content.clone(),
                embedding: None,
                source: "cli".to_string(),
                tags: vec![],
                created_at: chrono::Utc::now(),
            };
            repo.add(fragment).await?;
            println!("✓ Memory saved (id: {id})");
        }

        MemoryAction::Remove { id } => {
            repo.delete(&id).await?;
            println!("✓ Memory {id} deleted.");
        }
    }

    Ok(())
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── category_to_provider ──────────────────────────────────────────────────

    #[test]
    fn ollama_category_maps_to_ollama_provider() {
        assert_eq!(category_to_provider("ollama"), "ollama");
    }

    #[test]
    fn gguf_category_maps_to_local_provider() {
        assert_eq!(category_to_provider("gguf"), "local");
    }

    #[test]
    fn llamafile_category_maps_to_llamafile_provider() {
        assert_eq!(category_to_provider("llamafile"), "llamafile");
    }

    #[test]
    fn unknown_category_defaults_to_llamafile() {
        assert_eq!(category_to_provider("tts_piper"), "llamafile");
        assert_eq!(category_to_provider("whisper"),   "llamafile");
        assert_eq!(category_to_provider("unknown"),   "llamafile");
    }

    // ── sync_assignments_to_settings ──────────────────────────────────────────

    #[tokio::test]
    async fn sync_ollama_chat_assignment_updates_settings() {
        use pond_core::domain::model_record::{ModelCategory, ModelRecord};
        use pond_core::ports::model_repository::ModelRepository;
        use pond_core::ports::settings::SettingsRepository;
        use pond_infra::db::Database;
        use pond_infra::sqlite_model_repository::SqliteModelRepository;
        use pond_infra::sqlite_settings::SqliteSettingsRepository;

        let tmp = tempfile::tempdir().unwrap();
        let db  = Database::init(tmp.path()).await.unwrap();
        let repo          = SqliteModelRepository::new(db.system.clone());
        let settings_repo = SqliteSettingsRepository::new(db.system.clone());

        let model = ModelRecord {
            id:               "ollama/llama3.2".to_string(),
            category:         ModelCategory::Ollama,
            name:             "llama3.2".to_string(),
            filename:         None,
            description:      "Ollama Llama 3.2".to_string(),
            size_mb:          0,
            url:              None,
            hf_id:            None, ram_estimate_mb: None, recommended_role: None,
            context_length:   None, quantization: None, asr_language: None,
            asr_size:         None, tts_engine: None, tts_voice_name: None,
            config_filename:  None, config_url: None, tts_url: None,
            sample_rate:      None, downloaded: true, is_custom: false,
        };
        repo.upsert(&model).await.unwrap();
        repo.set_assignment("chat", "ollama/llama3.2").await.unwrap();

        sync_assignments_to_settings(&repo, &settings_repo).await;

        let settings = settings_repo.get().await.unwrap();
        assert_eq!(settings.chat_provider, "ollama");
        assert_eq!(settings.chat_model,    "llama3.2");
    }

    #[tokio::test]
    async fn sync_gguf_chat_assignment_sets_local_provider() {
        use pond_core::domain::model_record::{ModelCategory, ModelRecord};
        use pond_core::ports::model_repository::ModelRepository;
        use pond_core::ports::settings::SettingsRepository;
        use pond_infra::db::Database;
        use pond_infra::sqlite_model_repository::SqliteModelRepository;
        use pond_infra::sqlite_settings::SqliteSettingsRepository;

        let tmp = tempfile::tempdir().unwrap();
        let db  = Database::init(tmp.path()).await.unwrap();
        let repo          = SqliteModelRepository::new(db.system.clone());
        let settings_repo = SqliteSettingsRepository::new(db.system.clone());

        let model = ModelRecord {
            id:               "gguf/llama-3b".to_string(),
            category:         ModelCategory::Gguf,
            name:             "llama-3b".to_string(),
            filename:         Some("llama-3b.gguf".to_string()),
            description:      "GGUF Llama 3B".to_string(),
            size_mb:          2000,
            url:              Some("https://example.com/llama-3b.gguf".to_string()),
            hf_id:            None, ram_estimate_mb: Some(3000), recommended_role: None,
            context_length:   None, quantization: Some("Q4_K_M".to_string()),
            asr_language:     None, asr_size: None, tts_engine: None, tts_voice_name: None,
            config_filename:  None, config_url: None, tts_url: None,
            sample_rate:      None, downloaded: false, is_custom: false,
        };
        repo.upsert(&model).await.unwrap();
        repo.set_assignment("chat", "gguf/llama-3b").await.unwrap();

        sync_assignments_to_settings(&repo, &settings_repo).await;

        let settings = settings_repo.get().await.unwrap();
        assert_eq!(settings.chat_provider, "local",    "gguf category should map to 'local' provider");
        assert_eq!(settings.chat_model,    "llama-3b", "model name should be extracted from id");
    }

    #[tokio::test]
    async fn sync_think_and_task_roles_are_also_synced() {
        use pond_core::domain::model_record::{ModelCategory, ModelRecord};
        use pond_core::ports::model_repository::ModelRepository;
        use pond_core::ports::settings::SettingsRepository;
        use pond_infra::db::Database;
        use pond_infra::sqlite_model_repository::SqliteModelRepository;
        use pond_infra::sqlite_settings::SqliteSettingsRepository;

        let tmp = tempfile::tempdir().unwrap();
        let db  = Database::init(tmp.path()).await.unwrap();
        let repo          = SqliteModelRepository::new(db.system.clone());
        let settings_repo = SqliteSettingsRepository::new(db.system.clone());

        let model = ModelRecord {
            id: "ollama/gemma2".to_string(), category: ModelCategory::Ollama,
            name: "gemma2".to_string(), filename: None,
            description: String::new(), size_mb: 0, url: None,
            hf_id: None, ram_estimate_mb: None, recommended_role: None,
            context_length: None, quantization: None, asr_language: None,
            asr_size: None, tts_engine: None, tts_voice_name: None,
            config_filename: None, config_url: None, tts_url: None,
            sample_rate: None, downloaded: true, is_custom: false,
        };
        repo.upsert(&model).await.unwrap();
        repo.set_assignment("think", "ollama/gemma2").await.unwrap();
        repo.set_assignment("task",  "ollama/gemma2").await.unwrap();

        sync_assignments_to_settings(&repo, &settings_repo).await;

        let settings = settings_repo.get().await.unwrap();
        assert_eq!(settings.think_provider.as_deref(), Some("ollama"));
        assert_eq!(settings.think_model.as_deref(),    Some("gemma2"));
        assert_eq!(settings.task_provider.as_deref(),  Some("ollama"));
        assert_eq!(settings.task_model.as_deref(),     Some("gemma2"));
    }
}
