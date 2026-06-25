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

mod composite_model_catalog_provider;
mod filesystem_model_storage;
mod http_model_downloader;
mod inference_pool;
mod llamafile_process;
mod llm_memory_consolidator;
mod llm_memory_extractor;
mod model_download;
#[cfg(feature = "legacy-subprocess")]
mod piper_http;
#[cfg(feature = "legacy-subprocess")]
mod piper_process;
mod ports;
mod reqwest_model_downloader;
mod schedule_executors;
mod startup;
mod system_deps;
mod three_stage_consolidator;
mod tracing_setup;
#[cfg(feature = "legacy-subprocess")]
mod whisper_process;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt as _;
use pond_adapters_llamafile::LlamafileProvider;
use pond_adapters_ollama::OllamaProvider;
#[cfg(feature = "legacy-subprocess")]
use pond_adapters_piper::PiperOutput;
use pond_adapters_piper::PiperRsOutput;
use pond_adapters_weather::{OpenMeteoWeatherAdapter, WeatherProvider};
#[cfg(feature = "legacy-subprocess")]
use pond_adapters_whisper::WhisperInput;
use pond_adapters_whisper::{WhisperKeywordDetector, WhisperRsInput};
use pond_api::{AppState, LlamafileManager};
use pond_core::mcp::ports::mcp_server::McpServerRepository as _;
use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::models::ports::voice_input::VoiceInput;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::models::services::instant_activation::InstantActivation;
use pond_core::prompts::build_system_prompt;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::shared::services::chat::ChatService;
use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
use pond_core::shared::services::print_output::PrintOutput;
use pond_core::shared::services::stdin_input::StdinInput;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository as _;
use pond_core::user_data::services::onboarding::OnboardingService;
use pond_infra::db::Database;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_draft::SqliteDraftRepository;
use pond_infra::sqlite_event_log::{SqliteEventLog, SqliteEventLogRepository};
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use pond_infra::sqlite_mcp_servers::SqliteMcpServerRepository;
use pond_infra::sqlite_memory::SqliteMemoryRepository;
use pond_infra::sqlite_model_repository::SqliteModelRepository;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_sensor::{SqliteCameraStorage, SqliteSensorStorage};
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use pond_infra::sqlite_telemetry::SqliteTelemetry;
use pond_infra_scheduler::CronSchedulerAdapter;
use schedule_executors::{AgentScheduleExecutor, DeferredExecutor};
use std::collections::HashMap;
use std::io::{self, Write};
use std::net::UdpSocket;
use std::sync::Arc;

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

        /// Agent backend: "goose" (default, full-featured) | "pond" (independent, KV-cache reuse) | "mock".
        /// Also configurable via PUT /api/v1/settings with agent_backend field.
        #[arg(long, default_value = "goose")]
        agent: String,

        /// Port to listen on (defaults to 4000)
        #[arg(long)]
        port: Option<u16>,

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

fn main() -> Result<()> {
    let num_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    // On Jetson Orin Nano (6 cores), cap at 4 to leave headroom for OS + audio.
    // On dev machines, use all cores.
    let workers = if num_cpus <= 6 {
        num_cpus.min(4)
    } else {
        num_cpus
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?;
    runtime.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = default_data_dir();

    match cli.command {
        Some(Commands::Setup { model }) => run_setup(&model).await,
        Some(Commands::Serve {
            static_dir,
            open,
            debug,
            agent,
            port,
            native,
        }) => {
            let drain = tracing_setup::init_tracing(debug, &data_dir);
            run_server(static_dir, open, debug, &agent, port, native, drain).await
        }
        Some(Commands::Chat {
            provider,
            model,
            input,
            wake_word,
            no_wake_word,
            tts,
            tts_model,
        }) => {
            let _log = tracing_setup::init_tracing(false, &data_dir);
            run_chat(
                provider.as_deref(),
                model.as_deref(),
                &input,
                wake_word.as_deref(),
                no_wake_word,
                tts.as_deref(),
                tts_model,
            )
            .await
        }
        Some(Commands::Status) => run_status().await,
        Some(Commands::Onboard { reset }) => {
            if let Err(err) = run_onboard(reset).await {
                eprintln!("Error: {:?}", err);
            }
            Ok(())
        }
        Some(Commands::Models { action }) => run_models(action).await,
        Some(Commands::Agent { action }) => {
            let _log = tracing_setup::init_tracing(false, &data_dir);
            run_agent_cmd(action).await
        }
        Some(Commands::Prompts { action }) => run_prompts_cmd(action).await,
        Some(Commands::Skills { action }) => run_skills_cmd(action).await,
        Some(Commands::Recipes { action }) => run_recipes_cmd(action).await,
        Some(Commands::Memories { action }) => run_memories_cmd(action).await,
        Some(Commands::Calibrate {
            phrase,
            samples,
            whisper_url,
            reset,
        }) => run_calibrate(phrase.as_deref(), samples, whisper_url.as_deref(), reset).await,
        None => {
            // Default: run interactive chat (backward compat) — provider comes from Settings
            let _log = tracing_setup::init_tracing(false, &data_dir);
            run_chat(None, None, "stdin", None, true, Some("none"), None).await
        }
    }
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
        println!(
            "  ⚠  Some system deps could not be installed — see docs/developer/linux-setup.md"
        );
        println!("     Continuing setup; some features may not work until deps are installed.");
    }

    // Step 2: Initialize databases + seed model catalog
    println!("\n  [2/6] Initializing databases...");
    let db_setup = Database::init(&data_dir).await?;
    println!("  ✅ Databases ready");

    // One-shot HF cache migration — moves pre-existing flat model files into
    // the content-addressed blob layout so subsequent downloads dedupe. Errors
    // are logged but never block startup. Idempotent via filesystem marker.
    match pond_server::hf_cache_migration::migrate_flat_files_to_blobs(&data_dir).await {
        Ok(r) if r.is_empty() => {}
        Ok(r) => {
            println!(
                "  📦 HF cache migration: scanned {}, migrated {}, skipped {} symlinks, {} errors",
                r.scanned,
                r.migrated,
                r.skipped_symlinks,
                r.errors.len()
            );
            for e in &r.errors {
                tracing::warn!(target: "hf_cache_migration", "{e}");
            }
        }
        Err(e) => tracing::warn!(target: "hf_cache_migration", "migration failed: {e}"),
    }

    let setup_model_repo = SqliteModelRepository::new(db_setup.system.clone());
    println!("  📋 Fetching model catalog from upstream sources...");
    seed_model_catalog(&setup_model_repo, &data_dir).await;

    // Seed built-in prompt templates (INSERT OR IGNORE — never overwrites user edits)
    {
        use pond_core::prompts::{PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM};
        use pond_core::user_data::domain::prompt_template::PromptTemplate;
        #[allow(unused_imports)]
        use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;

        let template_repo = SqlitePromptTemplateRepository::new(db_setup.system.clone());
        let built_ins = [
            (
                "balanced",
                PROMPT_BALANCED,
                "Warm, practical, complete behaviour rules. Default for most households.",
            ),
            (
                "concise",
                PROMPT_CONCISE,
                "Minimal, action-first. For power users who want brevity.",
            ),
            (
                "technical",
                PROMPT_TECHNICAL,
                "Verbose, tool-aware, narrates reasoning. For developers.",
            ),
            (
                "warm",
                PROMPT_WARM,
                "Conversational, family-friendly, personality-forward.",
            ),
        ];
        for (name, content, description) in built_ins {
            let t = PromptTemplate {
                name: name.to_string(),
                content: content.to_string(),
                description: description.to_string(),
                is_system: true,
                updated_at: String::new(),
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
        use pond_core::models::ports::model_storage::ModelStorage as _;
        let storage = FilesystemModelStorage::new(&data_dir);
        let model_id = format!("whisper/{}", effective_model);
        match setup_model_repo.get_by_id(&model_id).await.ok().flatten() {
            Some(r) => {
                let path = storage.path_for(&r).unwrap_or_else(|| {
                    data_dir
                        .join("models")
                        .join(format!("ggml-{}.en.bin", effective_model))
                });
                (path, r.url.unwrap_or_default(), r.size_mb)
            }
            None => {
                let path = data_dir
                    .join("models")
                    .join(format!("ggml-{}.en.bin", effective_model));
                (path, String::new(), 0u64)
            }
        }
    };
    println!(
        "\n  [3/6] Downloading Whisper ASR model ({})...",
        effective_model
    );
    println!("  📁 Target: {}", expected_path.display());
    if expected_path.exists() {
        println!("  ✅ Already downloaded: {}", expected_path.display());
    } else if !whisper_dl_url.is_empty() {
        if let Some(parent) = expected_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        model_download::download_file(&whisper_dl_url, &expected_path, whisper_dl_mb).await?;
    } else {
        println!(
            "  ⚠  Model '{}' not found in catalog — skipping download",
            effective_model
        );
    }

    // Step 4: whisper-server binary — only with the legacy-subprocess escape valve.
    // Default build runs Whisper in-process via whisper-rs; no second binary needed.
    #[cfg(feature = "legacy-subprocess")]
    {
        println!("\n  [4/6] Downloading whisper-server binary (legacy-subprocess)...");
        let _ = model_download::download_whisper_binary(&data_dir).await;
    }
    #[cfg(not(feature = "legacy-subprocess"))]
    {
        println!("\n  [4/6] Whisper runs in-process — no binary download needed.");
    }

    // Step 5: Piper TTS binary — only with the legacy-subprocess escape valve.
    // Default build runs Piper in-process via piper-rs.
    #[cfg(feature = "legacy-subprocess")]
    {
        println!("\n  [5/6] Setting up Piper TTS subprocess (legacy-subprocess)...");
        let piper_bin_ok = model_download::download_piper_binary(&data_dir)
            .await
            .is_ok();
        if !piper_bin_ok {
            println!("  ⚠  Piper binary unavailable — voice output will be text-only.");
            println!("     Install piper manually or retry setup.");
        } else {
            println!("  ✅ Piper binary ready — select a voice model in the web Settings page.");
        }
    }
    #[cfg(not(feature = "legacy-subprocess"))]
    {
        println!(
            "\n  [5/6] Piper runs in-process — select a voice model in the web Settings page."
        );
    }

    // Step 6: ONNX Runtime — detect or auto-download
    println!("\n  [6/7] Checking ONNX Runtime...");
    ensure_onnx_runtime();

    // Step 7 (face-onnx feature only): face recognition models
    #[cfg(feature = "face-onnx")]
    {
        println!("\n  [7/7] Setting up face recognition models...");
        if let Err(e) = model_download::download_face_models(&data_dir).await {
            println!("  ⚠  Face model setup failed: {} — face recognition will be disabled until you add the files manually", e);
        }
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
    data_dir: std::path::PathBuf,
    model_service: Arc<pond_core::models::services::model_service::ModelService>,
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
        let data_dir = self.data_dir.clone();
        let model_service = self.model_service.clone();
        let model_hint = model_name.map(|s| s.to_string());
        let guard_arc = Arc::clone(&self.guard);
        let actual_port_arc = Arc::clone(&self.actual_port);

        tokio::spawn(async move {
            // Double-check under the lock to avoid a race where two concurrent
            // requests both reach the is_running() fast-path as false.
            let mut guard = guard_arc.lock().await;
            let cur = actual_port_arc.load(std::sync::atomic::Ordering::Acquire);
            if llamafile_process::is_running(cur).await {
                return; // someone else already started it
            }
            match llamafile_process::try_start(&data_dir, model_service, model_hint.as_deref())
                .await
            {
                Some((proc, port)) => {
                    actual_port_arc.store(port, std::sync::atomic::Ordering::Release);
                    tracing::info!("llamafile started on port {}", port);
                    *guard = Some(proc);
                }
                None => {
                    tracing::warn!(
                        "llamafile could not be started (no model found or already running)"
                    );
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
        let url = llamafile_process::url_for(port);

        // Fast path: already running
        if llamafile_process::is_running(port).await {
            return (url, true);
        }

        // Kick off background startup (reuses existing spawn+lock logic)
        self.ensure_started(model_name).await;

        // Poll until ready or timeout, re-reading actual_port each iteration
        // in case a just-started process chose a different port.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
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
        model_service: Arc<pond_core::models::services::model_service::ModelService>,
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

async fn run_server(
    static_dir: std::path::PathBuf,
    open: bool,
    debug: bool,
    agent_backend: &str,
    port: Option<u16>,
    native: bool,
    drain_handle: tracing_setup::LogDrainHandle,
) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!(
        "  ║   🦆  Goose In A Pond  v{}         ║",
        env!("CARGO_PKG_VERSION")
    );
    println!("  ╚═══════════════════════════════════════╝");

    // Ensure the ONNX Runtime shared library is available — check system
    // paths first, then auto-download from GitHub Releases if needed.
    // Must run before any ONNX-dependent init (face recognition, embeddings).
    ensure_onnx_runtime();

    // Bake in the face-recognition runtime defaults so the server Just Works
    // on a fresh install without the operator having to remember a four-line
    // env-var incantation.  Every var stays overridable — we only set it
    // when it is currently *unset*.
    apply_face_recognition_defaults();

    // Initialize databases
    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;

    // One-shot HF cache migration — moves pre-existing flat model files into
    // the content-addressed blob layout. Idempotent via filesystem marker; on
    // a migrated install the call costs one stat() and returns immediately.
    // Errors per-file are logged but never block startup.
    match pond_server::hf_cache_migration::migrate_flat_files_to_blobs(&data_dir).await {
        Ok(r) if r.is_empty() => {}
        Ok(r) => {
            tracing::info!(
                target: "hf_cache_migration",
                scanned = r.scanned,
                migrated = r.migrated,
                skipped_symlinks = r.skipped_symlinks,
                errors = r.errors.len(),
                "flat-file migration complete"
            );
            for e in &r.errors {
                tracing::warn!(target: "hf_cache_migration", "{e}");
            }
        }
        Err(e) => tracing::warn!(target: "hf_cache_migration", "migration failed: {e}"),
    }

    // Soft system-dep check (non-fatal — just warn if something looks wrong)
    system_deps::warn_if_missing();

    // ── Load settings early (drives model selection) ─────────────────────────
    let settings_repo_early = SqliteSettingsRepository::new(db.system.clone());
    let settings = settings_repo_early.get().await.unwrap_or_default();

    // Override agent_backend from DB settings (UI can change it without CLI restart).
    // CLI flag takes precedence only when explicitly set to something other than "goose".
    let agent_backend = if agent_backend == "goose" && !settings.agent_backend.is_empty() {
        &settings.agent_backend
    } else {
        agent_backend
    };

    // Q2-05: pond-agent is quarantined — experimental backend not ready for production.
    // Force goose even if the setting was written as "pond".
    let agent_backend: &str = if agent_backend == "pond" {
        tracing::warn!(
            "pond-agent backend is quarantined (not production-ready); \
             falling back to goose. Set agent_backend to \"goose\" in Settings to suppress this warning."
        );
        "goose"
    } else {
        agent_backend
    };

    // ── Component startup: auto-download + wire critical services ────────────
    println!("\n  ── Components ──────────────────────────────────────");

    // STT — whisper ggml model download (the in-process backend reads the
    // same `.bin` files the legacy subprocess used).
    let whisper_model_path: Option<std::path::PathBuf> = if settings.active_whisper_model.is_empty()
    {
        println!("  ⏭  STT: whisper skipped (no whisper model configured in Settings)");
        None
    } else {
        let (whisper_filename, whisper_url, whisper_mb) =
            SqliteModelRepository::new(db.system.clone())
                .get_by_id(&format!("whisper/{}", settings.active_whisper_model))
                .await
                .ok()
                .flatten()
                .map(|r| {
                    (
                        r.filename.unwrap_or_else(|| {
                            format!("ggml-{}.en.bin", &settings.active_whisper_model)
                        }),
                        r.url.unwrap_or_default(),
                        r.size_mb,
                    )
                })
                .unwrap_or_else(|| {
                    (
                        format!("ggml-{}.en.bin", &settings.active_whisper_model),
                        String::new(),
                        0u64,
                    )
                });
        let whisper_model = data_dir.join("models").join(&whisper_filename);
        if !whisper_model.exists() {
            println!(
                "  📥 STT model not found — downloading ({})...",
                settings.active_whisper_model
            );
            if !whisper_url.is_empty() {
                if let Some(parent) = whisper_model.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                match model_download::download_file(&whisper_url, &whisper_model, whisper_mb).await
                {
                    Ok(_) => {}
                    Err(e) => println!("  ⚠  STT model download failed: {}", e),
                }
            } else {
                println!(
                    "  ⚠  STT model '{}' not in catalog — cannot download",
                    settings.active_whisper_model
                );
            }
        }
        Some(whisper_model)
    };

    // Optional legacy subprocess — only compiled in with the escape-valve feature.
    #[allow(unused_mut, unused_assignments)]
    let mut whisper_port = ports::WHISPER;
    #[cfg(feature = "legacy-subprocess")]
    let _whisper_guard = if let Some(ref whisper_model) = whisper_model_path {
        if !model_download::whisper_binary_path(&data_dir).exists() {
            println!("  📥 STT binary not found — downloading...");
            if let Err(e) = model_download::download_whisper_binary(&data_dir).await {
                println!("  ⚠  STT binary download failed: {}", e);
            }
        }
        let (guard, port) = whisper_process::try_start(&data_dir, whisper_model).await;
        whisper_port = port;
        guard
    } else {
        None
    };
    #[cfg(not(feature = "legacy-subprocess"))]
    let _whisper_guard: Option<()> = None;

    // Honour an explicit settings override; otherwise compose the loopback URL.
    // Used by the (legacy) audio-transcribe / calibrate HTTP routes in pond-api.
    const DEFAULT_WHISPER_URL: &str = "http://127.0.0.1:9000";
    let whisper_url = if !settings.voice_whisper_url.is_empty()
        && settings.voice_whisper_url != DEFAULT_WHISPER_URL
    {
        settings.voice_whisper_url.clone()
    } else {
        format!("http://127.0.0.1:{}", whisper_port)
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
    //
    // In-process build (default): download the .onnx + .onnx.json voice model
    // and the espeak-ng-data directory. No `piper` binary needed any more —
    // piper-rs loads the ONNX model directly via ort.
    //
    // Legacy build (--features legacy-subprocess): additionally download the
    // `piper` executable.
    let piper_is_primary = settings.active_tts_model.starts_with("piper");
    if piper_is_primary {
        if let Some(ref piper_model_path) = piper_model {
            #[cfg(feature = "legacy-subprocess")]
            {
                if !model_download::piper_binary_path(&data_dir).exists() {
                    println!("  📥 Piper binary not found — downloading...");
                    let _ = model_download::download_piper_binary(&data_dir).await;
                }
            }
            if !piper_model_path.exists() {
                // Look up the exact voice in the DB to get the correct download URL.
                let voice_filename = &settings.voice_tts_voice;
                let registry_entry = SqliteModelRepository::new(db.system.clone())
                    .list_by_category(&ModelCategory::TtsPiper)
                    .await
                    .unwrap_or_default()
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
                    let _ = model_download::download_piper_model_entry(
                        &data_dir, &mf, &cf, &mu, &cu, sz,
                    )
                    .await;
                } else {
                    println!(
                        "  ⚠  Piper voice '{}' not in model catalog — cannot download",
                        voice_filename
                    );
                }
            }
            model_download::ensure_espeak_ng_data(&data_dir).await;
        } else {
            println!("  ⏭  Piper: active_tts_model=piper but no voice model configured — configure one in Settings");
        }
    }

    // espeak-ng phoneme data directory. Used by both backends:
    // - Legacy subprocess: passed to piper as `--espeak_data <dir>`.
    // - In-process: set as the `PIPER_ESPEAKNG_DATA_DIRECTORY` env var that
    //   espeak-rs consults during its lazy init.
    let espeak_data = {
        let p = model_download::piper_espeak_data_path(&data_dir);
        if p.exists() {
            Some(p)
        } else {
            None
        }
    };

    // ── Construct the TTS backend ──
    //
    // Default: in-process `PiperRsOutput`. Loads the .onnx + .onnx.json once
    // and synthesises with zero subprocess overhead.
    //
    // Legacy: `PiperOutput` (subprocess) plus a `piper_http` HTTP wrapper on a
    // background port for backwards compatibility with the old `piper_http_port`
    // status report.
    #[allow(unused_mut)]
    let mut piper_http_port: Option<u16> = None;

    #[cfg(not(feature = "legacy-subprocess"))]
    let piper_tts: Option<Arc<dyn pond_core::models::ports::voice_output::VoiceOutput>> =
        match &piper_model {
            Some(model_path) if model_path.exists() => {
                let config_path =
                    std::path::PathBuf::from(format!("{}.json", model_path.display()));
                if !config_path.exists() {
                    println!(
                        "  ⚠  Piper voice config (.onnx.json) missing at {}",
                        config_path.display()
                    );
                    None
                } else {
                    let model_path_owned = model_path.clone();
                    let config_path_owned = config_path.clone();
                    let piper_result = tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        tokio::task::spawn_blocking(move || {
                            PiperRsOutput::new(model_path_owned, config_path_owned)
                        }),
                    )
                    .await;
                    match piper_result {
                        Ok(Ok(Ok(out))) => {
                            let out = match espeak_data.clone() {
                                Some(d) => out.with_espeak_data(d),
                                None => out,
                            };
                            println!("  ✅ Piper TTS: in-process (piper-rs / ort)");
                            Some(Arc::new(out)
                                as Arc<
                                    dyn pond_core::models::ports::voice_output::VoiceOutput,
                                >)
                        }
                        Ok(Ok(Err(e))) => {
                            tracing::warn!("PiperRsOutput failed to load voice: {e}");
                            None
                        }
                        Ok(Err(e)) => {
                            tracing::warn!("PiperRsOutput spawn_blocking panicked: {e}");
                            None
                        }
                        Err(_) => {
                            tracing::warn!(
                                "PiperRsOutput timed out after 15 s — ONNX Runtime may be \
                                 version-incompatible (need ORT 1.24.2)"
                            );
                            None
                        }
                    }
                }
            }
            _ => None,
        };

    #[cfg(feature = "legacy-subprocess")]
    let piper_tts: Option<Arc<dyn pond_core::models::ports::voice_output::VoiceOutput>> =
        match (piper_process::find_binary(&data_dir), &piper_model) {
            (Some(bin), Some(model_path)) if model_path.exists() => {
                let ed = espeak_data.clone();
                match piper_http::start(bin.clone(), model_path.clone(), ed).await {
                    Ok(port) => {
                        println!("  ✅ Piper TTS running on port {}", port);
                        piper_http_port = Some(port);
                        let mut out = PiperOutput::new(bin, model_path.clone());
                        if let Some(d) = espeak_data.clone() {
                            out = out.with_espeak_data(d);
                        }
                        Some(Arc::new(out))
                    }
                    Err(e) => {
                        tracing::warn!("piper-http failed to start: {e}");
                        let mut out = PiperOutput::new(bin, model_path.clone());
                        if let Some(d) = espeak_data.clone() {
                            out = out.with_espeak_data(d);
                        }
                        Some(Arc::new(out))
                    }
                }
            }
            _ => None,
        };
    let tts: Option<Arc<dyn pond_core::models::ports::voice_output::VoiceOutput>> = match piper_tts
    {
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
    let model_repo: Arc<dyn ModelRepository + Send + Sync> =
        Arc::new(SqliteModelRepository::new(db.system.clone()));
    let model_service = Arc::new(
        pond_core::models::services::model_service::ModelService::new(
            model_repo.clone(),
            Arc::new(
                crate::composite_model_catalog_provider::CompositeModelCatalogProvider::new(
                    reqwest::Client::builder()
                        .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
                        .build()
                        .unwrap_or_default(),
                ),
            ),
            Arc::new(crate::http_model_downloader::HttpModelDownloader::new()),
            Arc::new(crate::filesystem_model_storage::FilesystemModelStorage::new(&data_dir)),
        ),
    );

    // Seed catalog from upstream sources (idempotent, safe to call every startup)
    if let Err(e) = model_service.seed_catalog().await {
        tracing::warn!("Failed to seed model catalog: {e}. Starting with existing DB records.");
    }
    // Correct any stale downloaded flags (files added/removed outside of GIAP)
    if let Ok(n) = model_service.sync_disk_flags().await {
        if n > 0 {
            tracing::info!("sync_disk_flags: corrected {n} stale record(s)");
        }
    }
    sync_assignments_to_settings(&*model_repo, &settings_repo_early).await;

    // Autonomous background download: any model assigned to a role but missing from disk.
    // Runs as a detached task so the HTTP server is available immediately.
    {
        use crate::filesystem_model_storage::FilesystemModelStorage;
        use crate::reqwest_model_downloader::ReqwestModelDownloader;
        use crate::startup::auto_download_assigned_models;

        let dl_repo: Arc<
            dyn pond_core::models::ports::model_repository::ModelRepository + Send + Sync,
        > = model_repo.clone();
        let dl_storage: Arc<
            dyn pond_core::models::ports::model_storage::ModelStorage + Send + Sync,
        > = Arc::new(FilesystemModelStorage::new(&data_dir));
        let dl_downloader: Arc<
            dyn pond_core::models::ports::model_downloader::ModelDownloader + Send + Sync,
        > = Arc::new(ReqwestModelDownloader);

        tokio::spawn(async move {
            let n = auto_download_assigned_models(dl_repo, dl_storage, dl_downloader).await;
            if n > 0 {
                tracing::info!("auto_download: triggered {n} download(s) for role-assigned models");
            }
        });
    }

    // LLM — only start llamafile when at least one role is configured to use it.
    // ModelService handles downloading autonomously inside try_start.
    let any_role_needs_llamafile = settings.chat_provider == "llamafile";

    let active_llm_name: String = settings.chat_model.clone();
    let (initial_llamafile_guard, llamafile_port) = if any_role_needs_llamafile {
        match llamafile_process::try_start(&data_dir, model_service.clone(), Some(&active_llm_name))
            .await
        {
            Some((proc, port)) => (Some(proc), port),
            None => (None, ports::LLAMAFILE),
        }
    } else {
        println!(
            "  ⏭  LLM: llamafile skipped (provider = {})",
            settings.chat_provider
        );
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
    let session_storage: Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage> =
        Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let settings_repo: Arc<
        dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync,
    > = Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    let profile_repo: Arc<
        dyn pond_core::user_data::ports::profile::ProfileRepository + Send + Sync,
    > = Arc::new(SqliteProfileRepository::new(db.system.clone()));
    let device_registry: Arc<
        dyn pond_core::user_data::ports::device_registry::DeviceRegistry + Send + Sync,
    > = Arc::new(SqliteDeviceRegistry::new(db.system.clone()));
    let memory_repo: Arc<
        dyn pond_core::user_data::ports::memory_repository::MemoryRepository + Send + Sync,
    > = Arc::new(SqliteMemoryRepository::new(db.system.clone()));
    let draft_repo: Arc<dyn pond_core::user_data::ports::draft::DraftRepository + Send + Sync> =
        Arc::new(SqliteDraftRepository::new(db.system.clone()));
    let sensor_storage: Arc<
        dyn pond_core::user_data::ports::sensor_storage::SensorStorage + Send + Sync,
    > = Arc::new(SqliteSensorStorage::new(db.logs.clone()));
    let camera_storage: Arc<
        dyn pond_core::user_data::ports::camera_storage::CameraStorage + Send + Sync,
    > = Arc::new(SqliteCameraStorage::new(db.logs.clone()));

    // ── Face recognition (Phase 2) ──────────────────────────────────────────
    // Built only when the --features face-onnx build flag is enabled AND an
    // ONNX embedding model is present on disk.  Missing model file → None
    // (server starts normally; /api/v1/faces/* return 503).
    //
    // First call the auto-downloader so a fresh `cargo run` brings the
    // models down on its own, exactly the way whisper / piper do.  We
    // do this only when the face feature is compiled in, and we let
    // failures fall through — `build_face_recognition` will simply
    // return `None` when the files are absent.
    #[cfg(feature = "face-onnx")]
    {
        if let Err(e) = model_download::download_face_models(&data_dir).await {
            tracing::warn!("face model auto-download failed: {e:#}");
        }
        // Re-apply defaults: the antispoof file may have just appeared on
        // disk for the first time, in which case the earlier env-default
        // pass was a no-op.  Idempotent — only sets unset vars.
        apply_face_recognition_defaults();
    }
    let face_recognition: Option<
        Arc<dyn pond_core::user_data::ports::face_recognition::FaceRecognition>,
    > = build_face_recognition(&data_dir, db.system.clone());

    let prompt_template_repo: Arc<
        dyn pond_core::user_data::ports::prompt_template::PromptTemplateRepository + Send + Sync,
    > = Arc::new(SqlitePromptTemplateRepository::new(db.system.clone()));
    let prompt_extra_repo: Arc<
        dyn pond_core::user_data::ports::prompt_extra::PromptExtraRepository + Send + Sync,
    > = Arc::new(SqlitePromptExtraRepository::new(db.system.clone()));
    let skill_repo: Arc<dyn pond_core::user_data::ports::skill::UserSkillRepository + Send + Sync> =
        Arc::new(SqliteSkillRepository::new(db.system.clone()));
    let recipe_repo: Arc<
        dyn pond_core::user_data::ports::recipe::AgentRecipeRepository + Send + Sync,
    > = Arc::new(SqliteRecipeRepository::new(db.system.clone()));

    // Reseed built-in prompt templates with latest Jinja2 general-purpose content.
    {
        use pond_core::prompts::{PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM};
        use pond_core::user_data::domain::prompt_template::PromptTemplate;
        #[allow(unused_imports)]
        use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
        let built_ins = [
            (
                "balanced",
                PROMPT_BALANCED,
                "Warm, practical, general-purpose. Default.",
            ),
            (
                "concise",
                PROMPT_CONCISE,
                "Minimal, action-first. For power users.",
            ),
            (
                "technical",
                PROMPT_TECHNICAL,
                "Verbose, tool-aware, narrates reasoning. For developers.",
            ),
            (
                "warm",
                PROMPT_WARM,
                "Conversational, family-friendly, personality-forward.",
            ),
        ];
        for (name, content, description) in built_ins {
            let t = PromptTemplate {
                name: name.to_string(),
                content: content.to_string(),
                description: description.to_string(),
                is_system: true,
                updated_at: String::new(),
            };
            if let Err(e) = prompt_template_repo.upsert(&t).await {
                tracing::warn!("Failed to reseed built-in prompt template '{name}': {e}");
            }
        }
        tracing::info!("Built-in prompt templates reseeded (Jinja2 general-purpose copilot)");
    }

    let effective_chat_provider = settings.chat_provider.clone();
    let effective_chat_model = settings.chat_model.clone();

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
                    None => LocalInferenceLlmAdapter::new(model).await,
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
                            model,
                            e
                        );
                        Arc::new(
                            LlamafileProvider::new(Some(llamafile_url))
                                .with_max_tokens(max_tokens)
                                .with_temperature(temperature),
                        ) as Arc<dyn LlmProvider>
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
    let max_tokens = settings.llm_max_tokens;
    let temperature = settings.llm_temperature;

    let chat_provider_arc = build_provider(
        &effective_chat_provider,
        &effective_chat_model,
        &llamafile_url,
        data_dir_ref,
        max_tokens,
        temperature,
    )
    .await;

    let llm_provider = Arc::new(tokio::sync::RwLock::new(Some(
        chat_provider_arc.clone() as Arc<dyn LlmProvider>
    )));

    // Build InferencePool — concurrent LLM task submission with provider-aware
    // concurrency limits. HTTP providers (Ollama/llamafile) get 3 concurrent
    // slots; local GGUF gets 1 (serialized by Goose's model mutex anyway).
    let inference_pool: Option<Arc<dyn pond_core::models::ports::inference_pool::InferencePool>> = {
        use pond_core::models::ports::inference_pool::InferencePool as _;
        let pool = inference_pool::TokioInferencePool::for_provider(
            llm_provider.clone(),
            &effective_chat_provider,
        );
        println!(
            "  Inference Pool: concurrency={} (provider={})",
            pool.concurrency(),
            effective_chat_provider
        );
        Some(Arc::new(pool))
    };

    // Build AnswerReviewer for the HTTP path — adversarial post-inference quality gate.
    // Always constructed so the user can toggle it on/off at runtime via settings.
    // The routes.rs handler checks review_mode at request time, not at startup.
    println!(
        "  Answer Reviewer: ready (mode={}, threshold={}/5, max_rounds={})",
        settings.review_mode, settings.review_pass_threshold, settings.review_max_rounds
    );
    let answer_reviewer_for_http: Option<
        Arc<dyn pond_core::models::ports::answer_reviewer::AnswerReviewer>,
    > = Some(Arc::new(GiapAnswerReviewer {
        live_provider: llm_provider.clone(),
        pass_threshold: settings.review_pass_threshold,
        max_rounds: settings.review_max_rounds,
    })
        as Arc<
            dyn pond_core::models::ports::answer_reviewer::AnswerReviewer,
        >);

    // Memory extractor — background extraction of durable facts from conversations.
    let (memory_extractor_for_http, memory_extraction_service_for_http) =
        if settings.memory_extraction_enabled {
            let extractor: Arc<dyn pond_core::user_data::ports::memory_extractor::MemoryExtractor> =
                Arc::new(llm_memory_extractor::LlmMemoryExtractor::new(
                    llm_provider.clone(),
                    settings.memory_extraction_max_facts,
                ));
            let service = Arc::new(
                pond_core::user_data::services::memory_extraction::MemoryExtractionService::new(
                    settings.memory_extraction_interval_secs,
                ),
            );
            tracing::info!(
                "memory extraction enabled — facts will be auto-extracted from conversations"
            );
            (Some(extractor), Some(service))
        } else {
            (None, None)
        };

    let db = Arc::new(db);

    // Spawn background TTL pruning task (runs every 6 hours)
    {
        let logs = db.logs.clone();
        let system = db.system.clone();
        tokio::spawn(async move {
            pond_infra::pruning::run_pruning(logs, system, Default::default()).await;
        });
    }

    // Spawn background memory decay/cleanup task
    if settings.memory_cleanup_enabled {
        let cleanup_repo = memory_repo.clone();
        let cleanup_interval_secs = settings.memory_cleanup_interval_hours as u64 * 3600;
        let prune_threshold = settings.memory_prune_threshold;
        let archive_threshold = settings.memory_archive_threshold;
        let base_half_life = settings.memory_decay_base_half_life_days;
        let decay_beta = settings.memory_decay_beta;
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(cleanup_interval_secs));
            loop {
                interval.tick().await;
                match pond_core::user_data::services::memory_cleanup::run_cleanup(
                    cleanup_repo.as_ref(),
                    prune_threshold,
                    archive_threshold,
                    base_half_life,
                    decay_beta,
                )
                .await
                {
                    Ok((scanned, archived, pruned)) => {
                        if archived > 0 || pruned > 0 {
                            tracing::info!(
                                "memory cleanup: scanned={scanned}, archived={archived}, pruned={pruned}"
                            );
                        }
                    }
                    Err(e) => tracing::warn!("memory cleanup failed: {e}"),
                }
            }
        });
        tracing::info!("memory cleanup enabled — runs every 6 hours");
    }

    // ── Adversarial memory consolidation (inactivity-based) ──────────────
    //
    // Three shared pieces of state:
    //   - last_user_activity: reset on every chat request
    //   - consolidation_cancel: abort mid-run when the user comes back
    //   - consolidation_event_tx: broadcast channel for SSE + background logs
    let last_user_activity = Arc::new(tokio::sync::RwLock::new(std::time::Instant::now()));
    let consolidation_cancel: Arc<
        tokio::sync::RwLock<Option<tokio_util::sync::CancellationToken>>,
    > = Arc::new(tokio::sync::RwLock::new(None));
    let (consolidation_event_tx, _) = tokio::sync::broadcast::channel::<
        pond_core::user_data::ports::memory_consolidator::ConsolidationEvent,
    >(64);

    // Build the ConsolidationRunner closure that pond-api will call from the
    // POST /api/v1/memory/consolidate endpoint. Captures repo, provider, and
    // the broadcast channel so pond-api never imports the consolidator crate.
    let consolidation_runner: Option<pond_api::ConsolidationRunner> =
        if settings.memory_consolidation_enabled {
            let cr_repo = memory_repo.clone();
            let cr_provider = llm_provider.clone();
            let cr_broadcast_tx = consolidation_event_tx.clone();
            Some(Arc::new(
                move |cancel: tokio_util::sync::CancellationToken| {
                    let repo = cr_repo.clone();
                    let provider = cr_provider.clone();
                    let broadcast_tx = cr_broadcast_tx.clone();
                    Box::pin(async move {
                        run_consolidation_pipeline(repo, provider, broadcast_tx, cancel).await;
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                },
            ))
        } else {
            None
        };

    // Spawn inactivity-based consolidation scheduler
    if settings.memory_consolidation_enabled {
        let inact_repo = memory_repo.clone();
        let inact_provider = llm_provider.clone();
        let inact_activity = last_user_activity.clone();
        let inact_cancel = consolidation_cancel.clone();
        let inact_event_tx = consolidation_event_tx.clone();
        let inactivity_secs = 15 * 60u64; // 15 minutes

        tokio::spawn(async move {
            loop {
                // Poll every 60 seconds
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;

                // Check if 15 minutes of inactivity have passed
                let elapsed = inact_activity.read().await.elapsed();
                if elapsed < std::time::Duration::from_secs(inactivity_secs) {
                    continue;
                }

                tracing::info!("15 minutes of inactivity — starting memory consolidation");

                let cancel = tokio_util::sync::CancellationToken::new();
                *inact_cancel.write().await = Some(cancel.clone());

                run_consolidation_pipeline(
                    inact_repo.clone(),
                    inact_provider.clone(),
                    inact_event_tx.clone(),
                    cancel,
                )
                .await;

                // Reset the activity timer so we don't immediately re-run
                *inact_activity.write().await = std::time::Instant::now();
            }
        });
        tracing::info!("memory consolidation enabled — triggers after 15 min inactivity");
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

    // Weather — used by the MCP weather module, not AppState.
    // The LLM calls giap__get_current_weather when it needs weather data.
    let weather: Option<Arc<dyn WeatherProvider>> = {
        if settings.weather_enabled
            && (settings.weather_latitude != 0.0 || settings.weather_longitude != 0.0)
        {
            let loc = if settings.weather_location_name.is_empty() {
                format!(
                    "{:.3}, {:.3}",
                    settings.weather_latitude, settings.weather_longitude
                )
            } else {
                settings.weather_location_name.clone()
            };
            tracing::info!(
                "weather enabled: {} ({}, {})",
                loc,
                settings.weather_latitude,
                settings.weather_longitude
            );
            Some(Arc::new(OpenMeteoWeatherAdapter::new(
                settings.weather_latitude,
                settings.weather_longitude,
                loc,
            )))
        } else {
            tracing::info!(
                "weather disabled — enable via PUT /api/v1/settings (weather_enabled + lat/lon)"
            );
            None
        }
    };

    // Scheduler — persist task list next to the databases.
    // Uses a DeferredExecutor so the scheduler can be created before the agent
    // exists.  The real executor (AgentScheduleExecutor) is injected after the
    // agent is constructed further below.
    let deferred_executor = Arc::new(DeferredExecutor::new());
    let (schedule_result_tx, _) = tokio::sync::broadcast::channel::<
        pond_core::user_data::domain::schedule::ScheduleResultEvent,
    >(32);
    let scheduler: Option<Arc<dyn pond_core::user_data::ports::scheduler::SchedulerPort>> = {
        let exec: Arc<dyn pond_core::user_data::ports::schedule_execution::ScheduleExecutor> =
            deferred_executor.clone();
        match CronSchedulerAdapter::with_options(
            data_dir.join("schedules.json"),
            data_dir.join("schedule_runs.json"),
            exec,
            Some(schedule_result_tx.clone()),
            settings.schedule_max_runs_per_task,
        )
        .await
        {
            Ok(s) => {
                tracing::info!(
                    "scheduler ready ({})",
                    data_dir.join("schedules.json").display()
                );
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
    let mcp_memory: Option<
        Arc<dyn pond_core::mcp::ports::mcp_knowledge::McpKnowledgePort + Send + Sync>,
    > = {
        use pond_adapters_mcp_memory::GooseMcpMemoryAdapter;
        let adapter = GooseMcpMemoryAdapter::new(data_dir.join("memory"));
        tracing::info!("MCP memory enabled ({})", data_dir.join("memory").display());
        Some(Arc::new(adapter))
    };
    #[cfg(not(feature = "mcp-memory"))]
    let mcp_memory: Option<
        Arc<dyn pond_core::mcp::ports::mcp_knowledge::McpKnowledgePort + Send + Sync>,
    > = None;

    // ── Embedding provider (fastembed / ONNX) ────────────────────────────────
    // Initialized before the agent backend so it can be wired into the memory
    // MCP server for semantic search on recall/save.
    let embedding_provider: Option<
        Arc<dyn pond_core::models::ports::embedding::EmbeddingProvider + Send + Sync>,
    > = {
        use pond_core::models::ports::embedding::EmbeddingProvider as _;
        if settings.embedding_provider == "none" {
            tracing::info!("embedding provider: disabled (embedding_provider = \"none\")");
            None
        } else {
            let emb_model = if settings.active_embedding_model.is_empty() {
                "all-MiniLM-L6-v2"
            } else {
                &settings.active_embedding_model
            };
            let cache_dir = data_dir.join("models").join("embedding");
            let emb_model_owned = emb_model.to_string();
            let emb_result = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                tokio::task::spawn_blocking(move || {
                    pond_infra::fastembed_embedding::FastembedEmbeddingProvider::new(
                        &emb_model_owned,
                        Some(cache_dir),
                    )
                }),
            )
            .await;
            let init_result = match emb_result {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => Err(anyhow::anyhow!("embedding spawn_blocking failed: {e}")),
                Err(_) => Err(anyhow::anyhow!(
                    "embedding provider init timed out after 30 s — ONNX Runtime may be \
                     version-incompatible (need ORT 1.24.2)"
                )),
            };
            match init_result {
                Ok(provider) => {
                    tracing::info!(
                        model = provider.model_name(),
                        dims = provider.dimensions(),
                        "embedding provider ready"
                    );
                    Some(Arc::new(provider)
                        as Arc<
                            dyn pond_core::models::ports::embedding::EmbeddingProvider
                                + Send
                                + Sync,
                        >)
                }
                Err(e) => {
                    tracing::warn!("embedding provider failed to init: {e:#}");
                    None
                }
            }
        }
    };

    // ── Agent backend ────────────────────────────────────────────────────────────
    // pond_agent_active is always false while the backend is quarantined (Q2-05).
    // agent_backend has already been normalised to "goose" above.
    #[cfg(feature = "pond-agent")]
    let pond_agent_active = agent_backend == "pond"; // stays false: quarantine override above
    #[cfg(not(feature = "pond-agent"))]
    let pond_agent_active = false;

    #[cfg(feature = "goose-agent")]
    // Device actuation backend — logging stub until MQTT/HTTP/IR or HA-MCP land.
    let device_control: Arc<
        dyn pond_core::user_data::ports::device_control::DeviceControlPort,
    > = Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new());

    let (agent, extension_manager, _tool_caller, tool_registry) = if pond_agent_active {
        // Build PondAgent directly — no Goose, no llama backend conflict.
        let default_registry: Arc<
            dyn pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort,
        > = Arc::new(pond_core::mcp::services::tool_registry::InMemoryToolRegistry::new());

        #[cfg(feature = "pond-agent")]
        let agent: Arc<dyn Agent> = {
            let settings = settings_repo.get().await.unwrap_or_default();
            if let Ok(eng) = pond_inference::LlamaCppEngine::new(&data_dir) {
                let model_id = &settings.chat_model;
                if !model_id.is_empty() {
                    if let Err(e) = eng.load_model(model_id, 99, true).await {
                        tracing::error!("Failed to load model '{}': {e}", model_id);
                    }
                }
                let dispatcher = pond_mcp_server::McpToolDispatcher::new(
                    memory_repo.clone(),
                    weather.clone(),
                    scheduler.clone(),
                    settings_repo.clone(),
                    device_registry.clone(),
                    skill_repo.clone(),
                    recipe_repo.clone(),
                    draft_repo.clone(),
                    embedding_provider.clone(),
                    device_control.clone(),
                );
                let disp: Arc<dyn pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher> =
                    Arc::new(dispatcher);
                let tool_defs: Vec<pond_core::models::ports::inference::ToolDefinition> = disp
                    .available_tool_definitions()
                    .await
                    .into_iter()
                    .map(|(name, desc, schema)| {
                        pond_core::models::ports::inference::ToolDefinition {
                            name,
                            description: desc,
                            parameters_schema: schema,
                        }
                    })
                    .collect();
                let agent = pond_agent::PondAgent::new(
                    Arc::new(eng),
                    tool_defs,
                    settings_repo.clone(),
                    Some(prompt_template_repo.clone()),
                    Some(prompt_extra_repo.clone()),
                    Some(skill_repo.clone()),
                    device_registry.clone(),
                    session_storage.clone(),
                    Some(disp),
                );
                tracing::info!("PondAgent ready — independent inference with KV-cache reuse");
                Arc::new(agent) as Arc<dyn Agent>
            } else {
                Arc::new(MockAgent::new()) as Arc<dyn Agent>
            }
        };
        #[cfg(not(feature = "pond-agent"))]
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new());

        (agent, None, None, default_registry)
    } else {
        build_goose_backend(
            agent_backend,
            &llamafile_url,
            &data_dir,
            weather.clone(),
            device_registry.clone(),
            scheduler.clone(),
            settings_repo.clone(),
            memory_repo.clone(),
            embedding_provider.clone(),
            skill_repo.clone(),
            recipe_repo.clone(),
            prompt_template_repo.clone(),
            prompt_extra_repo.clone(),
            draft_repo.clone(),
            device_control.clone(),
            Some(session_storage.clone()),
            false, // voice_mode — server mode, not voice
        )
        .await
    };

    #[cfg(not(feature = "goose-agent"))]
    let tool_caller: Option<Arc<dyn pond_core::mcp::ports::tools::tool_caller::ToolCaller>> = None;
    #[cfg(not(feature = "goose-agent"))]
    let tool_registry: Arc<dyn pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort> =
        Arc::new(pond_core::mcp::services::tool_registry::InMemoryToolRegistry::new());
    #[cfg(not(feature = "goose-agent"))]
    let (agent, extension_manager): (
        Arc<dyn Agent>,
        Option<Arc<dyn pond_core::mcp::ports::extension_manager::ExtensionManagerPort>>,
    ) = {
        if agent_backend == "goose" {
            tracing::warn!(
                "Goose agent backend requested but this binary was compiled without the `goose-agent` feature. \
                 Rebuild with: cargo run -p pond-server -- serve  (goose-agent is a default feature). \
                 Falling back to mock agent."
            );
        }
        (Arc::new(MockAgent::new()), None)
    };

    // ── Fill the deferred schedule executor now that the agent exists ─────────
    {
        let real_executor = Arc::new(AgentScheduleExecutor::new(
            agent.clone(),
            session_storage.clone(),
            settings.schedule_max_concurrent,
        ));
        deferred_executor
            .init(
                real_executor
                    as Arc<dyn pond_core::user_data::ports::schedule_execution::ScheduleExecutor>,
            )
            .await;
        tracing::info!("schedule executor initialized — agent-prompt schedules are now active");
    }

    // Secret storage — file-based at $DATA_DIR/secrets.json (0o600 permissions).
    // Initialized before MCP auto-connect so startup can resolve OAuth tokens.
    let secret_repo: Option<
        Arc<dyn pond_core::security::ports::secret::SecretRepository + Send + Sync>,
    > = {
        match pond_infra::keyring_secret_repository::FileSecretRepository::new(&data_dir) {
            Ok(repo) => {
                tracing::info!(
                    "secret repository initialized at {}/secrets.json",
                    data_dir.display()
                );
                Some(Arc::new(repo))
            }
            Err(e) => {
                tracing::warn!("failed to initialize secret repository: {e}");
                None
            }
        }
    };

    // Marketplace — curated registry of installable extensions.
    // Initialized before MCP auto-connect so startup can look up required_secrets.
    let marketplace: Arc<dyn pond_core::mcp::ports::extension_marketplace::ExtensionMarketplace> =
        Arc::new(pond_core::mcp::services::marketplace::BundledMarketplace::new());

    // MCP client — load persisted server configs and auto-connect enabled ones.
    let mcp_server_repo: Option<Arc<dyn pond_core::mcp::ports::mcp_server::McpServerRepository>> = {
        let repo = Arc::new(SqliteMcpServerRepository::new(db.system.clone()));
        // Auto-connect saved external MCP servers if the extension manager is available.
        if let Some(mgr) = &extension_manager {
            match repo.list().await {
                Ok(servers) => {
                    for srv in servers
                        .into_iter()
                        .filter(|s: &pond_core::mcp::ports::mcp_server::McpServerConfig| s.enabled)
                    {
                        use pond_core::mcp::ports::extension_manager::AddExtensionRequest;

                        // Start with persisted env, then resolve any secrets
                        // from the secret repo that aren't already present.
                        // This ensures OAuth tokens (stored in secrets.json,
                        // not in mcp_servers.env) are injected at startup.
                        let mut env = srv.env.clone();
                        if let Some(sr) = &secret_repo {
                            if let Ok(Some(ext)) = marketplace.get_by_id(&srv.name).await {
                                for secret_req in &ext.required_secrets {
                                    if !env.contains_key(&secret_req.key) {
                                        if let Ok(Some(val)) = sr.get(&secret_req.key).await {
                                            env.insert(secret_req.key.clone(), val);
                                        }
                                    }
                                }
                            }
                        }

                        let req = AddExtensionRequest {
                            name: srv.name.clone(),
                            kind: srv.kind.clone(),
                            description: srv.description.clone(),
                            command: srv.command.clone(),
                            args: srv.args.clone(),
                            env,
                            uri: srv.uri.clone(),
                        };
                        match mgr.add_extension(req).await {
                            Ok(_) => tracing::info!("auto-connected MCP server '{}'", srv.name),
                            Err(e) => tracing::warn!(
                                "failed to auto-connect MCP server '{}': {e}",
                                srv.name
                            ),
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
    let model_scheduler: Option<
        Arc<dyn pond_core::models::ports::model_scheduler::ModelScheduler>,
    > = {
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
        Some(sched_arc as Arc<dyn pond_core::models::ports::model_scheduler::ModelScheduler>)
    };
    #[cfg(not(feature = "local-inference"))]
    let model_scheduler: Option<
        Arc<dyn pond_core::models::ports::model_scheduler::ModelScheduler>,
    > = None;

    // Capture logs pool before `db` is moved into AppState
    let event_log_repo: Option<Arc<dyn pond_core::security::ports::event_log::EventLogRepository>> =
        Some(Arc::new(SqliteEventLogRepository::new(db.logs.clone())));

    // Privacy/security boundary hook — wraps the event log as its audit sink.
    // Default-allow; routes opt in to calling `allow`/`audit`.
    let security_policy: Option<Arc<dyn pond_core::security::ports::policy::SecurityPolicy>> =
        Some(Arc::new(
            pond_infra::sqlite_security_policy::SqliteSecurityPolicy::new(Arc::new(
                SqliteEventLogRepository::new(db.logs.clone()),
            )),
        ));

    // Start routing WARN+ tracing events into the SQLite event log.
    // _file_guard must live until run_server returns so the background file
    // writer keeps flushing log output to disk.
    let _file_guard = drain_handle.drain_into(event_log_repo.clone());

    // Durable per-turn telemetry persisted to pond_logs.db. Falls back to the
    // in-memory store if the SQLite-backed adapter cannot be initialised.
    let telemetry: Option<Arc<dyn pond_core::security::ports::telemetry::TelemetryPort>> =
        match SqliteTelemetry::new(db.logs.clone()).await {
            Ok(t) => Some(Arc::new(t)),
            Err(e) => {
                tracing::warn!(
                    "Failed to initialize SQLite telemetry, falling back to in-memory: {e}"
                );
                Some(Arc::new(
                    pond_core::security::services::telemetry::InMemoryTelemetry::new(),
                ))
            }
        };

    // Bind the API port early so we can thread it into AppState (needed for
    // dynamic OAuth redirect URIs).  The actual `axum::serve()` call that
    // consumes the listener happens further below.
    let (listener, api_port) =
        ports::bind_with_fallback("0.0.0.0", port.unwrap_or(ports::API_SERVER)).await?;

    // Direct MCP tool dispatcher for POST /api/v1/tools/invoke (bypasses the LLM).
    let tool_dispatcher: Option<
        Arc<dyn pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher>,
    > = Some(Arc::new(pond_mcp_server::McpToolDispatcher::new(
        memory_repo.clone(),
        weather.clone(),
        scheduler.clone(),
        settings_repo.clone(),
        device_registry.clone(),
        skill_repo.clone(),
        recipe_repo.clone(),
        draft_repo.clone(),
        embedding_provider.clone(),
        device_control.clone(),
    )));

    // Event bus + durable event log (#91/#109). The bus is shared with AppState
    // for publishing on ingest; a background bridge subscribes to it and appends
    // every bus event (sensor/camera/device) into the unified `events` table, so
    // events written in normal operation are queryable from pond_logs.db.
    let event_bus: Arc<dyn pond_core::shared::ports::event_bus::EventBus> =
        Arc::new(InProcessEventBus::new());
    // One shared event store: the bus→log bridge writes to it, and the activity
    // query API (#114) reads from it via AppState.
    let event_log: Arc<dyn pond_core::security::ports::event_log::EventLog> =
        Arc::new(SqliteEventLog::new(db.logs.clone()));
    {
        let event_log = event_log.clone();
        let mut events = event_bus.subscribe();
        tokio::spawn(async move {
            use futures::StreamExt;
            use pond_core::security::ports::event_log::EventLog as _;
            while let Some(bus_event) = events.next().await {
                if let Err(e) = event_log.append(bus_event.to_event()).await {
                    tracing::warn!(error = %e, "failed to persist bus event to event log");
                }
            }
        });
    }

    // DB-backed handshake/pairing (#93). Construct before `db` is moved into
    // AppState, then issue a fresh pairing code the operator reads off the CLI
    // to pair a GOTG device.
    let handshake: Arc<dyn pond_core::security::ports::handshake::Handshake> =
        Arc::new(SqliteHandshakeAdapter::new(db.system.clone()));
    match handshake.issue_pairing_code().await {
        Ok(pc) => {
            println!("\n  ┌───────────────────────────────────────┐");
            println!("  │  Pairing code:  {}   (valid 10 min) │", pc.code);
            println!("  └───────────────────────────────────────┘");
            println!("  Enter this in Goose On The Go to pair this device.\n");
        }
        Err(e) => tracing::warn!("failed to issue pairing code at startup: {e:#}"),
    }

    let state = Arc::new(AppState {
        db,
        onboarding_repo,
        handshake: handshake.clone(),
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
        embedding_provider,
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
        tool_registry: Some(tool_registry),
        tool_dispatcher,
        marketplace: Some(marketplace),
        secret_repo,
        download_tracker: std::sync::Arc::new(tokio::sync::RwLock::new(
            std::collections::HashMap::new(),
        )),
        piper_http_port,
        model_catalog_provider: Some(Arc::new(
            crate::composite_model_catalog_provider::CompositeModelCatalogProvider::new(
                reqwest::Client::builder()
                    .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .unwrap_or_default(),
            ),
        )),
        model_storage_dir: Some(data_dir.clone()),
        prompt_template_repo: Some(prompt_template_repo),
        prompt_extra_repo: Some(prompt_extra_repo),
        skill_repo: Some(skill_repo.clone()),
        recipe_repo: Some(recipe_repo.clone()),
        llamafile_manager: Some(llamafile_manager),
        event_log_repo: event_log_repo,
        event_bus: Some(event_bus.clone()),
        event_log: Some(event_log.clone()),
        face_recognition,
        session_user_bindings: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: answer_reviewer_for_http,
        memory_extractor: memory_extractor_for_http,
        memory_extraction_service: memory_extraction_service_for_http,
        last_user_activity: last_user_activity.clone(),
        consolidation_cancel: consolidation_cancel.clone(),
        consolidation_event_tx: consolidation_event_tx.clone(),
        consolidation_runner,
        inference_pool,
        schedule_result_tx: schedule_result_tx.clone(),
        telemetry,
        context_monitor: Arc::new(
            pond_core::models::services::context_monitor::ContextMonitor::new(),
        ),
        mcp_app_resources: pond_mcp_server::all_app_resources()
            .into_iter()
            .map(|(uri, html)| (uri.to_string(), html))
            .collect(),
        oauth_state: pond_api::oauth_callback::new_oauth_state(),
        security_policy,
        api_port,
    });

    // Spawn OAuth token auto-refresh worker.
    // Proactively refreshes tokens every 45 minutes so extensions don't hit
    // 401 errors mid-conversation. This worker only updates the secret store;
    // it does NOT restart extensions (avoids disrupting active tool calls).
    // Extensions get restarted on the next 401 retry or server restart.
    {
        let refresh_secret_repo = state.secret_repo.clone();
        let refresh_http_client = reqwest::Client::new();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(45 * 60));
            interval.tick().await; // skip the initial immediate tick
            loop {
                interval.tick().await;
                let providers =
                    pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
                for provider in &providers {
                    let Some(repo) = &refresh_secret_repo else {
                        continue;
                    };

                    // Only refresh if we have a refresh token stored
                    let has_refresh = repo.has(&provider.refresh_key).await.unwrap_or(false);
                    if !has_refresh {
                        continue;
                    }

                    let refresh_token = match repo.get(&provider.refresh_key).await {
                        Ok(Some(t)) => t,
                        _ => continue,
                    };

                    let client_id = repo
                        .get(&format!("{}_CLIENT_ID", provider.id.to_uppercase()))
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| provider.bundled_client_id.clone());

                    match refresh_http_client
                        .post(&provider.token_url)
                        .form(&[
                            ("grant_type", "refresh_token"),
                            ("refresh_token", refresh_token.as_str()),
                            ("client_id", client_id.as_str()),
                        ])
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => {
                            if let Ok(body) = resp.json::<serde_json::Value>().await {
                                if let Some(at) = body["access_token"].as_str() {
                                    let _ = repo.set(&provider.token_key, at).await;
                                }
                                if let Some(rt) = body["refresh_token"].as_str() {
                                    let _ = repo.set(&provider.refresh_key, rt).await;
                                }
                                tracing::info!(
                                    provider = %provider.id,
                                    "auto-refreshed OAuth token"
                                );
                            }
                        }
                        Ok(resp) => {
                            tracing::warn!(
                                provider = %provider.id,
                                status = %resp.status(),
                                "OAuth auto-refresh failed"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                provider = %provider.id,
                                error = %e,
                                "OAuth auto-refresh error"
                            );
                        }
                    }
                }
            }
        });
        tracing::info!("OAuth auto-refresh worker started — runs every 45 minutes");
    }

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
    let hostname = hostname
        .strip_suffix(".local")
        .unwrap_or(&hostname)
        .to_string();

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

async fn run_chat(
    provider: Option<&str>,
    model: Option<&str>,
    input: &str,
    wake_word: Option<&str>,
    no_wake_word: bool,
    tts: Option<&str>,
    tts_model: Option<std::path::PathBuf>,
) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!(
        "  ║   🦆  Goose-in-a-Pond  v{}       ║",
        env!("CARGO_PKG_VERSION")
    );
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
    println!(
        "  Provider: {} (model: {})",
        effective_provider, effective_model
    );

    // Resolve the whisper ggml model path (used by both the in-process backend
    // and the legacy HTTP subprocess). When voice input is not requested we
    // still resolve the path to surface a clear download-needed message.
    let whisper_model_path: Option<std::path::PathBuf> = if input == "whisper" {
        let whisper_model_name = settings.active_whisper_model.as_str();
        let (whisper_filename, whisper_url, whisper_mb) =
            SqliteModelRepository::new(db.system.clone())
                .get_by_id(&format!("whisper/{}", whisper_model_name))
                .await
                .ok()
                .flatten()
                .map(|r| {
                    (
                        r.filename
                            .unwrap_or_else(|| format!("ggml-{}.en.bin", whisper_model_name)),
                        r.url.unwrap_or_default(),
                        r.size_mb,
                    )
                })
                .unwrap_or_else(|| {
                    (
                        format!("ggml-{}.en.bin", whisper_model_name),
                        String::new(),
                        0u64,
                    )
                });
        let whisper_model = data_dir.join("models").join(&whisper_filename);
        if !whisper_model.exists() {
            println!(
                "  📥 STT model not found — downloading ({})...",
                whisper_model_name
            );
            if !whisper_url.is_empty() {
                if let Some(parent) = whisper_model.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                match model_download::download_file(&whisper_url, &whisper_model, whisper_mb).await
                {
                    Ok(_) => {}
                    Err(e) => println!("  ⚠  STT model download failed: {}", e),
                }
            } else {
                println!(
                    "  ⚠  STT model '{}' not in catalog — cannot download",
                    whisper_model_name
                );
            }
        }
        Some(whisper_model)
    } else {
        None
    };

    // Optionally start the legacy whisper.cpp subprocess. Default build skips
    // this entirely — `WhisperRsInput` handles inference in-process.
    #[allow(unused_mut, unused_assignments)]
    let mut whisper_port = ports::WHISPER;
    #[cfg(feature = "legacy-subprocess")]
    let _whisper_guard = if let Some(ref whisper_model) = whisper_model_path {
        let (guard, port) = whisper_process::try_start(&data_dir, whisper_model).await;
        whisper_port = port;
        guard
    } else {
        None
    };
    #[cfg(not(feature = "legacy-subprocess"))]
    let _whisper_guard: Option<()> = None;

    // The URL is only meaningful when the legacy subprocess is running; the
    // in-process backend ignores it. Kept here to populate AppState.whisper_url
    // for the (legacy) audio-transcribe / calibrate HTTP routes.
    let whisper_url = format!("http://127.0.0.1:{}", whisper_port);

    // ── Model catalog & ModelService (for autonomous downloading) ──────────────
    let chat_model_repo: Arc<dyn ModelRepository + Send + Sync> =
        Arc::new(SqliteModelRepository::new(db.system.clone()));
    let chat_model_service = Arc::new(
        pond_core::models::services::model_service::ModelService::new(
            chat_model_repo.clone(),
            Arc::new(
                crate::composite_model_catalog_provider::CompositeModelCatalogProvider::new(
                    reqwest::Client::builder()
                        .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
                        .build()
                        .unwrap_or_default(),
                ),
            ),
            Arc::new(crate::http_model_downloader::HttpModelDownloader::new()),
            Arc::new(crate::filesystem_model_storage::FilesystemModelStorage::new(&data_dir)),
        ),
    );

    // Seed catalog so model records exist for resolution
    if let Err(e) = chat_model_service.seed_catalog().await {
        tracing::warn!("Failed to seed model catalog: {e}");
    }
    if let Ok(n) = chat_model_service.sync_disk_flags().await {
        if n > 0 {
            tracing::info!("sync_disk_flags: corrected {n} stale record(s)");
        }
    }

    // Auto-start llamafile only when the provider is explicitly "llamafile".
    // Other providers (ollama, local, gguf, openai, etc.) manage their own process or need no process.
    let mut llamafile_port = ports::LLAMAFILE;
    let _llamafile_guard = if effective_provider == "llamafile" {
        match llamafile_process::try_start(&data_dir, chat_model_service, Some(effective_model))
            .await
        {
            Some((proc, port)) => {
                llamafile_port = port;
                Some(proc)
            }
            None => None,
        }
    } else {
        None
    };
    let llamafile_url = llamafile_process::url_for(llamafile_port);

    let session_id = "default-session".to_string();

    // ── Build repos for GooseAdapter (before db.system is consumed) ───────────────
    let settings_repo_arc: Arc<
        dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync,
    > = Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    let memory_repo: Arc<
        dyn pond_core::user_data::ports::memory_repository::MemoryRepository + Send + Sync,
    > = Arc::new(SqliteMemoryRepository::new(db.system.clone()));
    let skill_repo: Arc<dyn pond_core::user_data::ports::skill::UserSkillRepository + Send + Sync> =
        Arc::new(SqliteSkillRepository::new(db.system.clone()));
    let recipe_repo: Arc<
        dyn pond_core::user_data::ports::recipe::AgentRecipeRepository + Send + Sync,
    > = Arc::new(SqliteRecipeRepository::new(db.system.clone()));
    let template_repo: Arc<
        dyn pond_core::user_data::ports::prompt_template::PromptTemplateRepository + Send + Sync,
    > = Arc::new(SqlitePromptTemplateRepository::new(db.system.clone()));
    let extras_repo: Arc<
        dyn pond_core::user_data::ports::prompt_extra::PromptExtraRepository + Send + Sync,
    > = Arc::new(SqlitePromptExtraRepository::new(db.system.clone()));
    let device_registry_arc: Arc<
        dyn pond_core::user_data::ports::device_registry::DeviceRegistry + Send + Sync,
    > = Arc::new(SqliteDeviceRegistry::new(db.system.clone()));
    let draft_repo: Arc<dyn pond_core::user_data::ports::draft::DraftRepository + Send + Sync> =
        Arc::new(SqliteDraftRepository::new(db.system.clone()));

    // Reseed built-in prompt templates at startup with the latest Jinja2 general-purpose content.
    // Uses upsert (not insert_if_absent) so existing installs get the updated templates.
    // User-created templates (is_system = false) are never touched.
    {
        use pond_core::prompts::{PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM};
        use pond_core::user_data::domain::prompt_template::PromptTemplate;
        #[allow(unused_imports)]
        use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
        let built_ins = [
            (
                "balanced",
                PROMPT_BALANCED,
                "Warm, practical, general-purpose. Default.",
            ),
            (
                "concise",
                PROMPT_CONCISE,
                "Minimal, action-first. For power users.",
            ),
            (
                "technical",
                PROMPT_TECHNICAL,
                "Verbose, tool-aware, narrates reasoning. For developers.",
            ),
            (
                "warm",
                PROMPT_WARM,
                "Conversational, family-friendly, personality-forward.",
            ),
        ];
        for (name, content, description) in built_ins {
            let t = PromptTemplate {
                name: name.to_string(),
                content: content.to_string(),
                description: description.to_string(),
                is_system: true,
                updated_at: String::new(),
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
            format!(
                "{:.3}, {:.3}",
                settings.weather_latitude, settings.weather_longitude
            )
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
            s.chat_model = effective_model.to_string();
            settings_repo_arc.update(&s).await.ok();
        }
        let (a, _ext_mgr, _tc, _tr) = build_goose_backend(
            "goose",
            &llamafile_url,
            &data_dir,
            weather,
            device_registry_arc,
            None, // scheduler not used in voice mode
            settings_repo_arc,
            memory_repo,
            None, // embedding_provider — not used in voice/chat CLI mode
            skill_repo,
            recipe_repo,
            template_repo,
            extras_repo,
            draft_repo,
            Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
            None,               // session_storage — not needed for goose backend
            input == "whisper", // voice_mode
        )
        .await;
        a
    };

    #[cfg(not(feature = "goose-agent"))]
    let agent: Arc<dyn Agent> = Arc::new(MockAgent::new());

    // Resolve the system prompt using the already-loaded settings:
    // 1. File at $DATA_DIR/prompts/system.md (deployment override, rendered with all vars)
    // 2. build_system_prompt(&settings) — honours custom_system_prompt + prompt_style + addendum
    let system_prompt = {
        let prompt_dir = data_dir.join("prompts");
        let file_template = std::fs::read_to_string(prompt_dir.join("system.md")).ok();
        match file_template {
            Some(tmpl) => {
                println!(
                    "  Prompt:   custom ({})",
                    prompt_dir.join("system.md").display()
                );
                let name = pond_core::prompts::sanitize_field(&settings.assistant_name, 50);
                let user = pond_core::prompts::sanitize_field(&settings.user_name, 50);
                let persona =
                    pond_core::prompts::sanitize_field(&settings.assistant_personality, 200);
                let tz = pond_core::prompts::sanitize_field(&settings.timezone, 50);
                let location = if settings.weather_location_name.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\nLocation: {}.",
                        pond_core::prompts::sanitize_field(&settings.weather_location_name, 100)
                    )
                };
                let addendum = pond_core::prompts::sanitize_field(&settings.prompt_addendum, 500);
                pond_core::prompts::render_template(
                    &tmpl,
                    &[
                        ("assistant_name", name.as_str()),
                        ("user_name", user.as_str()),
                        ("personality", persona.as_str()),
                        ("timezone", tz.as_str()),
                        ("location", location.as_str()),
                        ("prompt_addendum", addendum.as_str()),
                    ],
                )
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
    let storage: Arc<dyn SessionStorage> = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    // Create session if it doesn't exist; ignore duplicate-key errors from prior runs
    if let Err(e) = storage.create_session(session_id.clone()).await {
        match e {
            pond_core::user_data::ports::session_storage::SessionStorageError::StorageError(_) => {
                // Likely a duplicate key — session already exists, which is fine
                tracing::debug!("Session already exists, reusing: {}", session_id);
            }
            other => return Err(other.into()),
        }
    }

    let mut chat_service =
        ChatService::new(agent, session_id.clone(), storage).with_system_prompt(system_prompt);

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
                    LocalInferenceLlmAdapter::new_with_data_dir(&hf_model_id, &data_dir).await?,
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
                effective_model, llamafile_url, settings.llm_max_tokens, settings.llm_temperature,
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
    // Build a shared in-process Whisper backend once per session. It powers
    // both the `VoiceInput` adapter and the wake-word detector — no separate
    // KWS subprocess needed any more.
    let whisper_backend: Option<Arc<WhisperRsInput>> = if input == "whisper" {
        match &whisper_model_path {
            Some(p) => match WhisperRsInput::new(p.clone()) {
                Ok(w) => Some(Arc::new(w)),
                Err(e) => {
                    println!("  ⚠  In-process whisper load failed: {}", e);
                    println!("     Falling back to stdin input.");
                    None
                }
            },
            None => None,
        }
    } else {
        None
    };

    let voice: Arc<dyn VoiceInput> = match (input, &whisper_backend) {
        ("whisper", Some(backend)) => {
            println!("  Input:    whisper (in-process via whisper.cpp)");
            backend.clone() as Arc<dyn VoiceInput>
        }
        _ => {
            println!("  Input:    stdin");
            Arc::new(StdinInput::new())
        }
    };
    chat_service = chat_service.with_voice_input(voice);

    // ── Wire wake word detector ──
    if no_wake_word || input != "whisper" || whisper_backend.is_none() {
        chat_service = chat_service.with_wake_word_detector(Arc::new(InstantActivation));
    } else {
        let backend = whisper_backend.clone().expect("checked above");
        let trigger = wake_word.unwrap_or(settings.voice_wake_word.as_str());
        let transcriptions = settings.voice_wake_word_transcriptions.clone();

        if transcriptions.is_empty() {
            println!(
                "  Wake word: \"{}\" (no calibration — using raw phrase)",
                trigger
            );
        } else {
            println!(
                "  Wake word: \"{}\" ({} calibrated variants)",
                trigger,
                transcriptions.len()
            );
        }
        println!(
            "  Energy gate:   {:.3} RMS  |  cooldown: {}ms  |  VAD silence: {}ms",
            settings.voice_kws_energy_threshold,
            settings.voice_kws_cooldown_ms,
            settings.voice_kws_post_trigger_silence_ms
        );

        use pond_adapters_whisper::{KeywordDetectorConfig, WhisperBackend};
        let kws_config = KeywordDetectorConfig {
            energy_threshold: settings.voice_kws_energy_threshold,
            post_trigger_silence_ms: settings.voice_kws_post_trigger_silence_ms,
            cooldown_ms: settings.voice_kws_cooldown_ms,
            ..KeywordDetectorConfig::default()
        };

        let detector = Arc::new(
            WhisperKeywordDetector::new(backend as Arc<dyn WhisperBackend>, trigger)
                .with_transcriptions(transcriptions)
                .with_config(kws_config),
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
                    // Piper requires both the .onnx weights AND the .onnx.json config.
                    // Check both — the JSON is often missing even when the onnx was
                    // downloaded in an earlier version that didn't fetch the config.
                    let config_path =
                        std::path::PathBuf::from(format!("{}.json", model_path.display()));
                    if !model_path.exists() || !config_path.exists() {
                        if model_path.exists() {
                            println!("  📥 TTS model config (.json) missing — downloading...");
                        } else {
                            println!("  📥 TTS model not found — downloading configured voice...");
                        }
                        // Look up in DB by filename to get the correct download URL.
                        let voice_filename = settings.voice_tts_voice.as_str();
                        let registry_entry = SqliteModelRepository::new(db_system.clone())
                            .list_by_category(&ModelCategory::TtsPiper)
                            .await
                            .unwrap_or_default()
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
                            let _ = model_download::download_piper_model_entry(
                                &data_dir, &mf, &cf, &mu, &cu, sz,
                            )
                            .await;
                        } else {
                            println!(
                                "  ⚠  Piper voice '{}' not in model catalog — cannot download",
                                voice_filename
                            );
                        }
                    }

                    // espeak-ng-data: in-process backend uses the env var
                    // path; legacy subprocess passes it as --espeak_data.
                    let espeak_data_dir = {
                        let p = model_download::piper_espeak_data_path(&data_dir);
                        if p.exists() {
                            Some(p)
                        } else {
                            None
                        }
                    };

                    // Default: in-process. Loads the .onnx + .onnx.json via
                    // piper-rs and synthesises with zero subprocess overhead.
                    #[cfg(not(feature = "legacy-subprocess"))]
                    {
                        if !model_path.exists() || !config_path.exists() {
                            println!("  TTS:      piper unavailable (model or config missing) — falling back to print");
                            Arc::new(PrintOutput) as Arc<dyn VoiceOutput>
                        } else {
                            match PiperRsOutput::new(model_path.clone(), config_path) {
                                Ok(out) => {
                                    let out = match espeak_data_dir {
                                        Some(d) => out.with_espeak_data(d),
                                        None => out,
                                    };
                                    println!(
                                        "  TTS:      piper-rs ({})",
                                        model_path
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                    );
                                    Arc::new(out) as Arc<dyn VoiceOutput>
                                }
                                Err(e) => {
                                    println!(
                                        "  TTS:      piper unavailable (load failed: {}) — falling back to print",
                                        e
                                    );
                                    Arc::new(PrintOutput) as Arc<dyn VoiceOutput>
                                }
                            }
                        }
                    }

                    // Legacy: subprocess `piper` binary.
                    #[cfg(feature = "legacy-subprocess")]
                    {
                        if piper_process::find_binary(&data_dir).is_none() {
                            println!("  📥 TTS binary not found — downloading...");
                            match model_download::download_piper_binary(&data_dir).await {
                                Ok(_) => {}
                                Err(e) => println!("  ⚠  TTS binary download failed: {}", e),
                            }
                        }
                        match piper_process::find_binary(&data_dir) {
                            Some(bin) => {
                                println!(
                                    "  TTS:      piper ({})",
                                    model_path.file_name().unwrap_or_default().to_string_lossy()
                                );
                                let mut out = PiperOutput::new(bin, model_path);
                                if let Some(d) = espeak_data_dir {
                                    out = out.with_espeak_data(d);
                                }
                                Arc::new(out) as Arc<dyn VoiceOutput>
                            }
                            None => {
                                println!("  TTS:      piper unavailable (binary not found) — falling back to print");
                                Arc::new(PrintOutput) as Arc<dyn VoiceOutput>
                            }
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

    chat_service.run_loop().await?;

    Ok(())
}

// ── Adversarial Answer Reviewer ───────────────────────────────────────────────

/// Adversarial answer reviewer — post-inference quality gate.
///
/// Uses the same LlmProvider as the main LLM with a critic system prompt.
/// Reviews the completed answer, and if it scores below threshold, sends
/// the critique back to the LLM for revision.
struct GiapAnswerReviewer {
    /// Live provider reference — reads from the RwLock so it always uses
    /// whatever model is currently loaded.
    live_provider:
        Arc<tokio::sync::RwLock<Option<Arc<dyn pond_core::models::ports::provider::LlmProvider>>>>,
    pass_threshold: u8,
    max_rounds: u32,
}

#[async_trait::async_trait]
impl pond_core::models::ports::answer_reviewer::AnswerReviewer for GiapAnswerReviewer {
    async fn review(
        &self,
        question: &str,
        answer: &str,
        tool_context: Option<&str>,
    ) -> anyhow::Result<pond_core::models::ports::answer_reviewer::ReviewResult> {
        use pond_core::models::domain::message::ChatMessage;
        use pond_core::models::ports::answer_reviewer::{ReviewResult, ReviewVerdict};

        // Read the LIVE provider — always uses whatever model is currently loaded.
        let provider = {
            let guard = self.live_provider.read().await;
            match guard.as_ref() {
                Some(p) => p.clone(),
                None => return Err(anyhow::anyhow!("no LLM provider available for review")),
            }
        };

        let mut current_answer = answer.to_string();
        let mut rounds = 0u32;
        let mut last_verdict: Option<ReviewVerdict> = None;

        for _ in 0..self.max_rounds {
            rounds += 1;

            // Step 1: Review the current answer
            let review_input = if let Some(ctx) = tool_context {
                format!(
                    "QUESTION: {}\n\nCONTEXT PROVIDED TO THE ANSWERER:\n{}\n\nANSWER TO REVIEW:\n{}",
                    question, ctx, current_answer
                )
            } else {
                format!(
                    "QUESTION: {}\n\nANSWER TO REVIEW:\n{}",
                    question, current_answer
                )
            };

            println!("[answer-reviewer] reviewing (round {})...", rounds);
            let review_msg = vec![ChatMessage::user(review_input)];
            let review_response = provider
                .complete(pond_core::prompts::REVIEW_SYSTEM_PROMPT, review_msg)
                .await?;

            let verdict = parse_review_verdict(&review_response.content);
            println!(
                "[answer-reviewer] verdict: pass={}, score={}/5",
                verdict.pass, verdict.score
            );

            if verdict.pass || verdict.score >= self.pass_threshold {
                return Ok(ReviewResult {
                    final_answer: current_answer,
                    was_revised: last_verdict.is_some(),
                    verdict,
                    rounds,
                });
            }

            // Step 2: Revise the answer using the critique
            println!("[answer-reviewer] critique: {}", verdict.critique);
            let revision_input = format!(
                "ORIGINAL QUESTION: {}\n\n\
                YOUR PREVIOUS ANSWER:\n{}\n\n\
                REVIEWER CRITIQUE:\n{}\n\n\
                WHAT THE ANSWER SHOULD INCLUDE:\n- {}\n\n\
                Please provide an improved, more thorough answer.",
                question,
                current_answer,
                verdict.critique,
                verdict.expectations.join("\n- ")
            );

            println!("[answer-reviewer] revising...");
            let revision_msg = vec![ChatMessage::user(revision_input)];
            let revision_response = provider
                .complete(pond_core::prompts::REVISION_SYSTEM_PROMPT, revision_msg)
                .await?;

            // Strip thinking tags — the revision model (same GGUF provider)
            // may emit <think>/<thought>/<|channel>thought reasoning in its
            // revision, and this bypasses the SSE ThoughtFilter.
            current_answer = strip_thinking_tags(&revision_response.content);
            last_verdict = Some(verdict);
        }

        // Exhausted rounds — return the last revision
        Ok(ReviewResult {
            final_answer: current_answer,
            was_revised: true,
            verdict: last_verdict.unwrap_or(ReviewVerdict {
                pass: true,
                score: 3,
                expectations: Vec::new(),
                critique: String::new(),
            }),
            rounds,
        })
    }
}

/// Strip reasoning-channel tags from model output.
///
/// Handles Gemma 4 (`<|channel>thought...<channel|>`), Qwen3/DeepSeek
/// (`<think>...</think>`), and alternate `<thought>...</thought>` format.
fn strip_thinking_tags(raw: &str) -> String {
    let mut text = raw.to_string();

    // Gemma 4: everything after last <channel|>
    if let Some(pos) = text.rfind("<channel|>") {
        text = text[pos + "<channel|>".len()..].trim().to_string();
    }

    // <think>...</think> blocks
    while let Some(start) = text.find("<think>") {
        if let Some(end) = text[start..].find("</think>") {
            let before = &text[..start];
            let after = &text[start + end + "</think>".len()..];
            text = format!("{}{}", before, after);
        } else {
            text = text[..start].to_string();
            break;
        }
    }

    // <thought>...</thought> blocks
    while let Some(start) = text.find("<thought>") {
        if let Some(end) = text[start..].find("</thought>") {
            let before = &text[..start];
            let after = &text[start + end + "</thought>".len()..];
            text = format!("{}{}", before, after);
        } else {
            text = text[..start].to_string();
            break;
        }
    }

    text.trim().to_string()
}

/// Parse a review verdict from LLM output.
///
/// Tries to extract JSON from the response text. If parsing fails,
/// defaults to `pass: true` — review must never block the user.
fn parse_review_verdict(text: &str) -> pond_core::models::ports::answer_reviewer::ReviewVerdict {
    use pond_core::models::ports::answer_reviewer::ReviewVerdict;

    let json_start = text.find('{');
    let json_end = text.rfind('}');
    if let (Some(start), Some(end)) = (json_start, json_end) {
        if end > start {
            if let Ok(verdict) = serde_json::from_str::<ReviewVerdict>(&text[start..=end]) {
                return verdict;
            }
        }
    }

    // Fallback — unparseable output defaults to pass
    println!(
        "[answer-reviewer] WARNING: unparseable verdict, defaulting to pass: {:?}",
        &text[..text.len().min(100)]
    );
    ReviewVerdict {
        pass: true,
        score: 3,
        expectations: Vec::new(),
        critique: String::new(),
    }
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
/// Run the three-stage adversarial consolidation pipeline, apply accepted
/// actions, and broadcast events. Shared by the inactivity scheduler and the
/// manual `POST /api/v1/memory/consolidate` endpoint (via `ConsolidationRunner`).
async fn run_consolidation_pipeline(
    repo: Arc<dyn pond_core::user_data::ports::memory_repository::MemoryRepository + Send + Sync>,
    provider: Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>,
    broadcast_tx: tokio::sync::broadcast::Sender<
        pond_core::user_data::ports::memory_consolidator::ConsolidationEvent,
    >,
    cancel: tokio_util::sync::CancellationToken,
) {
    use pond_core::user_data::domain::memory::MemoryLifecycle;
    use pond_core::user_data::ports::memory_consolidator::{
        ConsolidationAction, ConsolidationEvent,
    };

    let memories = match repo.search_scoreable(None).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("consolidation: failed to load memories: {e}");
            let _ = broadcast_tx.send(ConsolidationEvent::Error {
                message: e.to_string(),
            });
            return;
        }
    };

    if memories.len() < 6 {
        tracing::debug!(
            "consolidation: skipped — only {} memories (need >= 6)",
            memories.len()
        );
        let _ = broadcast_tx.send(ConsolidationEvent::Error {
            message: format!(
                "Need at least 6 memories to consolidate, found {}",
                memories.len()
            ),
        });
        return;
    }

    // Bridge: mpsc -> broadcast so the consolidator writes to mpsc and the
    // SSE stream (if any) reads from broadcast.
    let (mpsc_tx, mut mpsc_rx) = tokio::sync::mpsc::channel::<ConsolidationEvent>(64);
    let bridge_tx = broadcast_tx.clone();
    tokio::spawn(async move {
        while let Some(event) = mpsc_rx.recv().await {
            let _ = bridge_tx.send(event);
        }
    });

    let memory_count = memories.len();
    let consolidator = three_stage_consolidator::ThreeStageConsolidator::new(provider);
    let result = consolidator.run(&memories, cancel, Some(mpsc_tx)).await;

    match result {
        Ok(ref result) => {
            // Build lookup for source memory metadata (corrects, segment)
            let memory_map: std::collections::HashMap<
                &str,
                &pond_core::user_data::domain::memory::MemoryFragment,
            > = memories.iter().map(|m| (m.id.as_str(), m)).collect();

            for exchange in &result.exchanges {
                if !exchange.judgment.accepted {
                    continue;
                }
                match &exchange.proposal.action {
                    ConsolidationAction::Merge {
                        source_ids,
                        merged_content,
                        segment,
                        importance,
                    } => {
                        // Guard: if any source is a correction, preserve its metadata
                        let any_correction = source_ids
                            .iter()
                            .filter_map(|id| memory_map.get(id.as_str()))
                            .any(|m| m.is_correction());

                        let effective_segment = if any_correction {
                            pond_core::user_data::domain::memory::MemorySegment::Correction
                        } else {
                            segment.clone()
                        };

                        let corrects = source_ids
                            .iter()
                            .filter_map(|id| memory_map.get(id.as_str()))
                            .filter_map(|m| m.corrects.clone())
                            .next();

                        if any_correction {
                            tracing::info!(
                                "[consolidation] merge includes correction source — forcing segment=Correction"
                            );
                        }

                        let new_frag =
                            pond_core::user_data::domain::memory::MemoryFragment::from_extraction(
                                uuid::Uuid::new_v4().to_string(),
                                None,
                                merged_content.clone(),
                                effective_segment,
                                *importance,
                                corrects,
                            );
                        let new_id = new_frag.id.clone();
                        let _ = repo.add(new_frag).await;
                        for src_id in source_ids {
                            let _ = repo.mark_superseded(src_id, &new_id).await;
                            let _ = repo
                                .log_event(
                                    pond_core::user_data::domain::memory::MemoryEventKind::Superseded,
                                    src_id,
                                    None,
                                    Some(&new_id),
                                )
                                .await;
                        }
                        let _ = repo
                            .log_event(
                                pond_core::user_data::domain::memory::MemoryEventKind::Consolidated,
                                &new_id,
                                None,
                                None,
                            )
                            .await;
                    }
                    ConsolidationAction::Prune { id } => {
                        // Guard: never prune correction memories
                        if let Some(mem) = memory_map.get(id.as_str()) {
                            if mem.is_correction() {
                                tracing::warn!(
                                    "[consolidation] blocked prune of correction memory {id}"
                                );
                                continue;
                            }
                        }

                        let _ = repo.update_lifecycle(id, MemoryLifecycle::Archived).await;
                        let _ = repo
                            .log_event(
                                pond_core::user_data::domain::memory::MemoryEventKind::Pruned,
                                id,
                                None,
                                None,
                            )
                            .await;
                    }
                    ConsolidationAction::Recategorize {
                        id,
                        new_segment,
                        new_importance,
                    } => {
                        let _ = repo
                            .update_segment(id, new_segment.clone(), *new_importance)
                            .await;
                        let _ = repo
                            .log_event(
                                pond_core::user_data::domain::memory::MemoryEventKind::Consolidated,
                                id,
                                None,
                                Some("recategorized"),
                            )
                            .await;
                    }
                    ConsolidationAction::Split {
                        source_id,
                        new_memories,
                    } => {
                        // If source is a correction, propagate corrects to split entries
                        let source_corrects = memory_map
                            .get(source_id.as_str())
                            .and_then(|m| m.corrects.clone());

                        let mut first_new_id = String::new();
                        let mut corrects_assigned = false;

                        for entry in new_memories {
                            let entry_corrects = if !corrects_assigned
                                && source_corrects.is_some()
                                && entry.segment
                                    == pond_core::user_data::domain::memory::MemorySegment::Correction
                            {
                                corrects_assigned = true;
                                source_corrects.clone()
                            } else {
                                None
                            };

                            let new_frag =
                                pond_core::user_data::domain::memory::MemoryFragment::from_extraction(
                                    uuid::Uuid::new_v4().to_string(),
                                    None,
                                    entry.content.clone(),
                                    entry.segment.clone(),
                                    entry.importance,
                                    entry_corrects,
                                );
                            let nid = new_frag.id.clone();
                            if first_new_id.is_empty() {
                                first_new_id = nid.clone();
                            }
                            let _ = repo.add(new_frag).await;
                            let _ = repo
                                .log_event(
                                    pond_core::user_data::domain::memory::MemoryEventKind::Consolidated,
                                    &nid,
                                    None,
                                    None,
                                )
                                .await;
                        }
                        if !first_new_id.is_empty() {
                            let _ = repo.mark_superseded(source_id, &first_new_id).await;
                            let _ = repo
                                .log_event(
                                    pond_core::user_data::domain::memory::MemoryEventKind::Superseded,
                                    source_id,
                                    None,
                                    Some(&first_new_id),
                                )
                                .await;
                        }
                    }
                }
            }
            if result.accepted_count > 0 || result.rejected_count > 0 {
                tracing::info!(
                    accepted = result.accepted_count,
                    rejected = result.rejected_count,
                    duration_ms = result.duration_ms,
                    "adversarial consolidation complete"
                );
                // Persist audit trail
                let details_json = serde_json::to_string(&result).ok();
                let _ = repo
                    .log_consolidation_run(
                        "adversarial",
                        memory_count,
                        result.accepted_count,
                        result.rejected_count,
                        result.duration_ms,
                        details_json.as_deref(),
                    )
                    .await;
            }
        }
        Err(e) => {
            tracing::warn!("consolidation failed: {e}");
            let _ = broadcast_tx.send(ConsolidationEvent::Error {
                message: e.to_string(),
            });
        }
    }
}

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

    println!(
        "  [debug] tailing pond_logs.db event_log (cursor = {})...",
        cursor
    );

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let rows: Vec<(i64, String, String, String, String, Option<String>)> = sqlx::query_as(
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
                None => println!("  [db] {} {:>5} [{}] {}", timestamp, level, source, message),
            }
            cursor = id;
        }
    }
}

async fn run_status() -> Result<()> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let hostname = hostname
        .strip_suffix(".local")
        .unwrap_or(&hostname)
        .to_string();

    println!("  🦆 Goose In A Pond — Status");
    println!("  ─────────────────────────────");
    println!("  Version:   {}", env!("CARGO_PKG_VERSION"));
    println!("  Hostname:  {}", hostname);
    println!(
        "  Platform:  {} / {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    let data_dir = default_data_dir();
    let db_path = data_dir.join("pond_system.db");
    println!("  Database:  {}", db_path.display());
    if db_path.exists() {
        if let Ok(db) = Database::init(&data_dir).await {
            let settings_repo = SqliteSettingsRepository::new(db.system.clone());
            if let Ok(s) = settings_repo.get().await {
                println!(
                    "  Assistant: {} | Provider: {} | Model: {}",
                    s.assistant_name, s.chat_provider, s.active_llm_model
                );
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

// ── ONNX Runtime version pinned for auto-download ────────────────────────────
//
// v1.21.0 is the latest release with pre-built tarballs for all four
// platform/arch combos we support (macOS arm64/x86_64, Linux x64/aarch64).
// Bump this when upgrading — the archive layout is stable across releases.
const ORT_VERSION: &str = "1.24.2";

/// Approximate size of the platform library in MB (for the progress message).
const ORT_APPROX_SIZE_MB: u64 = 30;

/// Ensure that `ORT_DYLIB_PATH` points at a usable ONNX Runtime shared
/// library.  Resolution order:
///
///   1. Explicit env var — operator knows best; skip everything.
///   2. Well-known system paths (Homebrew, system `/usr/lib`).
///   3. Previously-downloaded local copy in `$DATA_DIR/lib/`.
///   4. Auto-download from GitHub Releases into `$DATA_DIR/lib/`.
///
/// Runs synchronously before tokio starts, so it shells out to `curl`+`tar`
/// instead of using async I/O.  Failures are non-fatal — the server starts
/// without ONNX-dependent features (face recognition, embeddings).
fn ensure_onnx_runtime() {
    // ── 1. Explicit env var ──────────────────────────────────────────────
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }

    // ── 2. Well-known system locations ───────────────────────────────────
    let system_candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/opt/homebrew/lib/libonnxruntime.dylib",
            "/usr/local/lib/libonnxruntime.dylib",
        ]
    } else {
        &[
            "/usr/lib/libonnxruntime.so",
            "/usr/lib/x86_64-linux-gnu/libonnxruntime.so",
            "/usr/lib/aarch64-linux-gnu/libonnxruntime.so",
            "/usr/local/lib/libonnxruntime.so",
        ]
    };

    for p in system_candidates {
        if std::path::Path::new(p).exists() {
            // SAFETY: single-threaded setup, before any worker spawns.
            unsafe { std::env::set_var("ORT_DYLIB_PATH", p) };
            return;
        }
    }

    // ── 3. Previously-downloaded local copy ──────────────────────────────
    let data_dir = default_data_dir();
    let (lib_name, versioned_name) = ort_lib_names();
    let lib_dir = data_dir.join("lib");
    let local_versioned = lib_dir.join(versioned_name);
    let local_unversioned = lib_dir.join(lib_name);

    // Prefer the versioned file (it is the real binary); fall back to the
    // unversioned name in case someone placed it there manually.
    for candidate in [&local_versioned, &local_unversioned] {
        if candidate.exists() {
            unsafe { std::env::set_var("ORT_DYLIB_PATH", candidate) };
            return;
        }
    }

    // ── 4. Auto-download ─────────────────────────────────────────────────
    let (os_tag, arch_tag) = match ort_platform_tags() {
        Some(tags) => tags,
        None => {
            eprintln!(
                "  ⚠  ONNX Runtime auto-download: unsupported platform \
                 (not macOS/Linux, or unsupported arch)"
            );
            return;
        }
    };

    let archive_stem = format!("onnxruntime-{os_tag}-{arch_tag}-{ORT_VERSION}");
    let url = format!(
        "https://github.com/microsoft/onnxruntime/releases/download/\
         v{ORT_VERSION}/{archive_stem}.tgz"
    );

    println!(
        "  ⬇  ONNX Runtime v{ORT_VERSION} not found — downloading (~{ORT_APPROX_SIZE_MB} MB)..."
    );

    match download_and_extract_ort(&url, &lib_dir, &archive_stem) {
        Ok(lib_path) => {
            println!("  ✅ ONNX Runtime installed to {}", lib_path.display());
            // SAFETY: single-threaded setup, before any worker spawns.
            unsafe { std::env::set_var("ORT_DYLIB_PATH", &lib_path) };
        }
        Err(e) => {
            eprintln!("  ⚠  Failed to download ONNX Runtime: {e}");
            eprintln!("     Embedding models and face recognition will be unavailable.");
            eprintln!("     Install manually: brew install onnxruntime");
        }
    }
}

/// Returns `(lib_name, versioned_name)` for the current platform.
///
/// - macOS: `("libonnxruntime.dylib", "libonnxruntime.{ver}.dylib")`
/// - Linux: `("libonnxruntime.so",    "libonnxruntime.so.{ver}")`
fn ort_lib_names() -> (&'static str, String) {
    if cfg!(target_os = "macos") {
        (
            "libonnxruntime.dylib",
            format!("libonnxruntime.{ORT_VERSION}.dylib"),
        )
    } else {
        (
            "libonnxruntime.so",
            format!("libonnxruntime.so.{ORT_VERSION}"),
        )
    }
}

/// Returns `(os_tag, arch_tag)` matching the GitHub release archive naming
/// convention, e.g. `("osx", "arm64")` or `("linux", "x64")`.
fn ort_platform_tags() -> Option<(&'static str, &'static str)> {
    let os = if cfg!(target_os = "macos") {
        "osx"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return None;
    };

    let arch = if cfg!(target_arch = "aarch64") {
        if cfg!(target_os = "macos") {
            "arm64"
        } else {
            "aarch64"
        }
    } else if cfg!(target_arch = "x86_64") {
        if cfg!(target_os = "macos") {
            "x86_64"
        } else {
            "x64"
        }
    } else {
        return None;
    };

    Some((os, arch))
}

/// Download an ONNX Runtime release tarball and extract the shared library
/// into `lib_dir`.  Uses `curl` + `tar` which are available on both macOS
/// and Linux without pulling in extra Rust dependencies.
///
/// Archive layout (stable across ORT releases):
/// ```text
/// onnxruntime-{os}-{arch}-{ver}/
///   lib/
///     libonnxruntime.{ver}.dylib   ← real file  (macOS)
///     libonnxruntime.dylib         ← symlink     (macOS)
///     libonnxruntime.so.{ver}      ← real file  (Linux)
///     libonnxruntime.so.1          ← symlink     (Linux)
///     libonnxruntime.so            ← symlink     (Linux)
/// ```
///
/// We extract only the `lib/` subtree with `--strip-components=1`, which
/// places the files directly into `lib_dir/lib/…` → we then just point at
/// the versioned file.
fn download_and_extract_ort(
    url: &str,
    lib_dir: &std::path::Path,
    archive_stem: &str,
) -> anyhow::Result<std::path::PathBuf> {
    use std::process::Command;

    // Ensure the target directory exists.
    std::fs::create_dir_all(lib_dir)?;

    let tmp_dir = std::env::temp_dir().join("giap-ort-download");
    // Clean up any leftover from a previous failed attempt.
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir)?;

    let tgz = tmp_dir.join("ort.tgz");

    // ── Download ─────────────────────────────────────────────────────────
    let status = Command::new("curl")
        .args([
            "-fSL",           // fail on HTTP errors, show errors, follow redirects
            "--progress-bar", // minimal progress indicator
            url,
            "-o",
        ])
        .arg(&tgz)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run curl: {e}"))?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("curl exited with status {status} for {url}");
    }

    // ── Extract lib/ subtree directly into lib_dir ───────────────────────
    // `--strip-components=1` removes the top-level `onnxruntime-{os}-…/`
    // prefix, so `lib/libonnxruntime.*` lands at `{lib_dir}/lib/…`.
    //
    // We use `--include` (GNU tar) / `--include` (bsdtar, macOS default) to
    // extract only the library files, skipping headers and pkgconfig.
    let status = Command::new("tar")
        .args(["xzf"])
        .arg(&tgz)
        .args(["-C"])
        .arg(&tmp_dir)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run tar: {e}"))?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("tar extraction failed with status {status}");
    }

    // ── Copy the library files into lib_dir ──────────────────────────────
    let extracted_lib_dir = tmp_dir.join(archive_stem).join("lib");
    let (_, versioned_name) = ort_lib_names();

    let src = extracted_lib_dir.join(&versioned_name);
    if !src.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        anyhow::bail!(
            "{versioned_name} not found in extracted archive at {}",
            extracted_lib_dir.display()
        );
    }

    let dest = lib_dir.join(&versioned_name);
    std::fs::copy(&src, &dest).map_err(|e| {
        anyhow::anyhow!("failed to copy {} → {}: {e}", src.display(), dest.display())
    })?;

    // Clean up the temp directory.
    let _ = std::fs::remove_dir_all(&tmp_dir);

    Ok(dest)
}

/// Populate the face-recognition-specific env vars with values that are
/// known to work end-to-end on a fresh install.
///
/// **Does not touch `ORT_DYLIB_PATH`** — that is handled by
/// `ensure_onnx_runtime()`, which must run first.
///
/// Each var is only set when currently **unset** — explicit values from
/// the operator's shell keep taking precedence.
///
/// The anti-spoof tuning (`LIVE_INDEX=2`, `PIXEL_SCALE=unit`) reflects the
/// specific 3-class Silent-Face ONNX file we shipped install instructions
/// for — on that export, slot 2 is the live class and the preprocess
/// expects `[0, 1]` pixels.  Other exports need different values; override
/// at the shell if you swap the model file.
fn apply_face_recognition_defaults() {
    // Anti-spoof ONNX model — default to the canonical location under the
    // platform data dir so users who followed the README land here too.
    if std::env::var_os("POND_FACE_ANTISPOOF_PATH").is_none() {
        let default_path = default_data_dir()
            .join("models")
            .join("face")
            .join("antispoof.onnx");
        if default_path.exists() {
            // SAFETY: single-threaded setup, before any worker spawns.
            unsafe { std::env::set_var("POND_FACE_ANTISPOOF_PATH", default_path) };
        }
    }

    // Tuning for the specific 3-class Silent-Face export we ship.
    if std::env::var_os("POND_FACE_ANTISPOOF_LIVE_INDEX").is_none() {
        unsafe { std::env::set_var("POND_FACE_ANTISPOOF_LIVE_INDEX", "2") };
    }
    if std::env::var_os("POND_FACE_ANTISPOOF_PIXEL_SCALE").is_none() {
        unsafe { std::env::set_var("POND_FACE_ANTISPOOF_PIXEL_SCALE", "unit") };
    }

    // Secondary PAD (DeepPixBis) for the ensemble — same auto-opt-in logic
    // as in `build_face_recognition`. Kept in both code paths because each
    // is reachable under different launch flows (`setup` vs `serve`).
    if std::env::var_os("POND_FACE_ANTISPOOF_PATH_2").is_none() {
        let default_path = default_data_dir()
            .join("models")
            .join("face")
            .join("OULU_Protocol_2_model_0_0.onnx");
        if default_path.exists() {
            // SAFETY: single-threaded setup, before any worker spawns.
            unsafe { std::env::set_var("POND_FACE_ANTISPOOF_PATH_2", default_path) };
        }
    }
}
/// `pond-server calibrate` — record N samples of the wake-word phrase and store
/// Whisper's transcriptions as calibration variants in settings.
async fn run_calibrate(
    phrase_arg: Option<&str>,
    target_samples: usize,
    _whisper_url_arg: Option<&str>,
    reset: bool,
) -> Result<()> {
    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;
    let settings_repo = SqliteSettingsRepository::new(db.system.clone());
    let mut settings = settings_repo.get().await?;

    let phrase = phrase_arg
        .unwrap_or(settings.voice_wake_word.as_str())
        .to_string();

    // Resolve the ggml model path used by the in-process whisper backend.
    let whisper_model_name = settings.active_whisper_model.clone();
    if whisper_model_name.is_empty() {
        anyhow::bail!(
            "No whisper model configured in Settings. Pick a model in the Models UI \
             before running calibrate."
        );
    }
    let whisper_filename = SqliteModelRepository::new(db.system.clone())
        .get_by_id(&format!("whisper/{}", whisper_model_name))
        .await
        .ok()
        .flatten()
        .and_then(|r| r.filename)
        .unwrap_or_else(|| format!("ggml-{}.en.bin", whisper_model_name));
    let whisper_model_path = data_dir.join("models").join(&whisper_filename);
    if !whisper_model_path.exists() {
        anyhow::bail!(
            "Whisper model not downloaded yet: {}\nRun `pond-server setup` first.",
            whisper_model_path.display()
        );
    }

    println!();
    println!("  ╔═══════════════════════════════════════════════╗");
    println!("  ║   🎤  Wake-Word Calibration                   ║");
    println!("  ╚═══════════════════════════════════════════════╝");
    println!("  Phrase:       \"{}\"", phrase);
    println!("  Model:        {} (in-process)", whisper_filename);
    println!("  Samples:      {}", target_samples);
    println!();

    if reset {
        settings.voice_wake_word_transcriptions.clear();
        settings_repo.update(&settings).await?;
        println!("  ✓  Previous calibration data cleared.");
        println!();
    } else if !settings.voice_wake_word_transcriptions.is_empty() {
        println!(
            "  Existing variants ({}):",
            settings.voice_wake_word_transcriptions.len()
        );
        for v in &settings.voice_wake_word_transcriptions {
            println!("    • {}", v);
        }
        println!("  (add --reset to discard these and start fresh)");
        println!();
    }

    if phrase_arg.is_some() {
        settings.voice_wake_word = phrase.clone();
    }

    let whisper = WhisperRsInput::new(whisper_model_path.clone())
        .with_context(|| format!("loading whisper model: {}", whisper_model_path.display()))?;
    let mut collected = 0usize;
    let mut attempt = 0usize;

    while collected < target_samples {
        attempt += 1;
        println!(
            "  ── Sample {} / {} ─────────────────────────────────",
            collected + 1,
            target_samples
        );
        println!("  Press Enter, then say \"{}\"...", phrase);
        {
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
                println!(" (error: {})", e);
                if attempt >= target_samples * 3 {
                    anyhow::bail!("Too many failed attempts — aborting calibration.");
                }
                continue;
            }
        };

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

        if settings
            .voice_wake_word_transcriptions
            .contains(&normalized)
        {
            println!("  (already stored as a variant — skipping duplicate)");
            collected += 1;
            continue;
        }

        settings
            .voice_wake_word_transcriptions
            .push(normalized.clone());
        settings_repo.update(&settings).await?;
        collected += 1;

        println!(
            "  ✓  Stored: \"{}\"  ({}/{})",
            normalized, collected, target_samples
        );
        println!();
    }

    println!("  ╔═══════════════════════════════════════════════╗");
    println!("  ║   ✅  Calibration Complete!                   ║");
    println!("  ╚═══════════════════════════════════════════════╝");
    println!("  Phrase:    \"{}\"", settings.voice_wake_word);
    println!(
        "  Variants ({}):",
        settings.voice_wake_word_transcriptions.len()
    );
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

/// Build the face-recognition service for Phase 2.
///
/// Returns `None` in three cases:
///   1. The `face-onnx` Cargo feature is disabled.
///   2. No ONNX embedding model is present at `$DATA_DIR/models/face/arcface.onnx`.
///   3. The ONNX Runtime shared library could not be loaded.
///
/// In all cases the server continues to start normally; the face endpoints
/// return 503 until a model is supplied.
#[cfg(feature = "face-onnx")]
fn build_face_recognition(
    data_dir: &std::path::Path,
    pool: sqlx::Pool<sqlx::Sqlite>,
) -> Option<Arc<dyn pond_core::user_data::ports::face_recognition::FaceRecognition>> {
    use pond_adapters_face_onnx::{
        EmbeddingModel, OnnxFaceEmbeddingExtractor, ScrfdDetector, UltraFaceDetector,
    };
    use pond_core::user_data::ports::face_detector::FaceDetector;
    use pond_core::user_data::ports::face_embedding_extractor::FaceEmbeddingExtractor;
    use pond_infra::sqlite_face_recognition::SqliteFaceRecognition;

    // Embedder lookup, preferred → fallback:
    //   1. `POND_FACE_MODEL_PATH` (explicit operator override)
    //   2. `adaface_ir101.onnx`   (AdaFace IR-101 — best low-light tolerance)
    //   3. `w600k_r50.onnx`       (ArcFace R50 from buffalo_l)
    //   4. `arcface.onnx`         (legacy filename, still supported)
    let model_path = match std::env::var("POND_FACE_MODEL_PATH") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => {
            let adaface = data_dir.join("models/face/adaface_ir101.onnx");
            let arcface = data_dir.join("models/face/w600k_r50.onnx");
            let legacy = data_dir.join("models/face/arcface.onnx");
            if adaface.exists() {
                adaface
            } else if arcface.exists() {
                arcface
            } else {
                legacy
            }
        }
    };

    // Default the anti-spoof path so users get the Silent-Face PAD gate
    // for free once the model file is present, with no env-var setup.
    if std::env::var_os("POND_FACE_ANTISPOOF_PATH").is_none() {
        let antispoof_default = data_dir.join("models/face/antispoof.onnx");
        if antispoof_default.exists() {
            // SAFETY: single-threaded init phase before any task scheduling.
            unsafe {
                std::env::set_var("POND_FACE_ANTISPOOF_PATH", antispoof_default.as_os_str());
            }
        }
    }
    // Same idea for the live-class index — the Silent-Face MiniFASNetV2
    // export at the install URL we ship has [fake_2D, fake_3D, live] order
    // (live is index 2), but the in-tree default is `auto` which assumes
    // index 0.  Pin the default to 2 so the model works out-of-the-box.
    if std::env::var_os("POND_FACE_ANTISPOOF_LIVE_INDEX").is_none() {
        unsafe {
            std::env::set_var("POND_FACE_ANTISPOOF_LIVE_INDEX", "2");
        }
    }

    // Secondary PAD (DeepPixBis) for the ensemble path in
    // `pond-adapters-face-onnx`.  When the file is on disk and the env var
    // is unset, opt the user into the stronger ensemble automatically —
    // the adapter already takes `max(spoof_score)` of primary + secondary
    // so a missing or dud secondary just falls back to primary-alone.
    if std::env::var_os("POND_FACE_ANTISPOOF_PATH_2").is_none() {
        let secondary_default = data_dir.join("models/face/OULU_Protocol_2_model_0_0.onnx");
        if secondary_default.exists() {
            // SAFETY: single-threaded init phase before any task scheduling.
            unsafe {
                std::env::set_var("POND_FACE_ANTISPOOF_PATH_2", secondary_default.as_os_str());
            }
        }
    }

    if !model_path.exists() {
        tracing::info!(
            "face-onnx feature enabled but no embedding model at {}; face recognition disabled",
            model_path.display()
        );
        return None;
    }

    let model_kind = if model_path.to_string_lossy().contains("mobilefacenet") {
        EmbeddingModel::MobileFaceNet128
    } else {
        EmbeddingModel::ArcFace512
    };

    // ONNX Runtime initialises lazily on the first `Session::builder()` call
    // and *panics* (rather than returning Err) when its dynamic library is
    // missing — see the `ort` crate.  Wrap the entire constructor in
    // `catch_unwind` so a missing libonnxruntime.dylib downgrades to
    // "face disabled" instead of taking down the whole server.  This makes
    // the misconfigured-ORT case behave the same as the missing-model case.
    let extractor: Arc<dyn FaceEmbeddingExtractor> =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            OnnxFaceEmbeddingExtractor::new(&model_path, model_kind)
        })) {
            Ok(Ok(e)) => Arc::new(e),
            Ok(Err(e)) => {
                tracing::warn!("face recognition disabled: {e:#}");
                return None;
            }
            Err(panic) => {
                // Best-effort: ort's panic payload is a String; surface it.
                let msg = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "ort init panicked (unknown payload)".to_string());
                tracing::warn!(
                    "face recognition disabled: ONNX Runtime failed to initialise — {} \
                 (hint: install onnxruntime and set ORT_DYLIB_PATH)",
                    msg
                );
                return None;
            }
        };

    // Detector resolution order:
    //   1. SCRFD at $POND_FACE_SCRFD_PATH or $DATA_DIR/models/face/scrfd.onnx
    //      — landmark-producing, drives similarity-transform alignment
    //      (dramatically better real-world accuracy).
    //   2. UltraFace at $POND_FACE_DETECTOR_PATH or $DATA_DIR/models/face/ultraface.onnx
    //      — bbox only; no alignment, roughly phase-2 baseline behaviour.
    //   3. No detector; adapter falls back to center-square cropping.  Safe
    //      but prone to the "everyone matches" failure mode — log loudly.
    // Detector preference: SCRFD 34G > SCRFD 10G > UltraFace > center-square.
    // 34G catches faces at smaller pixel sizes than 10G (deeper backbone)
    // but is ~140 MB instead of ~17 MB. `POND_FACE_SCRFD_PATH` overrides
    // both SCRFD candidates explicitly.
    let scrfd_path = std::env::var("POND_FACE_SCRFD_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let scrfd_34 = data_dir.join("models/face/scrfd_34g.onnx");
            if scrfd_34.exists() {
                scrfd_34
            } else {
                data_dir.join("models/face/scrfd.onnx")
            }
        });
    let ultraface_path = std::env::var("POND_FACE_DETECTOR_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| data_dir.join("models/face/ultraface.onnx"));

    let detector: Option<Arc<dyn FaceDetector>> = if scrfd_path.exists() {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ScrfdDetector::new(&scrfd_path, 0.5, 0.4)
        }))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("SCRFD ort init panicked")))
        {
            Ok(d) => {
                tracing::info!(
                    "SCRFD face detector loaded from {} — landmark alignment enabled",
                    scrfd_path.display()
                );
                Some(Arc::new(d))
            }
            Err(e) => {
                tracing::warn!("SCRFD detector unavailable: {e:#}; falling back to UltraFace");
                None
            }
        }
    } else {
        None
    };

    let detector = detector.or_else(|| {
        if ultraface_path.exists() {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                UltraFaceDetector::new(&ultraface_path, 0.85, 0.3)
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("UltraFace ort init panicked"))) {
                Ok(d) => {
                    tracing::warn!(
                        "SCRFD model not found at {}; using UltraFace fallback (no landmark alignment). \
                         Download SCRFD to restore real-world accuracy.",
                        scrfd_path.display()
                    );
                    Some(Arc::new(d) as Arc<dyn FaceDetector>)
                }
                Err(e) => {
                    tracing::warn!("UltraFace detector unavailable: {e:#}");
                    None
                }
            }
        } else {
            tracing::warn!(
                "No face detector model found (looked at {} and {}). \
                 Face recognition will use center-square fallback — \
                 accuracy will be poor.",
                scrfd_path.display(), ultraface_path.display(),
            );
            None
        }
    });

    // Thresholds calibrated against ArcFace R100 on Umeyama-aligned 112×112
    // crops.  The previous 0.50 floor was tuned for small in-house test
    // sets where every profile was visually distinct; on real webcams
    // with lighting / pose variance, 0.50 admits far too many
    // cross-identity near-neighbours (a different person can trivially
    // hit 0.55–0.65 post-blend once S-norm + centroid weights are in
    // play).  The new floors sit inside the empirically-safe 0.68–0.75
    // band for ArcFace aligned.  `POND_FACE_MATCH_THRESHOLD` still
    // overrides via the env-driven default, but only when this code
    // path does NOT call `.with_threshold()` — see below.
    let threshold = match (&detector, model_kind) {
        (Some(d), EmbeddingModel::ArcFace512) if d.produces_landmarks() => 0.70,
        (Some(_), EmbeddingModel::ArcFace512) => 0.72,
        (None, EmbeddingModel::ArcFace512) => 0.85,
        (Some(d), EmbeddingModel::MobileFaceNet128) if d.produces_landmarks() => 0.70,
        (Some(_), EmbeddingModel::MobileFaceNet128) => 0.72,
        (None, EmbeddingModel::MobileFaceNet128) => 0.85,
    };
    // Allow a shell-level override to take precedence over the
    // model-aware default — useful when a power user has tuned the gate
    // for their specific enrollment quality.
    let threshold = std::env::var("POND_FACE_MATCH_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
        .unwrap_or(threshold);

    tracing::info!(
        "face recognition enabled (model={:?}, threshold={}, aligned={})",
        model_kind,
        threshold,
        detector
            .as_ref()
            .map(|d| d.produces_landmarks())
            .unwrap_or(false),
    );

    let mut svc = SqliteFaceRecognition::new(pool, extractor)
        .with_threshold(threshold)
        .with_model_name(match model_kind {
            EmbeddingModel::ArcFace512 => "arcface-512",
            EmbeddingModel::MobileFaceNet128 => "mobilefacenet-128",
        });
    if let Some(d) = detector {
        svc = svc.with_detector(d);
    }
    Some(Arc::new(svc))
}

#[cfg(not(feature = "face-onnx"))]
fn build_face_recognition(
    _data_dir: &std::path::Path,
    _pool: sqlx::Pool<sqlx::Sqlite>,
) -> Option<Arc<dyn pond_core::user_data::ports::face_recognition::FaceRecognition>> {
    None
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
        println!("  4) Exit");
        println!();

        let choice = prompt_nonempty("Choose an option: ")?;

        match choice.trim() {
            "1" => {
                run_chat(None, None, "stdin", None, true, Some("none"), None).await?;
            }
            "2" => {
                let data_dir = default_data_dir();
                let drain = tracing_setup::init_tracing(false, &data_dir);
                run_server(
                    std::path::PathBuf::from("web/dist"),
                    false,
                    false,
                    "goose",
                    None,
                    false,
                    drain,
                )
                .await?;
            }
            "3" => {
                run_status().await?;
            }
            "4" => {
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
                let name = if name.trim().is_empty() {
                    "Goose".to_string()
                } else {
                    name.trim().to_string()
                };
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
                let catalog_models: Vec<_> = model_repo
                    .list_all()
                    .await
                    .unwrap_or_default()
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
                        println!(
                            "  {}) [{}] {} ({}, {} MB)",
                            i + 1,
                            dl,
                            m.name,
                            m.category.as_str(),
                            m.size_mb
                        );
                    }
                    println!();
                    let input =
                        prompt_nonempty("Enter number to select, or type a name directly: ")?;
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
                    if let Some(v) = user_data.get("user_name") {
                        settings.user_name = v.clone();
                    }
                    if let Some(v) = user_data.get("timezone") {
                        settings.timezone = v.clone();
                    }
                    if let Some(v) = user_data.get("prompt_style") {
                        settings.prompt_style = v.clone();
                    }
                    if let Some(v) = user_data.get("assistant_name") {
                        settings.assistant_name = v.clone();
                    }
                    if let Some(v) = user_data.get("voice_wake_word") {
                        settings.voice_wake_word = v.clone();
                    }
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
    device_registry: Arc<
        dyn pond_core::user_data::ports::device_registry::DeviceRegistry + Send + Sync,
    >,
    scheduler: Option<Arc<dyn pond_core::user_data::ports::scheduler::SchedulerPort>>,
    settings_repo: Arc<dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync>,
    memory_repo: Arc<
        dyn pond_core::user_data::ports::memory_repository::MemoryRepository + Send + Sync,
    >,
    embedding_provider: Option<
        Arc<dyn pond_core::models::ports::embedding::EmbeddingProvider + Send + Sync>,
    >,
    skill_repo: Arc<dyn pond_core::user_data::ports::skill::UserSkillRepository + Send + Sync>,
    recipe_repo: Arc<dyn pond_core::user_data::ports::recipe::AgentRecipeRepository + Send + Sync>,
    template_repo: Arc<
        dyn pond_core::user_data::ports::prompt_template::PromptTemplateRepository + Send + Sync,
    >,
    extras_repo: Arc<
        dyn pond_core::user_data::ports::prompt_extra::PromptExtraRepository + Send + Sync,
    >,
    draft_repo: Arc<dyn pond_core::user_data::ports::draft::DraftRepository + Send + Sync>,
    device_control: Arc<
        dyn pond_core::user_data::ports::device_control::DeviceControlPort + Send + Sync,
    >,
    session_storage: Option<Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage>>,
    voice_mode: bool,
) -> (
    Arc<dyn Agent>,
    Option<Arc<dyn pond_core::mcp::ports::extension_manager::ExtensionManagerPort>>,
    Option<Arc<dyn pond_core::mcp::ports::tools::tool_caller::ToolCaller>>,
    Arc<dyn pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort>,
) {
    use pond_adapters_goose::GooseAdapter;
    use pond_adapters_local_inference::ToolCallerEngine;
    use pond_core::mcp::ports::extension_manager::ExtensionManagerPort;
    use pond_core::mcp::ports::tools::tool_caller::ToolCaller;
    use pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort;

    let default_registry: Arc<dyn ToolRegistryPort> =
        Arc::new(pond_core::mcp::services::tool_registry::InMemoryToolRegistry::new());

    // ── PondAgent backend (independent, no Goose dependency) ──────────────
    #[cfg(feature = "pond-agent")]
    if agent_backend == "pond" {
        tracing::info!("Building PondAgent backend (independent, KV-cache enabled)");

        let settings = settings_repo.get().await.unwrap_or_default();

        // Build LlamaCppEngine as the inference provider.
        let engine = match pond_inference::LlamaCppEngine::new(data_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("LlamaCppEngine init failed: {e} — falling back to mock");
                return (Arc::new(MockAgent::new()), None, None, default_registry);
            }
        };

        // Load the configured model.
        let model_id = &settings.chat_model;
        if !model_id.is_empty() {
            if let Err(e) = engine.load_model(model_id, 99, true).await {
                tracing::error!("Failed to load model '{}': {e}", model_id);
                return (Arc::new(MockAgent::new()), None, None, default_registry);
            }
        }

        // Build the McpToolDispatcher for direct MCP tool routing.
        let dispatcher = pond_mcp_server::McpToolDispatcher::new(
            memory_repo.clone(),
            weather,
            scheduler,
            settings_repo.clone(),
            device_registry.clone(),
            skill_repo.clone(),
            recipe_repo.clone(),
            draft_repo,
            embedding_provider,
            device_control.clone(),
        );

        let dispatcher: Arc<dyn pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher> =
            Arc::new(dispatcher);

        // Get tool definitions from the dispatcher for prompt injection.
        let tool_defs: Vec<pond_core::models::ports::inference::ToolDefinition> = dispatcher
            .available_tools()
            .await
            .into_iter()
            .map(|name| pond_core::models::ports::inference::ToolDefinition {
                name: name.clone(),
                description: format!("GIAP tool: {}", name),
                parameters_schema: serde_json::json!({}),
            })
            .collect();

        let ss = session_storage.expect(
            "session_storage is required for pond-agent backend — pass it from the server scope",
        );
        let agent = pond_agent::PondAgent::new(
            Arc::new(engine),
            tool_defs,
            settings_repo.clone(),
            Some(template_repo),
            Some(extras_repo),
            Some(skill_repo),
            device_registry,
            ss,
            Some(dispatcher),
        );

        tracing::info!("PondAgent ready — independent inference with KV-cache reuse");
        return (Arc::new(agent), None, None, default_registry);
    }

    if agent_backend != "goose" {
        return (Arc::new(MockAgent::new()), None, None, default_registry);
    }

    // Build tool-calling specialist (FunctionGemma) if configured.
    let tool_caller: Option<Arc<dyn ToolCaller>> = {
        let settings = settings_repo.get().await.unwrap_or_default();
        match settings.tool_model.as_deref() {
            Some(model_name) if !model_name.is_empty() => {
                match ToolCallerEngine::new(model_name, data_dir).await {
                    Ok(engine) => {
                        tracing::info!("Tool-calling specialist loaded: {}", model_name);
                        Some(Arc::new(engine) as Arc<dyn ToolCaller>)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load tool specialist '{}': {e}", model_name);
                        None
                    }
                }
            }
            _ => None,
        }
    };

    // Register all GIAP MCP servers into Goose's builtin extension registry.
    // Extension toggles (ext_*_enabled) are read from settings to gate registration.
    let settings = settings_repo.get().await.unwrap_or_default();
    match pond_adapters_goose::register_giap_extensions(
        &settings,
        memory_repo.clone(),
        embedding_provider,
        scheduler,
        weather,
        settings_repo.clone(),
        device_registry.clone(),
        skill_repo.clone(),
        recipe_repo.clone(),
        draft_repo,
        device_control,
        tool_caller.clone(),
    ) {
        Ok(ext_names) => {
            tracing::info!(extensions = ?ext_names, "GIAP MCP registration complete");
        }
        Err(e) => {
            tracing::error!("GIAP MCP registration failed: {e} — falling back to mock agent");
            return (Arc::new(MockAgent::new()), None, None, default_registry);
        }
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
        Some(default_registry.clone()),
    )
    .await
    {
        Ok(adapter) => {
            if voice_mode {
                adapter.set_voice_mode(true);
            }
            let ext_mgr: Arc<dyn ExtensionManagerPort> = adapter.extension_manager();
            tracing::info!("Goose agent active — GIAP MCP extension registered");
            let agent: Arc<dyn Agent> = Arc::new(adapter);
            (agent, Some(ext_mgr), tool_caller, default_registry)
        }
        Err(e) => {
            tracing::error!("GooseAdapter init failed: {e} — falling back to mock agent");
            (Arc::new(MockAgent::new()), None, None, default_registry)
        }
    }
}

// ── Model catalog helpers ─────────────────────────────────────────────────────

/// Seed the persistent model catalog from upstream sources (static list + local Ollama).
///
/// Upserts all returned records (preserving `is_custom` rows) and sets `downloaded`
/// by checking the filesystem.  A failure to fetch is non-fatal — the server starts
/// with whatever models are already in the DB.
async fn seed_model_catalog(repo: &dyn ModelRepository, data_dir: &std::path::Path) {
    use crate::composite_model_catalog_provider::CompositeModelCatalogProvider;
    use crate::filesystem_model_storage::FilesystemModelStorage;
    use pond_core::models::ports::model_catalog_provider::ModelCatalogProvider;
    use pond_core::models::ports::model_storage::ModelStorage;

    let client = reqwest::Client::builder()
        .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();
    let provider = CompositeModelCatalogProvider::new(client);
    let storage = FilesystemModelStorage::new(data_dir);

    let (models, _binaries) = match provider.fetch().await {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch model catalog: {e}. Starting with existing DB records."
            );
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
    settings_repo: &dyn pond_core::user_data::ports::settings::SettingsRepository,
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
        let category = a.model_id.split('/').next().unwrap_or("");

        match a.role.as_str() {
            "chat" => {
                let provider = category_to_provider(category);
                let _ = settings_repo.set_key("chat_provider", provider).await;
                let _ = settings_repo
                    .set_key("chat_model", model_name.to_string())
                    .await;
            }
            "think" => {
                let provider = category_to_provider(category);
                let _ = settings_repo.set_key("think_provider", provider).await;
                let _ = settings_repo
                    .set_key("think_model", model_name.to_string())
                    .await;
            }
            "task" => {
                let provider = category_to_provider(category);
                let _ = settings_repo.set_key("task_provider", provider).await;
                let _ = settings_repo
                    .set_key("task_model", model_name.to_string())
                    .await;
            }
            "tool" => {
                let _ = settings_repo
                    .set_key("tool_model", model_name.to_string())
                    .await;
            }
            "asr" => {
                let _ = settings_repo
                    .set_key("active_whisper_model", model_name.to_string())
                    .await;
            }
            "tts" => {
                let _ = settings_repo
                    .set_key("active_tts_model", model_name.to_string())
                    .await;
                // For piper models also sync voice_tts_voice to the .onnx filename.
                if category == "tts_piper" {
                    if let Ok(Some(record)) = repo.get_by_id(&a.model_id).await {
                        if let Some(fname) = record.filename {
                            let _ = settings_repo.set_key("voice_tts_voice", fname).await;
                        }
                    }
                }
            }
            "embedding" => {
                let _ = settings_repo
                    .set_key("active_embedding_model", model_name.to_string())
                    .await;
            }
            other => {
                tracing::debug!("sync_assignments_to_settings: unknown role '{other}', skipping");
            }
        }
    }

    if !assignments.is_empty() {
        tracing::info!(
            "synced {} role assignment(s) to settings KV",
            assignments.len()
        );
    }
}

fn category_to_provider(category: &str) -> String {
    match category {
        "ollama" => "ollama".to_string(),
        "gguf" => "local".to_string(),
        _ => "llamafile".to_string(),
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
                        eprintln!("Unknown category '{cat_str}'. Valid: gguf, llamafile, whisper, tts, ollama, embedding");
                        std::process::exit(1);
                    }
                }
            } else {
                repo.list_all().await?
            };

            let assignments = repo.list_assignments().await.unwrap_or_default();

            println!(
                "{:<12} {:<28} {:>8}  {:>6}  {:>10}  Role",
                "Category", "Name", "Size(MB)", "DL?", "RAM(MB)"
            );
            println!("{}", "─".repeat(78));

            for m in &models {
                let role = assignments
                    .iter()
                    .find(|a| a.model_id == m.id)
                    .map(|a| a.role.as_str())
                    .unwrap_or("—");
                let dl = if m.downloaded { "✓" } else { "✗" };
                let ram = m
                    .ram_estimate_mb
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "—".to_string());
                println!(
                    "{:<12} {:<28} {:>8}  {:>6}  {:>10}  {}",
                    m.category.as_str(),
                    m.name,
                    m.size_mb,
                    dl,
                    ram,
                    role
                );
            }
        }

        ModelAction::Download { category, name } => {
            let cat = ModelCategory::from_str(&category)
                .ok_or_else(|| anyhow::anyhow!("Unknown category '{category}'"))?;
            let id = ModelRecord::id_for(&cat, &name);
            let record = repo
                .get_by_id(&id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Model '{id}' not found in catalog"))?;

            let url = record
                .url
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Model '{id}' has no download URL"))?;
            let filename = record
                .filename
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Model '{id}' has no filename"))?;

            let subdir = match cat {
                ModelCategory::Whisper => "models",
                ModelCategory::Llamafile => "models/llm",
                ModelCategory::Gguf => "models/gguf",
                ModelCategory::TtsPiper => "models/tts",
                _ => "models",
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
            let cat = ModelCategory::from_str(&category)
                .ok_or_else(|| anyhow::anyhow!("Unknown category '{category}'"))?;
            let id = ModelRecord::id_for(&cat, &name);
            let record = repo
                .get_by_id(&id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Model '{id}' not found in catalog"))?;

            let assignments = repo.list_assignments().await?;
            if let Some(a) = assignments.iter().find(|a| a.model_id == id) {
                anyhow::bail!(
                    "Model is assigned to role '{}'. Deactivate it first.",
                    a.role
                );
            }

            if let Some(filename) = &record.filename {
                let subdir = match cat {
                    ModelCategory::Whisper => "models",
                    ModelCategory::Llamafile => "models/llm",
                    ModelCategory::Gguf => "models/gguf",
                    ModelCategory::TtsPiper => "models/tts",
                    _ => "models",
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

        ModelAction::Activate {
            category,
            name,
            role,
        } => {
            let cat = ModelCategory::from_str(&category)
                .ok_or_else(|| anyhow::anyhow!("Unknown category '{category}'"))?;
            let id = ModelRecord::id_for(&cat, &name);
            repo.get_by_id(&id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Model '{id}' not found in catalog"))?;

            use pond_core::models::domain::model_record::ModelRoleAssignment;
            if !ModelRoleAssignment::category_matches_role(&cat, &role) {
                anyhow::bail!(
                    "Category '{}' is not compatible with role '{}'. \
                     (whisper→asr, tts_piper/tts_http→tts, gguf/llamafile/ollama→chat|think|task)",
                    cat.as_str(),
                    role
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

/// Resolve the model role string from a `--role` flag value.
/// All requests default to "chat" — the LLM handles tool routing natively via MCP.
fn resolve_role(role_arg: &str, _message: &str) -> String {
    match role_arg {
        "auto" | "chat" => "chat".to_string(),
        other => other.to_string(),
    }
}

/// Stream a single agent request to stdout, printing tool calls to stderr.
///
/// Text tokens are printed as they arrive. Tool calls and results are shown
/// on stderr so they don't pollute piped output. Returns when the stream ends.
async fn stream_agent_response(
    agent: &Arc<dyn Agent>,
    request: pond_core::shared::domain::agent::AgentRequest,
) -> Result<()> {
    use pond_core::shared::domain::agent::AgentStreamEvent;

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
            AgentStreamEvent::Thinking { content } => {
                eprint!("\r\x1b[K\x1b[2m  💭 {content}\x1b[0m");
                let _ = io::stderr().flush();
            }
            AgentStreamEvent::ReviewStatus { content } => {
                eprint!("\r\x1b[K\x1b[33m  🔍 {content}\x1b[0m");
                let _ = io::stderr().flush();
            }
            AgentStreamEvent::ReviewRevision {
                content,
                score,
                rounds,
            } => {
                // Clear previous answer and print revised version
                eprintln!(
                    "\r\x1b[K\x1b[33m  📝 Revised (score: {score}/5, rounds: {rounds})\x1b[0m"
                );
                println!("{content}");
                printed_newline = content.ends_with('\n');
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
    let settings_repo: Arc<
        dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync,
    > = Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    let memory_repo: Arc<
        dyn pond_core::user_data::ports::memory_repository::MemoryRepository + Send + Sync,
    > = Arc::new(SqliteMemoryRepository::new(db.system.clone()));
    let skill_repo: Arc<dyn pond_core::user_data::ports::skill::UserSkillRepository + Send + Sync> =
        Arc::new(SqliteSkillRepository::new(db.system.clone()));
    let recipe_repo: Arc<
        dyn pond_core::user_data::ports::recipe::AgentRecipeRepository + Send + Sync,
    > = Arc::new(SqliteRecipeRepository::new(db.system.clone()));
    let template_repo: Arc<
        dyn pond_core::user_data::ports::prompt_template::PromptTemplateRepository + Send + Sync,
    > = Arc::new(SqlitePromptTemplateRepository::new(db.system.clone()));
    let extras_repo: Arc<
        dyn pond_core::user_data::ports::prompt_extra::PromptExtraRepository + Send + Sync,
    > = Arc::new(SqlitePromptExtraRepository::new(db.system.clone()));
    let device_registry: Arc<
        dyn pond_core::user_data::ports::device_registry::DeviceRegistry + Send + Sync,
    > = Arc::new(SqliteDeviceRegistry::new(db.system.clone()));
    let draft_repo: Arc<dyn pond_core::user_data::ports::draft::DraftRepository + Send + Sync> =
        Arc::new(SqliteDraftRepository::new(db.system.clone()));

    let settings = settings_repo.get().await.unwrap_or_default();
    // Use the configured LLM server URL (llamafile default). GooseAdapter uses this to
    // route requests when chat_provider = "llamafile"; for ollama/local it uses its own logic.
    let llamafile_url = format!("http://127.0.0.1:{}", ports::LLAMAFILE);

    // Wire weather from settings so giap__get_current_weather MCP tool is available.
    let weather: Option<Arc<dyn WeatherProvider>> = if settings.weather_enabled
        && (settings.weather_latitude != 0.0 || settings.weather_longitude != 0.0)
    {
        let loc = if settings.weather_location_name.is_empty() {
            format!(
                "{:.3}, {:.3}",
                settings.weather_latitude, settings.weather_longitude
            )
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
        AgentAction::Chat {
            message,
            session,
            role,
        } => {
            use pond_core::shared::domain::agent::AgentRequest;

            let model_role = resolve_role(&role, &message);
            eprintln!(
                "  {} | provider: {}  model: {}  role: {}",
                settings.assistant_name, settings.chat_provider, settings.chat_model, model_role
            );

            let (agent, _ext_mgr, _tc, _tr) = build_goose_backend(
                "goose",
                &llamafile_url,
                &data_dir,
                weather,
                device_registry,
                None,
                settings_repo,
                memory_repo,
                None, // embedding_provider — not used in CLI chat
                skill_repo,
                recipe_repo,
                template_repo,
                extras_repo,
                draft_repo,
                Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
                None, // session_storage — not needed for goose backend
                false,
            )
            .await;

            let request = AgentRequest {
                message,
                session_id: session,
                model_role,
                images: Vec::new(),
                voice_mode: false,
                canvas_mode: false,
            };
            stream_agent_response(&agent, request).await?;
        }

        AgentAction::Repl { session, role } => {
            use pond_core::shared::domain::agent::AgentRequest;
            use tokio::io::AsyncBufReadExt as _;

            let (agent, _ext_mgr, _tc, _tr) = build_goose_backend(
                "goose",
                &llamafile_url,
                &data_dir,
                weather,
                device_registry,
                None,
                settings_repo,
                memory_repo,
                None, // embedding_provider — not used in CLI repl
                skill_repo,
                recipe_repo,
                template_repo,
                extras_repo,
                draft_repo,
                Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
                None, // session_storage — not needed for goose backend
                false,
            )
            .await;

            eprintln!(
                "  {} — session: {}  (Ctrl+C or 'exit' to quit)",
                settings.assistant_name, session
            );

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
                    images: Vec::new(),
                    voice_mode: false,
                    canvas_mode: false,
                };
                if let Err(e) = stream_agent_response(&agent, request).await {
                    eprintln!("\n  error: {e}");
                }
            }
        }

        AgentAction::Tools => {
            let (_agent, ext_mgr, _tc, _tr) = build_goose_backend(
                "goose",
                &llamafile_url,
                &data_dir,
                weather,
                device_registry,
                None,
                settings_repo,
                memory_repo,
                None, // embedding_provider — not used in CLI tools listing
                skill_repo,
                recipe_repo,
                template_repo,
                extras_repo,
                draft_repo,
                Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
                None, // session_storage — not needed for goose backend
                false,
            )
            .await;

            match ext_mgr {
                None => println!("No extension manager available (agent backend may be 'mock')."),
                Some(mgr) => {
                    let extensions = mgr.list_extensions().await.unwrap_or_default();
                    if extensions.is_empty() {
                        println!(
                            "No extensions loaded yet (start the server to initialise sessions)."
                        );
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
            println!(
                "{:<4} {:<20} {:<6} {}",
                "Ord", "Key", "Active", "Instruction"
            );
            println!("{}", "─".repeat(72));
            for e in &extras {
                let active = if e.active { "✓" } else { "✗" };
                let preview = if e.instruction.len() > 40 {
                    format!("{}…", &e.instruction[..39])
                } else {
                    e.instruction.clone()
                };
                println!(
                    "{:<4} {:<20} {:<6} {}",
                    e.sort_order, e.key, active, preview
                );
            }
        }
    }

    Ok(())
}

// ── Prompts CLI ───────────────────────────────────────────────────────────────

async fn run_prompts_cmd(action: PromptAction) -> Result<()> {
    use pond_core::user_data::ports::prompt_template::PromptTemplateRepository as _;

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

        PromptAction::Show { name } => match repo.get(&name).await? {
            None => {
                eprintln!("Template '{name}' not found.");
                std::process::exit(1);
            }
            Some(t) => {
                println!("─── {} ─── (system={})", t.name, t.is_system);
                println!("{}", t.content);
            }
        },

        PromptAction::Reset { name } => {
            use pond_core::prompts::{
                PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM,
            };
            use pond_core::user_data::domain::prompt_template::PromptTemplate;

            let (content, description) = match name.as_str() {
                "balanced" => (
                    PROMPT_BALANCED,
                    "Warm, practical, complete behaviour rules. Default for most households.",
                ),
                "concise" => (
                    PROMPT_CONCISE,
                    "Minimal, action-first. For power users who want brevity.",
                ),
                "technical" => (
                    PROMPT_TECHNICAL,
                    "Verbose, tool-aware, narrates reasoning. For developers.",
                ),
                "warm" => (
                    PROMPT_WARM,
                    "Conversational, family-friendly, personality-forward.",
                ),
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
    use pond_core::user_data::ports::skill::UserSkillRepository as _;

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
                let hint = if all {
                    ""
                } else {
                    " (use --all to include inactive)"
                };
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
            use pond_core::user_data::domain::skill::UserSkill;

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
            use pond_core::user_data::ports::skill::UserSkillRepository as _;

            let skill = repo
                .get(&id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Skill '{id}' not found"))?;
            let updated = pond_core::user_data::domain::skill::UserSkill {
                active: !skill.active,
                ..skill.clone()
            };
            repo.update(&updated).await?;
            let state = if updated.active {
                "enabled"
            } else {
                "disabled"
            };
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
    use pond_core::user_data::ports::recipe::AgentRecipeRepository as _;

    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;
    let repo = SqliteRecipeRepository::new(db.system.clone());

    match action {
        RecipeAction::List => {
            let recipes = repo.list().await?;
            if recipes.is_empty() {
                println!(
                    "No recipes found. Import one with `pond recipes import <name> <file.yaml>`"
                );
                return Ok(());
            }
            println!(
                "{:<38} {:<6} {:<20} {}",
                "ID", "Active", "Name", "Description"
            );
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

        RecipeAction::Show { name } => match repo.get_by_name(&name).await? {
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
        },

        RecipeAction::Import {
            name,
            file,
            description,
        } => {
            use pond_core::user_data::domain::recipe::AgentRecipe;

            let yaml = tokio::fs::read_to_string(&file)
                .await
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

        RecipeAction::Remove { name } => match repo.get_by_name(&name).await? {
            None => {
                eprintln!("Recipe '{name}' not found.");
                std::process::exit(1);
            }
            Some(r) => {
                repo.delete(&r.id).await?;
                println!("✓ Recipe '{name}' deleted.");
            }
        },
    }

    Ok(())
}

// ── Memories CLI ──────────────────────────────────────────────────────────────

async fn run_memories_cmd(action: MemoryAction) -> Result<()> {
    use pond_core::user_data::ports::memory_repository::MemoryRepository as _;

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
            use pond_core::user_data::domain::memory::MemoryFragment;

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
                segment: None,
                importance: None,
                tier: None,
                decay_rate: None,
                access_count: 0,
                last_accessed_at: None,
                lifecycle: None,
                superseded_by: None,
                corrects: None,
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
        assert_eq!(category_to_provider("whisper"), "llamafile");
        assert_eq!(category_to_provider("unknown"), "llamafile");
    }

    // ── sync_assignments_to_settings ──────────────────────────────────────────

    #[tokio::test]
    async fn sync_ollama_chat_assignment_updates_settings() {
        use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
        use pond_core::models::ports::model_repository::ModelRepository;
        use pond_core::user_data::ports::settings::SettingsRepository;
        use pond_infra::db::Database;
        use pond_infra::sqlite_model_repository::SqliteModelRepository;
        use pond_infra::sqlite_settings::SqliteSettingsRepository;

        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let repo = SqliteModelRepository::new(db.system.clone());
        let settings_repo = SqliteSettingsRepository::new(db.system.clone());

        let model = ModelRecord {
            id: "ollama/llama3.2".to_string(),
            category: ModelCategory::Ollama,
            name: "llama3.2".to_string(),
            filename: None,
            description: "Ollama Llama 3.2".to_string(),
            size_mb: 0,
            url: None,
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: None,
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: true,
            is_custom: false,
        };
        repo.upsert(&model).await.unwrap();
        repo.set_assignment("chat", "ollama/llama3.2")
            .await
            .unwrap();

        sync_assignments_to_settings(&repo, &settings_repo).await;

        let settings = settings_repo.get().await.unwrap();
        assert_eq!(settings.chat_provider, "ollama");
        assert_eq!(settings.chat_model, "llama3.2");
    }

    #[tokio::test]
    async fn sync_gguf_chat_assignment_sets_local_provider() {
        use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
        use pond_core::models::ports::model_repository::ModelRepository;
        use pond_core::user_data::ports::settings::SettingsRepository;
        use pond_infra::db::Database;
        use pond_infra::sqlite_model_repository::SqliteModelRepository;
        use pond_infra::sqlite_settings::SqliteSettingsRepository;

        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let repo = SqliteModelRepository::new(db.system.clone());
        let settings_repo = SqliteSettingsRepository::new(db.system.clone());

        let model = ModelRecord {
            id: "gguf/llama-3b".to_string(),
            category: ModelCategory::Gguf,
            name: "llama-3b".to_string(),
            filename: Some("llama-3b.gguf".to_string()),
            description: "GGUF Llama 3B".to_string(),
            size_mb: 2000,
            url: Some("https://example.com/llama-3b.gguf".to_string()),
            hf_id: None,
            ram_estimate_mb: Some(3000),
            recommended_role: None,
            context_length: None,
            quantization: Some("Q4_K_M".to_string()),
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: false,
            is_custom: false,
        };
        repo.upsert(&model).await.unwrap();
        repo.set_assignment("chat", "gguf/llama-3b").await.unwrap();

        sync_assignments_to_settings(&repo, &settings_repo).await;

        let settings = settings_repo.get().await.unwrap();
        assert_eq!(
            settings.chat_provider, "local",
            "gguf category should map to 'local' provider"
        );
        assert_eq!(
            settings.chat_model, "llama-3b",
            "model name should be extracted from id"
        );
    }

    #[tokio::test]
    async fn sync_think_and_task_roles_are_also_synced() {
        use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
        use pond_core::models::ports::model_repository::ModelRepository;
        use pond_core::user_data::ports::settings::SettingsRepository;
        use pond_infra::db::Database;
        use pond_infra::sqlite_model_repository::SqliteModelRepository;
        use pond_infra::sqlite_settings::SqliteSettingsRepository;

        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let repo = SqliteModelRepository::new(db.system.clone());
        let settings_repo = SqliteSettingsRepository::new(db.system.clone());

        let model = ModelRecord {
            id: "ollama/gemma2".to_string(),
            category: ModelCategory::Ollama,
            name: "gemma2".to_string(),
            filename: None,
            description: String::new(),
            size_mb: 0,
            url: None,
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: None,
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: true,
            is_custom: false,
        };
        repo.upsert(&model).await.unwrap();
        repo.set_assignment("think", "ollama/gemma2").await.unwrap();
        repo.set_assignment("task", "ollama/gemma2").await.unwrap();

        sync_assignments_to_settings(&repo, &settings_repo).await;

        // think_provider/think_model and task_provider/task_model are KV-only
        // (not first-class fields on Settings), so read via get_key().
        assert_eq!(
            settings_repo
                .get_key("think_provider")
                .await
                .unwrap()
                .as_deref(),
            Some("ollama"),
        );
        assert_eq!(
            settings_repo
                .get_key("think_model")
                .await
                .unwrap()
                .as_deref(),
            Some("gemma2"),
        );
        assert_eq!(
            settings_repo
                .get_key("task_provider")
                .await
                .unwrap()
                .as_deref(),
            Some("ollama"),
        );
        assert_eq!(
            settings_repo
                .get_key("task_model")
                .await
                .unwrap()
                .as_deref(),
            Some("gemma2"),
        );
    }

    #[tokio::test]
    async fn sync_tool_assignment_updates_settings() {
        use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
        use pond_core::models::ports::model_repository::ModelRepository;
        use pond_core::user_data::ports::settings::SettingsRepository;
        use pond_infra::db::Database;
        use pond_infra::sqlite_model_repository::SqliteModelRepository;
        use pond_infra::sqlite_settings::SqliteSettingsRepository;

        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let repo = SqliteModelRepository::new(db.system.clone());
        let settings_repo = SqliteSettingsRepository::new(db.system.clone());

        let model = ModelRecord {
            id: "ollama/gemma3:4b".to_string(),
            category: ModelCategory::Ollama,
            name: "gemma3:4b".to_string(),
            filename: None,
            description: String::new(),
            size_mb: 0,
            url: None,
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: None,
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: true,
            is_custom: false,
        };
        repo.upsert(&model).await.unwrap();
        repo.set_assignment("tool", "ollama/gemma3:4b")
            .await
            .unwrap();

        sync_assignments_to_settings(&repo, &settings_repo).await;

        assert_eq!(
            settings_repo
                .get_key("tool_model")
                .await
                .unwrap()
                .as_deref(),
            Some("gemma3:4b"),
            "tool_model should be synced from tool role assignment"
        );
    }
}
