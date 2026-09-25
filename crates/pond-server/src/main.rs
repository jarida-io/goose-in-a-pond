//! Goose In A Pond server entry point.
//!
//! TODO: `scripts/setup.sh`: dedicated vs shared host URL, port 80→8080→4000→5000, mDNS, systemd.

mod asset_root;
mod composite_model_catalog_provider;
mod filesystem_model_storage;
mod http_model_downloader;
mod inference_lane_runner;
mod kokoro_control;
mod llamafile_process;
mod llm_memory_consolidator;
mod llm_memory_extractor;
mod mdns_advertiser;
mod model_download;
mod node_path;
mod ports;
mod reqwest_model_downloader;
mod schedule_executors;
mod startup;
mod system_deps;
mod three_stage_consolidator;
mod tracing_setup;
mod voice_lock;
mod voice_models;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt as _;
use pond_adapters_llamafile::LlamafileProvider;
#[cfg(feature = "mesh")]
use pond_adapters_mesh_libp2p::{Libp2pMeshTransport, Libp2pMeshTransportConfig};
use pond_adapters_ollama::OllamaProvider;
use pond_adapters_weather::{OpenMeteoWeatherAdapter, WeatherProvider};
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
use pond_core::shared::services::print_output::{PrintOutput, SilentOutput};
use pond_core::shared::services::stdin_input::StdinInput;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository as _;
use pond_core::user_data::services::onboarding::OnboardingService;
use pond_infra::db::Database;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_event_log::{SqliteEventLog, SqliteOperationalLog};
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
        /// Path to the built web dashboard assets (run `cd pond-desktop && npm run build` first)
        #[arg(long, default_value = "pond-desktop/dist")]
        static_dir: std::path::PathBuf,

        /// Open the dashboard in the browser
        #[arg(long)]
        open: bool,

        /// Enable debug logging
        #[arg(long)]
        debug: bool,

        /// Agent backend: "goose" (default, full-featured) | "mistralrs" (direct
        /// path to a mistral.rs server, needs --features mistralrs-agent) |
        /// "pond" (quarantined) | "mock".
        /// Also configurable via PUT /api/v1/settings with agent_backend field.
        #[arg(long, default_value = "goose")]
        agent: String,

        /// Port to listen on (defaults to 4000)
        #[arg(long)]
        port: Option<u16>,

        /// Also launch the native desktop app after the server starts.
        /// macOS only. Looks for an installed app in /Applications, then a
        /// locally packaged one under pond-desktop/release/, then the dev
        /// Electron runtime. On Linux it says so and keeps serving: the UI
        /// there is this server's own dashboard.
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

        /// Listen on the microphone instead of the keyboard.
        ///
        /// One flag turns on the whole stack — wake word, speech detection,
        /// recognition, spoken reply — and fetches whatever part of it is not
        /// on disk yet. It replaces `--input stdin|whisper`, which asked the
        /// operator to name a component in order to choose a mode.
        #[arg(long)]
        voice: bool,

        /// Enable voice-based wake word detection (requires --voice).
        /// Say the trigger phrase to activate the assistant before each turn.
        /// Defaults to the wake word stored in Settings.
        #[arg(long)]
        wake_word: Option<String>,

        /// Disable wake word detection (jump straight to listen on each turn).
        #[arg(long)]
        no_wake_word: bool,

        /// Text-to-speech engine: kokoro, or none (print only).
        /// Defaults to kokoro, which is the only engine there is — the help
        /// said `piper` for two releases after Piper stopped being wired.
        #[arg(long)]
        tts: Option<String>,

        /// Session id for conversation continuity + history. Defaults to
        /// "default-session". The desktop shell passes a per-session uuid so
        /// the chat sidebar can read history via GET /api/v1/sessions/{id}/messages.
        #[arg(long)]
        session_id: Option<String>,

        /// Emit the workflow as newline-delimited JSON (NDJSON) on stdout, one
        /// event per line. In this mode stdout carries NOTHING but JSON lines
        /// (no banners, prompts, or emoji — those go to stderr); the desktop shell
        /// parses these lines to drive the desktop voice UI.
        #[arg(long)]
        json_events: bool,
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

    /// Show or refresh the device pairing code.
    ///
    /// Prints the current pairing code (if one is still valid) or issues a
    /// fresh 6-digit code the operator can enter into the Goose On The Go app.
    /// Also prints a QR code the phone can scan to complete pairing.
    Pairing {
        /// Force a fresh code even if one is still active
        #[arg(long)]
        refresh: bool,
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
        /// Unique skill name (e.g. "Morning Briefing", "Light Control")
        name: String,
        /// Short description of what the skill does and when it applies.
        /// Always visible to the model, so keep it brief.
        #[arg(long, default_value = "")]
        description: String,
        /// Icon key from the desktop app's skill-icon set (cosmetic only).
        #[arg(long, default_value = "sparkles")]
        icon: String,
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
    // Before the runtime (env mutation must be single-threaded) and any child spawn: a GUI
    // launch inherits launchd's bare PATH, which hides nvm's node.
    node_path::ensure_node_on_path();
    // Both rustls backends are on (`aws_lc_rs` from the root Cargo.toml, `ring` via reqwest), so
    // rustls panics wherever it infers one; pin it. `Err` means one is already installed: fine.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let num_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    // ≤6 cores (Jetson Orin Nano): cap at 4 workers to leave headroom for OS + audio.
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

/// Sets `GOOSE_PATH_ROOT` so Goose's engine state lives under the pond data dir, not a shared
/// Goose app dir. Must run before `SESSION_STORAGE` (a `LazyLock`) latches its path.
fn pin_goose_state_under(data_dir: &std::path::Path) {
    // Goose's `validated_path_root` silently ignores a relative path.
    let root = match std::fs::canonicalize(data_dir) {
        Ok(abs) => abs,
        Err(_) => {
            // Missing on a first run; Goose only needs the path absolute.
            if data_dir.is_absolute() {
                data_dir.to_path_buf()
            } else {
                match std::env::current_dir() {
                    Ok(cwd) => cwd.join(data_dir),
                    Err(e) => {
                        tracing::warn!(
                            "could not make {} absolute ({e}); goose state stays in its own app dir",
                            data_dir.display()
                        );
                        return;
                    }
                }
            }
        }
    };
    let engine_root = root.join("engine");
    if let Err(e) = std::fs::create_dir_all(&engine_root) {
        tracing::warn!(
            "could not create {} ({e}); goose state stays in its own app dir",
            engine_root.display()
        );
        return;
    }
    std::env::set_var("GOOSE_PATH_ROOT", &engine_root);
}

async fn async_main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = default_data_dir();
    pin_goose_state_under(&data_dir);

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
            voice,
            wake_word,
            no_wake_word,
            tts,
            session_id,
            json_events,
        }) => {
            // --json-events: stdout carries only NDJSON, so diagnostics go to stderr.
            let console = if json_events {
                tracing_setup::ConsoleSink::Stderr
            } else {
                tracing_setup::ConsoleSink::Stdout
            };
            // Console: WARN+ only (turn lines use diag!/out!); the log file keeps full detail.
            let _log = tracing_setup::init_tracing_with_console(false, &data_dir, console, true);
            run_chat(
                provider.as_deref(),
                model.as_deref(),
                voice,
                wake_word.as_deref(),
                no_wake_word,
                tts.as_deref(),
                session_id.as_deref(),
                json_events,
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
        Some(Commands::Pairing { refresh }) => run_pairing(refresh).await,
        None => {
            // No subcommand: interactive text chat, provider from Settings.
            let _log = tracing_setup::init_tracing(false, &data_dir);
            run_chat(None, None, false, None, true, Some("none"), None, false).await
        }
    }
}

async fn run_setup(model: &str) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose In A Pond — Setup         ║");
    println!("  ╚═══════════════════════════════════════╝");

    // Before any model loads, so this line isn't buried under provider-init noise.
    report_acceleration();

    let data_dir = default_data_dir();

    println!("\n  📂 Data directory: {}", data_dir.display());

    println!("\n  [1/8] Checking system dependencies...");
    if system_deps::ensure_system_deps().await {
        println!("  ✅ System dependencies OK");
    } else {
        println!(
            "  ⚠  Some system deps could not be installed — see docs/developer/linux-setup.md"
        );
        println!("     Continuing setup; some features may not work until deps are installed.");
    }

    println!("\n  [2/8] Initializing databases...");
    let db_setup = Database::init(&data_dir).await?;
    println!("  ✅ Databases ready");

    // Install the stored network mode before the first download so `offline` is honoured;
    // after DB init because the setting lives there.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(
            &SqliteSettingsRepository::new(db_setup.system.clone())
                .get()
                .await
                .unwrap_or_default()
                .network_mode,
        ),
    );

    // Moves flat model files into the content-addressed blob layout; idempotent, never fatal.
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
    let settings_repo_setup = SqliteSettingsRepository::new(db_setup.system.clone());
    println!("  📋 Fetching model catalog from upstream sources...");
    seed_model_catalog(&setup_model_repo, &data_dir).await;
    ensure_tts_is_set_up(&setup_model_repo, &settings_repo_setup).await;

    {
        use pond_core::prompts::BUILTIN_PROMPT_TEMPLATES;
        use pond_core::user_data::domain::prompt_template::PromptTemplate;
        #[allow(unused_imports)]
        use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;

        let template_repo = SqlitePromptTemplateRepository::new(db_setup.system.clone());
        for &(name, content, description) in BUILTIN_PROMPT_TEMPLATES {
            let t = PromptTemplate {
                name: name.to_string(),
                content: content.to_string(),
                description: description.to_string(),
                is_system: true,
                is_customized: false,
                factory_version: pond_core::user_data::domain::prompt_template::FACTORY_VERSION,
                updated_at: String::new(),
            };
            if let Err(e) = template_repo.insert_if_absent(&t).await {
                println!("  ⚠  Failed to seed prompt template '{name}': {e}");
            }
        }
        println!("  ✅ Prompt templates seeded");
    }

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
        "\n  [3/8] Downloading Whisper ASR model ({})...",
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

    {
        println!("\n  [4/8] Whisper runs in-process — no binary download needed.");
    }

    {
        println!("\n  [5/8] Checking Piper TTS voice model...");
        let tts_dir = model_download::tts_models_dir(&data_dir);
        let has_voice_model = std::fs::read_dir(&tts_dir)
            .ok()
            .map(|mut d| {
                d.any(|e| {
                    e.ok()
                        .and_then(|e| e.path().extension().map(|x| x == "onnx"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if has_voice_model {
            println!("  ✅ Piper voice model found.");
        } else {
            println!("  ⚠  No Piper voice model downloaded yet.");
            println!("     Go to Settings → Voice in the web UI to download a voice.");
        }
    }

    println!("\n  [6/8] Checking ONNX Runtime...");
    ensure_onnx_runtime();
    match std::env::var("ORT_DYLIB_PATH") {
        Ok(p) => println!("  ✅ ONNX Runtime: {}", p),
        Err(_) => {
            println!("  ⚠  ONNX Runtime not found — Piper TTS and face recognition will not work.");
            println!("     Install manually: brew install onnxruntime  (macOS)");
            println!("     or: apt install libonnxruntime-dev  (Linux)");
        }
    }

    // piper-rs needs espeak-ng-data for phonemization.
    println!("\n  [7/8] Checking espeak-ng-data...");
    {
        let espeak_path = data_dir.join("bin").join("espeak-ng-data");
        if espeak_path.exists() {
            println!("  ✅ espeak-ng-data found at {}", espeak_path.display());
        } else {
            let brew_path = std::path::Path::new("/opt/homebrew/share/espeak-ng-data");
            let usr_path = std::path::Path::new("/usr/share/espeak-ng-data");
            let source = if brew_path.exists() {
                Some(brew_path)
            } else if usr_path.exists() {
                Some(usr_path)
            } else {
                None
            };
            if let Some(src) = source {
                if let Some(parent) = espeak_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                #[cfg(unix)]
                match std::os::unix::fs::symlink(src, &espeak_path) {
                    Ok(_) => println!("  ✅ Created espeak-ng-data symlink → {}", src.display()),
                    Err(e) => println!("  ⚠  Could not symlink espeak-ng-data: {e}"),
                }
                #[cfg(not(unix))]
                println!("  ⚠  espeak-ng-data not found at expected path. Install espeak-ng.");
            } else {
                println!("  ⚠  espeak-ng-data not found. Piper TTS will fail.");
                println!("     Run: brew install espeak-ng");
            }
        }
    }

    println!("\n  [8/8] Checking microphone access...");
    {
        use cpal::traits::HostTrait;
        let host = cpal::default_host();
        match host.default_input_device() {
            Some(dev) => {
                use cpal::traits::DeviceTrait;
                let name = dev.name().unwrap_or_else(|_| "unknown".to_string());
                println!("  ✅ Microphone available: {}", name);
            }
            None => {
                println!("  ⚠  No microphone input device found.");
                #[cfg(target_os = "macos")]
                {
                    println!("     On macOS this usually means microphone permission is denied.");
                    println!("     Go to System Settings → Privacy & Security → Microphone");
                    println!("     and enable access for your terminal app.");
                }
                #[cfg(target_os = "linux")]
                println!(
                    "     Check that ALSA or PulseAudio is configured and a mic is connected."
                );
            }
        }
    }

    #[cfg(feature = "face-onnx")]
    {
        println!("\n  [9/9] Setting up face recognition models...");
        if let Err(e) = model_download::download_face_models(&data_dir).await {
            println!("  ⚠  Face model setup failed: {} — face recognition will be disabled until you add the files manually", e);
        }
    }

    // Pre-generate the mesh keypair so the first enable doesn't pay the generation cost.
    #[cfg(feature = "mesh")]
    {
        use pond_mesh_protocol::identity::MeshKeypair;
        println!("\n  Setting up private mesh identity...");
        match settings_repo_setup.get_key("mesh_identity_secret").await {
            Ok(Some(_)) => println!("  ✅ Mesh identity already set up"),
            _ => {
                let keypair = MeshKeypair::generate();
                let hex = hex_encode_32(&keypair.secret_bytes());
                match settings_repo_setup
                    .set_key("mesh_identity_secret", hex)
                    .await
                {
                    Ok(()) => println!(
                        "  ✅ Mesh identity ready — peer_id={}\n     Enable it later via Settings → Mesh once you have a trusted peer to pair with.",
                        keypair.peer_id()
                    ),
                    Err(e) => println!(
                        "  ⚠  Failed to persist mesh identity: {} — it will be generated on first enable instead",
                        e
                    ),
                }
            }
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
    println!("       pond-server chat --voice");
    println!("  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    Ok(())
}

// ── LlamafileManager implementation ──────────────────────────────────────────

/// Owns the llamafile child process so `pond-api` can start it without knowing how.
struct LlamafileManagerImpl {
    data_dir: std::path::PathBuf,
    model_service: Arc<pond_core::models::services::model_service::ModelService>,
    /// Holds the spawned process guard so it stays alive as long as AppState does.
    guard: Arc<tokio::sync::Mutex<Option<llamafile_process::LlamafileProcess>>>,
    /// Port the process bound to; may differ from the base port, updated by the spawn task.
    actual_port: Arc<std::sync::atomic::AtomicU16>,
}

#[async_trait::async_trait]
impl LlamafileManager for LlamafileManagerImpl {
    async fn ensure_started(&self, model_name: Option<&str>) -> String {
        let effective = self.effective_port();

        if llamafile_process::is_running(effective).await {
            return llamafile_process::url_for(effective);
        }

        // Background spawn: model load takes 5–30 s and must not delay the settings-save response.
        let data_dir = self.data_dir.clone();
        let model_service = self.model_service.clone();
        let model_hint = model_name.map(|s| s.to_string());
        let guard_arc = Arc::clone(&self.guard);
        let actual_port_arc = Arc::clone(&self.actual_port);

        tokio::spawn(async move {
            // Re-check under the lock: two concurrent callers can both miss the fast path.
            let mut guard = guard_arc.lock().await;
            let cur = actual_port_arc.load(std::sync::atomic::Ordering::Acquire);
            if llamafile_process::is_running(cur).await {
                return;
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

        if llamafile_process::is_running(port).await {
            return (url, true);
        }

        self.ensure_started(model_name).await;

        // Re-read the port each poll: a just-started process may bind a different one.
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
    /// Seeds `actual_port` with the startup process's port, or the base port if none started.
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

    fn effective_port(&self) -> u16 {
        self.actual_port.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// First index pass waits for boot to settle, so backfill never slows the first turn.
const INDEX_MAINTENANCE_DELAY_SECS: u64 = 60;

/// How often a pass is considered after that; the lane decides whether one runs.
const INDEX_MAINTENANCE_POLL_SECS: u64 = 15 * 60;

/// Logs an error when this build can't use the host's GPU; a CPU build is silently ~30x slower.
fn report_acceleration() {
    use pond_core::models::domain::acceleration::{classify, host_is_accelerated, warning};

    // `cuda` is a feature of `pond-adapters-local-inference`; a `cfg!` here is always false.
    #[cfg(feature = "local-inference")]
    let cuda_build = pond_adapters_local_inference::CUDA_ENABLED;
    #[cfg(not(feature = "local-inference"))]
    let cuda_build = false;

    // A device profile stands in for the host only when POND_DEVICE_PROFILE is set (never in prod).
    let profile = pond_core::models::domain::device_profile::active();

    let probed = std::fs::read_to_string("/proc/device-tree/model").ok();
    // The device tree pads with NULs, which would defeat a `contains`.
    let probed = probed.as_deref().map(|m| m.trim_end_matches('\0').trim());

    let (model, tegra_release) = match profile {
        Some(p) => (p.device_tree_model.as_deref(), p.has_tegra_release),
        None => (
            probed,
            std::path::Path::new("/etc/nv_tegra_release").exists(),
        ),
    };
    let accelerated = host_is_accelerated(model, tegra_release);
    // OR-ed, not substituted: a profile must never hide a real CUDA build.
    let cuda_build = cuda_build || profile.is_some_and(|p| p.pretend_cuda);

    match warning(classify(accelerated, cuda_build)) {
        Some(w) => tracing::error!("{w}"),
        None => tracing::debug!(
            cuda_build,
            accelerated_host = accelerated,
            "acceleration check passed"
        ),
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
    use pond_core::user_data::ports::matter_runtime::MatterRuntimePort;

    println!("  ╔═══════════════════════════════════════╗");
    println!(
        "  ║   🦆  Goose In A Pond  v{}         ║",
        env!("CARGO_PKG_VERSION")
    );
    println!("  ╚═══════════════════════════════════════╝");

    // Face-recognition env defaults, each set only where unset so it stays overridable.
    apply_face_recognition_defaults();

    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;

    // Idempotent (one stat() once migrated); per-file errors are logged, never fatal.
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

    system_deps::warn_if_missing();

    // ── Load settings early (drives model selection) ─────────────────────────
    let settings_repo_early = SqliteSettingsRepository::new(db.system.clone());
    let settings = settings_repo_early.get().await.unwrap_or_default();

    // The egress gate is process-global: install it before any adapter can call out.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(&settings.network_mode),
    );

    // After `set_network_mode` (it may download ~100 MB from github.com) and before any
    // ONNX-dependent init (face recognition, embeddings).
    ensure_onnx_runtime();

    // The stored setting wins unless the CLI flag names a backend other than the "goose" default.
    let agent_backend = if agent_backend == "goose" && !settings.agent_backend.is_empty() {
        &settings.agent_backend
    } else {
        agent_backend
    };

    // pond-agent is quarantined. Heal a stored "pond" too: the UI PUTs the whole settings
    // object, so that row would trip the 422 quarantine guard on every save.
    let agent_backend: &str = if agent_backend == "pond" {
        tracing::warn!(
            "pond-agent backend is quarantined (not production-ready); \
             falling back to goose and repairing the stored setting."
        );
        let mut healed = settings.clone();
        healed.agent_backend = "goose".to_string();
        if let Err(e) = settings_repo_early.update(&healed).await {
            tracing::warn!("could not persist agent_backend repair: {e}");
        }
        "goose"
    } else {
        agent_backend
    };

    // ── Component startup: auto-download + wire critical services ────────────
    println!("\n  ── Components ──────────────────────────────────────");

    // Apply the mic privacy setting before anything can open a device.
    pond_core::models::domain::mic_gate::set_mic_enabled(settings.mic_enabled);

    // Serve never opens the mic (only `/transcribe`), but `WhisperRsInput::new` needs a handle.
    let (mic_handle, _mic_owner_join) = pond_audio::spawn(
        Box::new(pond_audio::CpalCapture::new()),
        pond_audio::CAPTURE_RATE_HZ,
        15_000,
        settings.mic_enabled,
    );

    // One resolution for both voice models, shared with the `chat` path.
    let voice_models = voice_models::resolve_voice_models(
        &settings,
        &SqliteModelRepository::new(db.system.clone()),
        &data_dir,
    )
    .await;

    let whisper_model_path: Option<std::path::PathBuf> = match voice_models.whisper.as_ref() {
        None => {
            if settings.active_whisper_model.is_empty() {
                println!("  ⏭  STT: whisper skipped (no whisper model configured in Settings)");
            } else {
                println!(
                    "  ⚠  STT: '{}' matches no catalog entry or file on disk — transcription disabled",
                    settings.active_whisper_model
                );
            }
            None
        }
        Some(w) => {
            if !w.path.exists() {
                println!("  📥 STT model not found — downloading...");
                match w.download.as_ref() {
                    Some(dl) => {
                        if let Some(parent) = w.path.parent() {
                            let _ = tokio::fs::create_dir_all(parent).await;
                        }
                        if let Err(e) =
                            model_download::download_file(&dl.url, &w.path, dl.size_mb).await
                        {
                            println!("  ⚠  STT model download failed: {}", e);
                        }
                    }
                    None => println!("  ⚠  STT model has no catalog download URL"),
                }
            }
            Some(w.path.clone())
        }
    };

    // Transcription is in-process; this URL only reaches a deliberately configured whisper.cpp.
    const DEFAULT_WHISPER_URL: &str = "http://127.0.0.1:9000";
    let whisper_url = if settings.voice_whisper_url.is_empty() {
        DEFAULT_WHISPER_URL.to_string()
    } else {
        settings.voice_whisper_url.clone()
    };

    // A closure, so AppState holds in-process whisper without depending on pond-adapters-whisper.
    let transcribe_audio: Option<Arc<dyn Fn(Vec<u8>) -> anyhow::Result<String> + Send + Sync>> =
        whisper_model_path.as_ref().and_then(|p| {
            match WhisperRsInput::new(p.clone(), mic_handle.clone()) {
                Ok(w) => {
                    let w = Arc::new(w);
                    Some(Arc::new(move |wav_bytes: Vec<u8>| {
                        w.transcribe_wav_bytes(&wav_bytes)
                    }) as Arc<dyn Fn(Vec<u8>) -> anyhow::Result<String> + Send + Sync>)
                }
                Err(e) => {
                    tracing::warn!("in-process whisper init failed, transcribe route will proxy to external: {e}");
                    None
                }
            }
        });

    // Unconditional: Kokoro phonemizes through espeak-ng too.
    model_download::ensure_espeak_ng_data(&data_dir).await;

    // espeak-rs phoneme tables; the `PIPER_` env var name is what the espeak-rs crate reads.
    let espeak_data = {
        let p = model_download::piper_espeak_data_path(&data_dir);
        if p.exists() {
            Some(p)
        } else {
            None
        }
    };

    // Only feeds the status report's `piper_http_port` field, which is always absent.
    let piper_http_port: Option<u16> = None;

    // Shared with the TTS control so voice/tier fetches show in the Models page progress feed.
    let download_tracker: std::sync::Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, pond_api::DownloadEntry>>,
    > = std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));

    // ── Kokoro: the TTS engine ──
    // Adopt the host's tier, then resolve a usable one, before fetching: an unusable tier must
    // never be downloaded. The ~92 MB weights load on the first utterance, not in `new`.
    ensure_host_tts_tier(&settings_repo_early).await;
    let tier = settings_repo_early
        .get()
        .await
        .map(|s| s.voice_tts_quality)
        .unwrap_or_else(|_| settings.voice_tts_quality.clone());

    let quality = pond_adapters_kokoro::usable_quality(&tier).to_string();
    model_download::ensure_kokoro_engine(&data_dir, &quality, &settings.voice_tts_voice).await;

    let kokoro_dir = model_download::kokoro_dir(&data_dir);
    let kokoro_engine: Option<Arc<pond_adapters_kokoro::KokoroOutput>> = {
        let quality = quality.as_str();
        let model_path = kokoro_dir.join(pond_adapters_kokoro::model_filename(quality));
        let tokenizer_path = kokoro_dir.join("tokenizer.json");
        if !tokenizer_path.exists() {
            None
        } else {
            let cfg = pond_adapters_kokoro::KokoroConfig {
                model_path,
                voices_dir: kokoro_dir.join("voices"),
                tokenizer_path,
                // Bounded so ONNX Runtime doesn't starve the LLM; see `default_intra_threads`.
                intra_threads: Some(pond_adapters_kokoro::default_intra_threads()),
                espeak_data: espeak_data.clone(),
            };
            match pond_adapters_kokoro::KokoroOutput::new(cfg) {
                Ok(out) => {
                    // Voice and pace are hot — neither touches the session.
                    let voice = settings.voice_tts_voice.trim();
                    if !voice.is_empty() && out.set_voice(voice).await.is_err() {
                        // Heal a stale (e.g. Piper) name to the voice in use, so the
                        // picker is honest and this warns only once.
                        let actual = out.voice().await;
                        tracing::warn!(
                            configured = voice,
                            using = %actual,
                            "configured Kokoro voice is not installed; \
                             rewriting voice_tts_voice to the voice in use"
                        );
                        if let Err(e) = settings_repo_early
                            .set_key("voice_tts_voice", actual.clone())
                            .await
                        {
                            tracing::warn!("could not heal voice_tts_voice: {e}");
                        }
                    }
                    out.set_speed(settings.voice_tts_speed);
                    println!(
                        "  ✅ Kokoro TTS: {} @ {:.2}x (weights load on first utterance)",
                        out.voice().await,
                        out.speed()
                    );
                    Some(Arc::new(out))
                }
                Err(e) => {
                    tracing::warn!("Kokoro TTS unavailable: {e}");
                    None
                }
            }
        }
    };

    let tts: Option<Arc<dyn pond_core::models::ports::voice_output::VoiceOutput>> =
        match &kokoro_engine {
            Some(kokoro) => {
                println!("  ✅ TTS: kokoro");
                Some(kokoro.clone() as Arc<dyn pond_core::models::ports::voice_output::VoiceOutput>)
            }
            None => {
                println!("  ⚠  TTS: unavailable — responses will be text-only");
                None
            }
        };

    // Runtime reconfiguration, kept apart from `tts`: the chat loop only ever speaks.
    let tts_control: Option<Arc<dyn pond_core::models::ports::tts_control::TtsControl>> =
        kokoro_engine.as_ref().map(|engine| {
            Arc::new(kokoro_control::KokoroTtsControl::new(
                engine.clone(),
                data_dir.clone(),
                download_tracker.clone(),
            )) as Arc<dyn pond_core::models::ports::tts_control::TtsControl>
        });

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

    // Idempotent, so safe on every startup.
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

    // Detached so the HTTP server comes up without waiting on downloads.
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

    // `try_start` fetches a missing model itself through ModelService.
    let any_role_needs_llamafile = settings.chat_provider == "llamafile";

    let active_llm_name: String = settings.chat_model.clone();
    let (initial_llamafile_guard, llamafile_port) = if any_role_needs_llamafile {
        match llamafile_process::try_start(&data_dir, model_service.clone(), Some(&active_llm_name))
            .await
        {
            Some((proc, port)) => (Some(proc), port),
            None => (None, ports::llamafile_port()),
        }
    } else {
        println!(
            "  ⏭  LLM: llamafile skipped (provider = {})",
            settings.chat_provider
        );
        (None, ports::llamafile_port())
    };
    let llamafile_url = llamafile_process::url_for(llamafile_port);

    let llamafile_manager: Arc<dyn LlamafileManager> = Arc::new(LlamafileManagerImpl::new(
        data_dir.clone(),
        model_service.clone(),
        initial_llamafile_guard,
        llamafile_port,
    ));

    println!("  ────────────────────────────────────────────────────\n");

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
    // Deterministic, rule-based redactor (no model in the loop), shared by both chokepoints below.
    let redactor: Arc<dyn pond_core::security::ports::redactor::Redactor> =
        Arc::new(pond_infra::rule_redactor::RuleRedactor::new());
    // ── Embedding provider (gguf / fastembed / none) ─────────────────────────
    // Before the memory/context repos (they need its model id) and the agent (semantic recall).
    // "gguf" suits the Jetson (fastembed's ORT won't init there); it loads lazily on first embed.
    let embedding_provider: Option<
        Arc<dyn pond_core::models::ports::embedding::EmbeddingProvider + Send + Sync>,
    > = {
        use pond_core::models::ports::embedding::EmbeddingProvider as _;
        match settings.embedding_provider.as_str() {
            "none" => {
                tracing::info!("embedding provider: disabled (embedding_provider = \"none\")");
                None
            }
            #[cfg(feature = "local-inference")]
            "gguf" => {
                let embedding_dir = data_dir.join("models").join("embedding");
                // Shared with fastembed: a stale name or typo means the gguf default, never None.
                let spec =
                    pond_inference::EmbeddingModelSpec::resolve(&settings.active_embedding_model)
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                "'{}' is not a GGUF embedding model ({e}); using the default",
                                settings.active_embedding_model
                            );
                            pond_inference::EmbeddingModelSpec::nomic_embed_text_v1_5()
                        });
                let dest = embedding_dir.join(&spec.filename);
                if !dest.exists() {
                    if let Err(e) = std::fs::create_dir_all(&embedding_dir) {
                        tracing::warn!("could not create embedding model dir: {e}");
                    }
                    tracing::info!(
                        model_id = %spec.model_id,
                        size_mb = spec.size_hint_mb,
                        "fetching GGUF embedding model in the background (one-time, egress-gated)"
                    );
                    // Not awaited: ~146 MB would keep the port unbound; the provider loads lazily.
                    // Egress-gated in `pond_hf_cache`; HF URLs bypass `download_file`'s own gate.
                    let url = spec.download_url.clone();
                    let size_hint = spec.size_hint_mb;
                    let model_id = spec.model_id.clone();
                    let dest_bg = dest.clone();
                    tokio::spawn(async move {
                        use pond_core::models::ports::model_downloader::ModelDownloader as _;
                        let downloader = crate::http_model_downloader::HttpModelDownloader::new();
                        match downloader.download(&url, &dest_bg, size_hint).await {
                            Ok(()) => tracing::info!(
                                model_id = %model_id,
                                "GGUF embedding model downloaded; it loads on the next embed"
                            ),
                            Err(e) => tracing::warn!(
                                model_id = %model_id,
                                "GGUF embedding model download failed: {e:#} -- retrieval \
                                 falls back to keyword until it succeeds"
                            ),
                        }
                    });
                }
                let provider = pond_inference::GgufEmbeddingProvider::new(spec, &embedding_dir);
                tracing::info!(
                    model_id = provider.model_id(),
                    dims = provider.dimensions(),
                    "GGUF embedding provider ready (loads lazily on first embed)"
                );
                Some(Arc::new(provider)
                    as Arc<
                        dyn pond_core::models::ports::embedding::EmbeddingProvider + Send + Sync,
                    >)
            }
            #[cfg(not(feature = "local-inference"))]
            "gguf" => {
                tracing::warn!(
                    "embedding_provider = \"gguf\" needs the local-inference feature; \
                     embeddings disabled"
                );
                None
            }
            _ => {
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
        }
    };

    // Shared personal-context index; the model id below is `None` when embeddings are off.
    let vector_index: Arc<dyn pond_core::context::vector_index::VectorIndex> = Arc::new(
        pond_infra::sqlite_vector_index::SqliteVectorIndex::new(db.vectors.clone()),
    );
    // A member's first turn must not queue behind the pond indexing itself.
    let index_maintenance_cancel = tokio_util::sync::CancellationToken::new();
    // Held here, not in the sweep, so the reindex route can trigger an immediate refill.
    let index_reindex_requested = Arc::new(tokio::sync::Notify::new());
    let vector_model_id = embedding_provider.as_ref().map(|p| p.model_id());

    // Chokepoint 1: this one construction redacts every memory write, whoever the writer.
    let memory_repo: Arc<
        dyn pond_core::user_data::ports::memory_repository::MemoryRepository + Send + Sync,
    > = Arc::new(
        pond_core::user_data::services::redacting_memory_repository::RedactingMemoryRepository::new(
            // Inside the redactor, which drops a secret's vector: the index must mirror the store.
            Arc::new(
                SqliteMemoryRepository::new(db.system.clone())
                    .with_vector_index(vector_index.clone(), vector_model_id.clone()),
            ),
            redactor.clone(),
        ),
    );
    let sensor_storage: Arc<
        dyn pond_core::user_data::ports::sensor_storage::SensorStorage + Send + Sync,
    > = Arc::new(SqliteSensorStorage::new(db.logs.clone()));
    let camera_storage: Arc<
        dyn pond_core::user_data::ports::camera_storage::CameraStorage + Send + Sync,
    > = Arc::new(SqliteCameraStorage::new(db.logs.clone()));

    // Plain SQLite, so unlike `mesh_transport` these exist without the `mesh` feature.
    let peer_directory: Arc<
        dyn pond_core::mesh::ports::peer_directory::PeerDirectory + Send + Sync,
    > = Arc::new(pond_infra::sqlite_peer_directory::SqlitePeerDirectory::new(
        db.system.clone(),
    ));
    let credit_ledger: Arc<dyn pond_core::mesh::ports::credit_ledger::CreditLedger + Send + Sync> =
        Arc::new(pond_infra::sqlite_credit_ledger::SqliteCreditLedger::new(
            db.system.clone(),
        ));
    let usage_tally: Arc<dyn pond_core::mesh::ports::usage_tally::UsageTally + Send + Sync> =
        Arc::new(pond_infra::sqlite_usage_tally::SqliteUsageTally::new(
            db.system.clone(),
        ));

    // ── Face recognition ────────────────────────────────────────────────────
    // Needs the face-onnx feature and the model files on disk; otherwise `None`
    // (server starts normally; /api/v1/faces/* return 503).
    #[cfg(feature = "face-onnx")]
    {
        if let Err(e) = model_download::download_face_models(&data_dir).await {
            tracing::warn!("face model auto-download failed: {e:#}");
        }
        // Again: the antispoof file may only now exist, making the first pass a no-op.
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

    {
        use pond_core::prompts::BUILTIN_PROMPT_TEMPLATES;
        use pond_core::user_data::domain::prompt_template::PromptTemplate;
        #[allow(unused_imports)]
        use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
        for &(name, content, description) in BUILTIN_PROMPT_TEMPLATES {
            let t = PromptTemplate {
                name: name.to_string(),
                content: content.to_string(),
                description: description.to_string(),
                is_system: true,
                is_customized: false,
                factory_version: pond_core::user_data::domain::prompt_template::FACTORY_VERSION,
                updated_at: String::new(),
            };
            if let Err(e) = prompt_template_repo.seed_system_template(&t).await {
                tracing::warn!("Failed to reseed built-in prompt template '{name}': {e}");
            }
        }
        tracing::info!("Built-in prompt templates reseeded (Jinja2 general-purpose copilot)");
    }

    let effective_chat_provider = settings.chat_provider.clone();
    let effective_chat_model = settings.chat_model.clone();

    // Speculative-decoding drafter; on failure, decode is just not accelerated.
    // Before ANY provider: `apply_jetson_settings` reads the registry once, at adapter build.
    let drafter_wanted = model_download::drafter_for(&settings.chat_model).is_some();
    let drafter_ready = if drafter_wanted {
        let present = model_download::ensure_mtp_drafter(&data_dir, &settings.chat_model)
            .await
            .is_some();
        // The engine finds a drafter by its registry row; a file with no row is invisible.
        #[cfg(feature = "goose-agent")]
        if present {
            pond_adapters_goose::mtp_drafter::ensure_drafter_registered(
                &data_dir,
                &settings.chat_model,
            );
        }
        present
    } else {
        false
    };

    // ── Build per-role LLM providers ────────────────────────────────────────
    // TODO(cloud-fallback): wire `cloud_fallback_enabled` (spill to cloud on local failure only).
    async fn build_provider(
        provider: &str,
        model: &str,
        llamafile_url: &str,
        data_dir: Option<&std::path::Path>,
        max_tokens: u32,
        temperature: f32,
    ) -> Arc<dyn LlmProvider> {
        match provider {
            // Mesh isn't built yet at startup; fail visibly rather than silently use llamafile.
            // The next `PUT /settings` re-derives this from the live mesh provider.
            "mesh" => Arc::new(UnavailableProvider::new(
                "mesh inference is not ready yet at startup — save any setting via \
                 PUT /api/v1/settings to re-check, or wait for mesh to finish connecting",
            )) as Arc<dyn LlmProvider>,

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

            // OpenAI-compatible like llamafile, so LlamafileProvider only needs the URL.
            "mistralrs" => {
                let host = std::env::var("GIAP_MISTRALRS_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:9002".to_string());
                Arc::new(
                    LlamafileProvider::new(Some(&host))
                        .with_max_tokens(max_tokens)
                        .with_temperature(temperature),
                ) as Arc<dyn LlmProvider>
            }

            _ => Arc::new(
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

    // No `InferencePool` implementation yet: nothing submits work to one.
    let inference_pool: Option<Arc<dyn pond_core::models::ports::inference_pool::InferencePool>> =
        None;

    // Always built: routes.rs checks `review_mode` per request, so it toggles at runtime.
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

    let (memory_extractor_for_http, memory_extraction_service_unwired) =
        if settings.memory_extraction_enabled {
            let extractor: Arc<dyn pond_core::user_data::ports::memory_extractor::MemoryExtractor> =
                Arc::new(llm_memory_extractor::LlmMemoryExtractor::new(
                    llm_provider.clone(),
                    settings.memory_extraction_max_facts,
                ));
            let service =
                pond_core::user_data::services::memory_extraction::MemoryExtractionService::new(
                    settings.memory_extraction_interval_secs,
                );
            tracing::info!(
                "memory extraction enabled — facts will be auto-extracted from conversations"
            );
            (Some(extractor), Some(service))
        } else {
            (None, None)
        };

    let db = Arc::new(db);

    // TTL pruning every 6 h; re-reads retention settings each cycle.
    {
        let logs = db.logs.clone();
        let system = db.system.clone();
        let settings_repo = settings_repo.clone();
        tokio::spawn(async move {
            pond_infra::pruning::run_pruning(logs, system, settings_repo).await;
        });
    }

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

    // ── Memory consolidation (inactivity-based) ──────────────────────────
    // `last_user_activity` is bumped by every route; the event channel feeds SSE and logs.
    let last_user_activity = Arc::new(tokio::sync::RwLock::new(std::time::Instant::now()));
    let consolidation_cancel: Arc<
        tokio::sync::RwLock<Option<tokio_util::sync::CancellationToken>>,
    > = Arc::new(tokio::sync::RwLock::new(None));
    let (consolidation_event_tx, _) = tokio::sync::broadcast::channel::<
        pond_core::user_data::ports::memory_consolidator::ConsolidationEvent,
    >(64);

    // The one inference slot all background jobs share; the lane decides when each may run.
    let inference_lane = crate::inference_lane_runner::InferenceLane::new();

    // For POST /api/v1/memory/consolidate; a closure so pond-api never imports the consolidator.
    // Built unconditionally: the enable toggle is read fresh from settings at run time.
    let consolidation_runner: Option<pond_api::ConsolidationRunner> = {
        let cr_repo = memory_repo.clone();
        let cr_provider = llm_provider.clone();
        let cr_broadcast_tx = consolidation_event_tx.clone();
        let cr_settings_repo = settings_repo.clone();
        Some(Arc::new(
            move |cancel: tokio_util::sync::CancellationToken| {
                let repo = cr_repo.clone();
                let provider = cr_provider.clone();
                let broadcast_tx = cr_broadcast_tx.clone();
                let settings_repo = cr_settings_repo.clone();
                Box::pin(async move {
                    // Read per run, not at startup, so a manual run honours the latest settings.
                    let (mode, batch_size) = match settings_repo.get().await {
                        Ok(s) => (
                            s.memory_consolidation_mode,
                            s.memory_consolidation_batch_size as usize,
                        ),
                        Err(e) => {
                            tracing::warn!(
                                "consolidation: settings read failed ({e}) — using defaults"
                            );
                            let d = pond_core::user_data::domain::settings::Settings::default();
                            (
                                d.memory_consolidation_mode,
                                d.memory_consolidation_batch_size as usize,
                            )
                        }
                    };
                    run_consolidation_pipeline(
                        repo,
                        provider,
                        broadcast_tx,
                        cancel,
                        &mode,
                        batch_size,
                    )
                    .await;
                })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            },
        ))
    };

    // ── Inactivity-based consolidation scheduler ─────────────────────────
    // At most one run per interval, only after idling that follows real activity since boot.
    // Activity = HTTP routes or `sessions.updated_at` (the voice loop is a separate process).
    {
        use pond_core::user_data::services::consolidation_schedule as sched;
        use pond_core::user_data::services::inference_lane::LaneJob;

        let inact_repo = memory_repo.clone();
        let inact_provider = llm_provider.clone();
        let inact_activity = last_user_activity.clone();
        let inact_cancel = consolidation_cancel.clone();
        let inact_event_tx = consolidation_event_tx.clone();
        let inact_settings_repo = settings_repo.clone();
        let inact_storage = session_storage.clone();
        let inact_lane = inference_lane.clone();

        // Baselines for the "never on startup" guard; captured before the server binds.
        let started_at = std::time::Instant::now();
        let started_at_utc = chrono::Utc::now();

        tokio::spawn(async move {
            const POLL_SECS: u64 = 60;
            let idle_threshold = std::time::Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS);

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(POLL_SECS)).await;

                let settings = match inact_settings_repo.get().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("consolidation scheduler: settings read failed: {e}");
                        continue;
                    }
                };

                // Out-of-process activity (voice child, anything else writing turns).
                let db_activity = newest_session_activity(inact_storage.as_ref()).await;
                let in_process_at = *inact_activity.read().await;

                let saw_activity_since_start = sched::saw_activity_since_start(
                    started_at,
                    in_process_at,
                    started_at_utc,
                    db_activity,
                );

                let idle_for =
                    sched::combined_idle_for(in_process_at, db_activity, chrono::Utc::now());

                // The lane gates across all jobs; holding `slot` means no other job is mid-run.
                let Some(slot) = inact_lane
                    .acquire(
                        LaneJob::Consolidation,
                        settings.memory_consolidation_enabled,
                        sched::interval_floor_from_hours(
                            settings.memory_consolidation_interval_hours,
                        ),
                        saw_activity_since_start,
                        idle_for,
                        idle_threshold,
                        // Never exempt: a pond nobody has talked to has nothing to consolidate.
                        false,
                    )
                    .await
                else {
                    continue;
                };

                tracing::info!(
                    mode = %settings.memory_consolidation_mode,
                    idle_secs = idle_for.as_secs(),
                    "idle after user activity — starting memory consolidation"
                );

                let cancel = tokio_util::sync::CancellationToken::new();
                *inact_cancel.write().await = Some(cancel.clone());

                // Routes cancel directly; the DB poll lets an out-of-process voice turn abort.
                let watcher_activity = inact_activity.clone();
                let watcher_storage = inact_storage.clone();
                let watcher_cancel = cancel.clone();
                let watcher_baseline_in_process = in_process_at;
                let watcher_baseline_db = db_activity;
                let watcher = tokio::spawn(async move {
                    // The clock is a cheap lock read; the DB is queried only every Nth tick.
                    const TICK_MS: u64 = 500;
                    const DB_EVERY_N_TICKS: u32 = 4;
                    let mut tick: u32 = 0;
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
                        if watcher_cancel.is_cancelled() {
                            break;
                        }
                        tick = tick.wrapping_add(1);

                        let resumed_in_process =
                            *watcher_activity.read().await > watcher_baseline_in_process;

                        let resumed_in_db = if tick % DB_EVERY_N_TICKS == 0 {
                            match newest_session_activity(watcher_storage.as_ref()).await {
                                // A failed read is None, not activity.
                                Some(latest) => match watcher_baseline_db {
                                    Some(baseline) => latest > baseline,
                                    None => true,
                                },
                                None => false,
                            }
                        } else {
                            false
                        };

                        if resumed_in_process || resumed_in_db {
                            tracing::info!(
                                "user activity resumed — cancelling memory consolidation"
                            );
                            watcher_cancel.cancel();
                            break;
                        }
                    }
                });

                run_consolidation_pipeline(
                    inact_repo.clone(),
                    inact_provider.clone(),
                    inact_event_tx.clone(),
                    cancel,
                    &settings.memory_consolidation_mode,
                    settings.memory_consolidation_batch_size as usize,
                )
                .await;
                watcher.abort();

                // Dropping the slot spends the interval budget even if cancelled or skipped; that
                // floor, not the activity clock, stops re-fires (a missed pass beats churn).
                drop(slot);
            }
        });
        tracing::info!(
            "memory consolidation scheduler active — runs after {} min idle, at most once per interval (enable toggle is live)",
            sched::INACTIVITY_THRESHOLD_SECS / 60
        );
    }

    // ── Idle rolling-summary refresh (hybrid compaction, soft half) ──────
    // Same idle contract as consolidation, over every session (the voice child's too).
    if settings.hybrid_compaction_enabled {
        let sum_storage = session_storage.clone();
        let sum_provider = llm_provider.clone();
        let sum_activity = last_user_activity.clone();
        let idle_secs = settings.summary_idle_secs.max(30) as u64;

        tokio::spawn(async move {
            // "Never at startup": only sessions updated since this process started.
            let started_at = chrono::Utc::now();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;

                let idle_for = sum_activity.read().await.elapsed();
                if idle_for < std::time::Duration::from_secs(idle_secs) {
                    continue;
                }
                let provider = match sum_provider.read().await.clone() {
                    Some(p) => p,
                    None => continue,
                };
                let sessions = match sum_storage.list_sessions().await {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                for session in sessions {
                    if session.updated_at < started_at {
                        continue;
                    }
                    // Abort on activity: the serial engine must never make a user turn wait.
                    let cancel = tokio_util::sync::CancellationToken::new();
                    let watcher_activity = sum_activity.clone();
                    let watcher_cancel = cancel.clone();
                    let baseline = idle_for;
                    let watcher = tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            if watcher_activity.read().await.elapsed() < baseline {
                                watcher_cancel.cancel();
                                break;
                            }
                        }
                    });

                    let svc =
                        pond_core::shared::services::session_summary::SessionSummaryService::new(
                            provider.clone(),
                            sum_storage.clone(),
                        );
                    match svc.refresh(&session.id, &cancel).await {
                        Ok(pond_core::shared::services::session_summary::RefreshOutcome::Refreshed { through_message_id }) => {
                            tracing::info!(
                                target: "giap::trace",
                                kind = "summary_refresh",
                                session_id = %session.id,
                                through_message_id = %through_message_id,
                            );
                        }
                        Ok(pond_core::shared::services::session_summary::RefreshOutcome::Cancelled) => {
                            tracing::debug!("summary refresh cancelled by user activity");
                            watcher.abort();
                            break; // user is back — stop the whole sweep
                        }
                        Ok(pond_core::shared::services::session_summary::RefreshOutcome::NothingToDo) => {}
                        Err(e) => tracing::debug!("summary refresh failed: {e}"),
                    }
                    watcher.abort();
                }
            }
        });
        tracing::info!(
            "hybrid compaction enabled — rolling-summary refresh after {}s idle",
            settings.summary_idle_secs.max(30)
        );
    }

    // ── Idle conversation re-titling ─────────────────────────────────────
    // Renames six-word first-message titles, under consolidation's idle contract. Title writers
    // must not bump `sessions.updated_at`: it's an activity source, so the job would cancel itself.
    {
        use pond_core::shared::domain::session_activity::SessionOrigin;
        use pond_core::shared::services::session_title::{RetitleOutcome, SessionTitleService};
        use pond_core::user_data::services::consolidation_schedule as sched;
        use pond_core::user_data::services::inference_lane::LaneJob;

        // How often a pass is considered; the gate decides whether one runs.
        const POLL_SECS: u64 = 5 * 60;
        // Per-pass cap, so hundreds of conversations can't eat a whole idle window.
        const MAX_PER_PASS: usize = 5;

        let title_storage = session_storage.clone();
        let title_provider = llm_provider.clone();
        let title_activity = last_user_activity.clone();
        let title_settings_repo = settings_repo.clone();
        let title_lane = inference_lane.clone();

        // Baselines for the "never on startup" guard; captured before the server binds.
        let started_at = std::time::Instant::now();
        let started_at_utc = chrono::Utc::now();

        tokio::spawn(async move {
            let idle_threshold = std::time::Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS);

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(POLL_SECS)).await;

                let settings = match title_settings_repo.get().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("titling scheduler: settings read failed: {e}");
                        continue;
                    }
                };

                let db_activity = newest_session_activity(title_storage.as_ref()).await;
                let in_process_at = *title_activity.read().await;
                let now = chrono::Utc::now();

                let saw_activity_since_start = sched::saw_activity_since_start(
                    started_at,
                    in_process_at,
                    started_at_utc,
                    db_activity,
                );
                let idle_for = sched::combined_idle_for(in_process_at, db_activity, now);

                let Some(slot) = title_lane
                    .acquire(
                        LaneJob::Titling,
                        // Re-read every tick, so the toggle is live.
                        settings.session_titling_enabled,
                        // The tick is the floor: a pass is bounded and cheap.
                        std::time::Duration::from_secs(POLL_SECS),
                        saw_activity_since_start,
                        idle_for,
                        idle_threshold,
                        // Never exempt: no turn since boot means no conversation to name.
                        false,
                    )
                    .await
                else {
                    continue;
                };

                let Some(provider) = title_provider.read().await.clone() else {
                    continue;
                };
                let sessions = match title_storage.list_sessions().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("titling scheduler: session list failed: {e}");
                        continue;
                    }
                };

                // One token per pass: resumed activity ends the whole sweep.
                let cancel = tokio_util::sync::CancellationToken::new();

                let watcher_activity = title_activity.clone();
                let watcher_storage = title_storage.clone();
                let watcher_cancel = cancel.clone();
                let watcher_baseline_in_process = in_process_at;
                let watcher_baseline_db = db_activity;
                let watcher = tokio::spawn(async move {
                    // The clock is a cheap lock read; the DB is queried only every Nth tick.
                    const TICK_MS: u64 = 500;
                    const DB_EVERY_N_TICKS: u32 = 4;
                    let mut tick: u32 = 0;
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
                        if watcher_cancel.is_cancelled() {
                            break;
                        }
                        tick = tick.wrapping_add(1);

                        let resumed_in_process =
                            *watcher_activity.read().await > watcher_baseline_in_process;

                        let resumed_in_db = if tick % DB_EVERY_N_TICKS == 0 {
                            match newest_session_activity(watcher_storage.as_ref()).await {
                                // A failed read is None, not activity.
                                Some(latest) => match watcher_baseline_db {
                                    Some(baseline) => latest > baseline,
                                    None => true,
                                },
                                None => false,
                            }
                        } else {
                            false
                        };

                        if resumed_in_process || resumed_in_db {
                            tracing::info!(
                                "user activity resumed — cancelling conversation re-titling"
                            );
                            watcher_cancel.cancel();
                            break;
                        }
                    }
                });

                let service = SessionTitleService::new(provider, title_storage.clone());
                let mut renamed = 0usize;

                for session in sessions {
                    if renamed >= MAX_PER_PASS {
                        break;
                    }
                    // Skip background sessions (e.g. cron runs): nobody reads their titles.
                    if !SessionOrigin::of(&session.id).is_human() {
                        continue;
                    }

                    match service.retitle(&session.id, &cancel).await {
                        Ok(RetitleOutcome::Retitled { title, .. }) => {
                            renamed += 1;
                            tracing::info!(
                                target: "giap::trace",
                                kind = "session_retitled",
                                session_id = %session.id,
                                title = %title,
                            );
                        }
                        Ok(RetitleOutcome::Cancelled) => {
                            tracing::debug!("re-titling cancelled — somebody is back");
                            break;
                        }
                        Ok(RetitleOutcome::Skipped(_)) | Ok(RetitleOutcome::Unusable) => {}
                        Err(e) => {
                            tracing::debug!("re-titling {} failed: {e}", session.id);
                        }
                    }
                }

                watcher.abort();

                // Dropping the slot releases the lane and spends the budget, renamed or not.
                drop(slot);
            }
        });
        tracing::info!(
            "conversation re-titling active — runs after {} min idle, at most {} per pass (enable toggle is live)",
            sched::INACTIVITY_THRESHOLD_SECS / 60,
            MAX_PER_PASS,
        );
    }

    // Debug: print new event_log rows live; rows from before startup are not replayed.
    if debug {
        let logs_pool = db.logs.clone();
        tokio::spawn(async move {
            tail_event_log(logs_pool).await;
        });
    }

    // For the MCP weather tool (giap__get_current_weather), not AppState.
    let weather: Option<Arc<dyn WeatherProvider>> = {
        // `Location::weather_target` decides: coordinates, or a name the adapter geocodes.
        let place = pond_core::user_data::services::location::resolve(&settings);
        if let (true, Some((lat, lon, loc))) = (settings.weather_enabled, place.weather_target()) {
            tracing::info!("weather enabled: {} ({}, {})", loc, lat, lon);
            Some(Arc::new(OpenMeteoWeatherAdapter::new(lat, lon, loc)))
        } else {
            tracing::info!(
                "weather disabled — enable via PUT /api/v1/settings (weather_enabled + lat/lon or location_name)"
            );
            None
        }
    };

    // libp2p MeshTransport, gated on `mesh_enabled`; wired into AppState so the
    // /api/v1/mesh/* routes can use it.
    let mesh_transport =
        build_mesh_transport(&settings, &settings_repo, peer_directory.clone()).await;

    // Lightning rail, before build_mesh_provider so InvoiceRequests are answerable at once.
    let payment_rail = build_payment_rail(&settings, &settings_repo, &data_dir).await;

    // All three are `None` without a mesh transport (disabled, or built without `mesh`).
    let (mesh_provider, peer_capability_query, invoice_requester) = build_mesh_provider(
        &mesh_transport,
        peer_directory.clone(),
        credit_ledger.clone(),
        usage_tally.clone(),
        settings_repo.clone(),
        llm_provider.clone(),
        payment_rail.clone(),
    );
    // Captures the startup `invoice_requester`: a hot-enable via `mesh_rebuild` doesn't reach
    // this job, so settlement stays restart-only.
    spawn_settlement_job(
        peer_directory.clone(),
        usage_tally.clone(),
        payment_rail.clone(),
        invoice_requester,
    );

    // Locks so `mesh_rebuild` can hot-enable mesh; GooseAdapter shares the `mesh_provider` lock.
    let mesh_transport = Arc::new(tokio::sync::RwLock::new(mesh_transport));
    let mesh_provider = Arc::new(tokio::sync::RwLock::new(mesh_provider));
    let peer_capability_query = Arc::new(tokio::sync::RwLock::new(peer_capability_query));

    // Called by `update_settings` when `mesh_enabled` turns true; no-op once built (the
    // transport allows one `recv()` consumer). Never tears down on disable.
    let mesh_rebuild: Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync> = {
        let settings_repo = settings_repo.clone();
        let peer_directory = peer_directory.clone();
        let credit_ledger = credit_ledger.clone();
        let usage_tally = usage_tally.clone();
        let llm_provider = llm_provider.clone();
        let payment_rail = payment_rail.clone();
        let mesh_transport = mesh_transport.clone();
        let mesh_provider = mesh_provider.clone();
        let peer_capability_query = peer_capability_query.clone();
        Arc::new(move || {
            let settings_repo = settings_repo.clone();
            let peer_directory = peer_directory.clone();
            let credit_ledger = credit_ledger.clone();
            let usage_tally = usage_tally.clone();
            let llm_provider = llm_provider.clone();
            let payment_rail = payment_rail.clone();
            let mesh_transport = mesh_transport.clone();
            let mesh_provider = mesh_provider.clone();
            let peer_capability_query = peer_capability_query.clone();
            Box::pin(async move {
                if mesh_transport.read().await.is_some() {
                    return;
                }
                let settings = match settings_repo.get().await {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!("mesh hot-enable: failed to read settings: {err}");
                        return;
                    }
                };
                let Some(new_transport) =
                    build_mesh_transport(&settings, &settings_repo, peer_directory.clone()).await
                else {
                    return; // still disabled, or a problem build_mesh_transport already logged
                };
                let new_transport = Some(new_transport);
                let (new_provider, new_capability_query, _invoice_requester) = build_mesh_provider(
                    &new_transport,
                    peer_directory,
                    credit_ledger,
                    usage_tally,
                    settings_repo.clone(),
                    llm_provider,
                    payment_rail,
                );
                *mesh_transport.write().await = new_transport;
                *mesh_provider.write().await = new_provider;
                *peer_capability_query.write().await = new_capability_query;
                tracing::info!(
                    "mesh enabled at runtime — transport/provider built, no restart needed"
                );
            }) as futures::future::BoxFuture<'static, ()>
        })
    };

    // DeferredExecutor: the scheduler precedes the agent; AgentScheduleExecutor is injected later.
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

    // Embed facts at write, so they're searchable next turn rather than after a backfill.
    let memory_extraction_service_for_http = memory_extraction_service_unwired.map(|service| {
        Arc::new(match &embedding_provider {
            Some(provider) => service.with_embedding_provider(provider.clone()),
            None => service,
        })
    });

    // ── Embedding backfill ───────────────────────────────────────────────────
    // `search_similar` skips unembedded rows; batched with pauses as it competes with inference.
    if let Some(provider) = embedding_provider.clone() {
        let backfill_repo = memory_repo.clone();
        tokio::spawn(async move {
            use pond_core::user_data::services::memory_relevance as relevance;
            relevance::run_backfill(
                backfill_repo.as_ref(),
                provider.as_ref(),
                relevance::BACKFILL_BATCH_SIZE,
                relevance::BACKFILL_BATCH_PAUSE_MS,
            )
            .await;
            // Then re-embed rows from another model: search excludes them and the backfill can't
            // see them. Second, because a row with no vector at all is the worse miss.
            relevance::run_dimension_repair(
                backfill_repo.as_ref(),
                provider.as_ref(),
                relevance::BACKFILL_BATCH_SIZE,
                relevance::BACKFILL_BATCH_PAUSE_MS,
            )
            .await;
        });
    }

    // ── Personal-context index maintenance ───────────────────────────────────
    // `index_sweep_running` lets the rebuild route say whether anything will refill the index.
    let index_sweep_running = embedding_provider.is_some() && vector_model_id.is_some();
    if let (Some(provider), Some(_)) = (embedding_provider.clone(), vector_model_id.clone()) {
        use pond_core::user_data::services::inference_lane::LaneJob;

        let index = vector_index.clone();
        let storage = session_storage.clone();
        let cancel = index_maintenance_cancel.clone();
        let sweep_lane = inference_lane.clone();
        let sweep_activity = last_user_activity.clone();
        let sweep_storage = session_storage.clone();
        let reindex = index_reindex_requested.clone();

        let started_at = std::time::Instant::now();
        let started_at_utc = chrono::Utc::now();

        tokio::spawn(async move {
            use pond_core::context::index_maintenance::{plan_sweep, run_index_maintenance};
            // Consolidation's idle threshold, so the two definitions of "idle" can't drift.
            use pond_core::user_data::services::consolidation_schedule as sched;

            let poll = std::time::Duration::from_secs(INDEX_MAINTENANCE_POLL_SECS);
            let chore_idle = std::time::Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS);
            // Whether a pass has COMPLETED since this process started.
            let mut indexed_since_boot = false;

            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(
                    INDEX_MAINTENANCE_DELAY_SECS,
                )) => {}
            }

            loop {
                let asked = tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(poll) => false,
                    _ = reindex.notified() => true,
                };

                let db_activity = newest_session_activity(sweep_storage.as_ref()).await;
                let in_process_at = *sweep_activity.read().await;
                let now = chrono::Utc::now();
                let saw_activity_since_start = sched::saw_activity_since_start(
                    started_at,
                    in_process_at,
                    started_at_utc,
                    db_activity,
                );
                let idle_for = sched::combined_idle_for(in_process_at, db_activity, now);

                // A requested pass skips the idle wait and the floor (a person is waiting on it);
                // the lane's single slot still serialises jobs.
                let (floor, idle_threshold) = if asked {
                    (std::time::Duration::ZERO, std::time::Duration::ZERO)
                } else {
                    (poll, chore_idle)
                };

                let first_post_boot = !indexed_since_boot;
                let tick = plan_sweep(asked, indexed_since_boot);

                let Some(_slot) = sweep_lane
                    .acquire(
                        LaneJob::IndexMaintenance,
                        // No toggle of its own: an unmaintained index silently goes stale.
                        true,
                        floor,
                        // A request or the first post-boot pass is exempt; see `plan_sweep`.
                        saw_activity_since_start,
                        idle_for,
                        idle_threshold,
                        // Per-job, so other chores stay gated.
                        tick.exempt_from_activity_gate,
                    )
                    .await
                else {
                    continue;
                };

                // Interruptible per pass; a child token, so cancelling it spares the sweep task.
                let pass = cancel.child_token();
                // Cancel only on activity newer than this pass's admission, not merely recent:
                // otherwise the turn before a Reindex press kills the pass it asked for.
                let baseline = *sweep_activity.read().await;
                let watcher = tokio::spawn({
                    let pass = pass.clone();
                    let activity = sweep_activity.clone();
                    async move {
                        // In-process only: set at turn start, while the DB timestamp lags.
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            if *activity.read().await > baseline {
                                pass.cancel();
                                return;
                            }
                        }
                    }
                });

                let report = run_index_maintenance(
                    &index,
                    storage.as_ref(),
                    provider.as_ref(),
                    &pass,
                    tick.budget,
                )
                .await;
                watcher.abort();

                // Only a completed pass spends the exemption: an interrupted one left the backlog.
                if !pass.is_cancelled() {
                    indexed_since_boot = true;
                }

                if asked || first_post_boot {
                    tracing::info!(
                        requested = asked,
                        interrupted = pass.is_cancelled(),
                        adopted = report.adopted,
                        summaries = report.summaries_indexed,
                        context = report.context_indexed,
                        still_missing = report.still_missing,
                        "personal-context index pass finished"
                    );
                }
            }
        });
    }

    // ── Agent backend ────────────────────────────────────────────────────────────
    #[cfg(feature = "pond-agent")]
    let pond_agent_active = agent_backend == "pond"; // stays false: quarantine override above
    #[cfg(not(feature = "pond-agent"))]
    let pond_agent_active = false;

    // Reachable, unlike "pond"; the off-by-default cargo feature keeps it out of production.
    #[cfg(feature = "mistralrs-agent")]
    let mistralrs_active = agent_backend == pond_adapters_mistralrs::BACKEND_NAME;
    #[cfg(not(feature = "mistralrs-agent"))]
    let mistralrs_active = false;

    // Created before the Matter runtime, which publishes sensor updates onto it.
    let event_bus: Arc<dyn pond_core::shared::ports::event_bus::EventBus> =
        Arc::new(InProcessEventBus::new());

    // `device_control` is a facade over Matter (when on and reachable) or the logging stub,
    // swapped at runtime behind one long-lived `Arc`.
    type MatterRuntimeHandle =
        Option<Arc<dyn pond_core::user_data::ports::matter_runtime::MatterRuntimePort>>;
    type DeviceControl = Arc<dyn pond_core::user_data::ports::device_control::DeviceControlPort>;

    // Concrete runtime kept too: `attach_notifications` is adapter-only, not on the port.
    #[cfg(feature = "goose-agent")]
    let (matter_concrete, device_control): (
        Option<Arc<pond_adapters_matter::MatterRuntime>>,
        DeviceControl,
    ) = {
        // Persists each Matter sensor event before publishing it. `AppState` keeps the plain
        // bus: `record_sensor` persists inline, so the decorator there would double-write.
        let matter_bus: Arc<dyn pond_core::shared::ports::event_bus::EventBus> = Arc::new(
            pond_core::shared::services::sensor_persisting_event_bus::SensorPersistingEventBus::new(
                event_bus.clone(),
                sensor_storage.clone(),
            ),
        );
        let runtime = pond_adapters_matter::MatterRuntime::new(
            data_dir.clone(),
            device_registry.clone(),
            matter_bus,
        );
        let control = runtime.device_control(Arc::new(
            pond_infra::logging_device_control::LoggingDeviceControl::new(),
        ));
        // Not converged yet: `apply` waits for the notification sender, further down.
        (Some(runtime), control)
    };

    #[cfg(feature = "goose-agent")]
    let matter_runtime: MatterRuntimeHandle = matter_concrete
        .clone()
        .map(|runtime| runtime as Arc<dyn MatterRuntimePort>);

    #[cfg(not(feature = "goose-agent"))]
    let (matter_runtime, device_control): (MatterRuntimeHandle, DeviceControl) = (
        None,
        Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
    );

    // After the Matter runtime: `device_control` lets `get_sensor_reading` ask a live device
    // (the log only records changes). Anywhere before serving is early enough.
    pond_mcp_server::init_sensor_deps(
        sensor_storage.clone(),
        device_registry.clone(),
        device_control.clone(),
    );

    let (agent, extension_manager, _tool_caller, tool_registry) = if mistralrs_active {
        // No goose: no extension manager or tool specialist, and an empty registry.
        let default_registry: Arc<
            dyn pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort,
        > = Arc::new(pond_core::mcp::services::tool_registry::InMemoryToolRegistry::new());

        #[cfg(feature = "mistralrs-agent")]
        let agent: Arc<dyn Agent> = {
            let settings = settings_repo.get().await.unwrap_or_default();
            let base_url = pond_adapters_mistralrs::base_url_from_env();
            // mistral.rs doesn't report its window; a wrong value means refused requests.
            let context_tokens =
                pond_core::models::services::context::context_governor::ContextGovernor::resolve(
                    &pond_core::models::services::context::context_governor::ContextInputs {
                        provider: &settings.chat_provider,
                        model: &settings.chat_model,
                        override_tokens: settings.context_window_override,
                        registry_pinned: None,
                        catalog_context_length: None,
                        engine_reported: None,
                        capability_window: None,
                    },
                )
                .tokens;

            let dispatcher: Arc<dyn pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher> =
                Arc::new(pond_mcp_server::McpToolDispatcher::new(
                    memory_repo.clone(),
                    weather.clone(),
                    scheduler.clone(),
                    settings_repo.clone(),
                    device_registry.clone(),
                    skill_repo.clone(),
                    embedding_provider.clone(),
                    device_control.clone(),
                ));

            let provider = Arc::new(
                pond_adapters_mistralrs::MistralRsProvider::new(&base_url, &settings.chat_model)
                    .with_context_window(u32::try_from(context_tokens).unwrap_or(u32::MAX)),
            );
            println!("  Agent backend: mistralrs -> {base_url} (no goose)");
            tracing::info!(
                base_url = %base_url,
                model = %settings.chat_model,
                context_tokens,
                "MistralRsAgent ready — direct path, goose not involved"
            );
            Arc::new(pond_adapters_mistralrs::MistralRsAgent::new(
                provider,
                settings_repo.clone(),
                Some(prompt_template_repo.clone()),
                Some(prompt_extra_repo.clone()),
                Some(skill_repo.clone()),
                session_storage.clone(),
                Some(dispatcher),
            )) as Arc<dyn Agent>
        };
        #[cfg(not(feature = "mistralrs-agent"))]
        let agent: Arc<dyn Agent> = Arc::new(MockAgent::new());

        (agent, None, None, default_registry)
    } else if pond_agent_active {
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
            device_control.clone(),
            Some(session_storage.clone()),
            Some(model_repo.clone()),
            false, // voice_mode — server mode, not voice
            mesh_provider.clone(),
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
            Some(device_control.clone()),
            settings.schedule_max_concurrent,
            tts.clone(),
            Some(settings_repo.clone()),
        ));
        deferred_executor
            .init(
                real_executor
                    as Arc<dyn pond_core::user_data::ports::schedule_execution::ScheduleExecutor>,
            )
            .await;
        tracing::info!("schedule executor initialized — agent-prompt schedules are now active");
    }

    // Encrypted at rest; before MCP auto-connect, which resolves OAuth tokens from it.
    let secret_repo: Option<
        Arc<dyn pond_core::security::ports::secret::SecretRepository + Send + Sync>,
    > = {
        match pond_infra::file_secret_repository::FileSecretRepository::new(&data_dir) {
            Ok(repo) => {
                tracing::info!(
                    store = %data_dir.join("secrets.json").display(),
                    key_file = %pond_infra::secret_crypto::key_path(&data_dir).display(),
                    "secret repository initialized (encrypted at rest)"
                );
                Some(Arc::new(repo))
            }
            Err(e) => {
                // Stays None either way, so nothing can overwrite the ciphertext (recoverable if
                // the key returns); a locked store gets its own operator guidance.
                if let Some(locked) =
                    e.downcast_ref::<pond_infra::secret_crypto::SecretStoreLocked>()
                {
                    tracing::error!("{locked}");
                    tracing::error!(
                        "no API key or connector token can be read or written until that key \
                         is restored. If it is genuinely gone the secrets cannot be recovered \
                         by anyone, including us: move {} aside, restart, then re-enter API \
                         keys and re-authorise connectors.",
                        locked.store_path.display()
                    );
                } else {
                    tracing::warn!("failed to initialize secret repository: {e}");
                }
                None
            }
        }
    };

    // The SAME instance: `FileSecretRepository` caches secrets.json and rewrites it whole on
    // `set`, so a second one would serve stale data and clobber writes.
    if let Some(repo) = &secret_repo {
        pond_mcp_server::init_secret_deps(repo.clone());

        // Move API keys out of the settings table; `Settings` no longer reads them.
        match pond_infra::secret_migration::migrate_api_keys_to_secret_repository(
            &settings_repo,
            repo,
        )
        .await
        {
            Ok(report) => {
                if !report.left_in_place.is_empty() {
                    tracing::warn!(
                        keys = ?report.left_in_place,
                        "API-key migration could not complete for these settings rows; they were \
                         left in place so the values are not lost. They are no longer read."
                    );
                }
            }
            Err(e) => tracing::warn!("API-key migration failed: {e}"),
        }
    } else {
        tracing::warn!(
            "no secret repository: third-party API keys are unavailable this run and the \
             settings-to-secrets migration did not run"
        );
    }

    // Before MCP auto-connect, which looks up required_secrets. The asset root makes bundled
    // script paths independent of the launch cwd.
    let asset_root = asset_root::resolve();
    tracing::info!(asset_root = %asset_root.display(), "resolved extension asset root");
    let marketplace: Arc<dyn pond_core::mcp::ports::extension_marketplace::ExtensionMarketplace> =
        Arc::new(
            pond_core::mcp::services::marketplace::BundledMarketplace::with_asset_root(&asset_root),
        );

    let mcp_server_repo: Option<Arc<dyn pond_core::mcp::ports::mcp_server::McpServerRepository>> = {
        let repo = Arc::new(SqliteMcpServerRepository::new(db.system.clone()));
        if let Some(mgr) = &extension_manager {
            match repo.list().await {
                Ok(servers) => {
                    for srv in servers
                        .into_iter()
                        .filter(|s: &pond_core::mcp::ports::mcp_server::McpServerConfig| s.enabled)
                    {
                        use pond_core::mcp::ports::extension_manager::AddExtensionRequest;

                        // Secrets overwrite the persisted env: the row's copy is an
                        // install-time snapshot, and tokens rotate.
                        let mut env = srv.env.clone();
                        if let Some(sr) = &secret_repo {
                            if let Ok(Some(ext)) = marketplace.get_by_id(&srv.name).await {
                                for secret_req in &ext.required_secrets {
                                    if let Ok(Some(val)) = sr.get(&secret_req.key).await {
                                        env.insert(secret_req.key.clone(), val);
                                    }
                                }
                            }
                        }

                        // The internal token is per process run; a persisted one always 401s.
                        // GIAP_SERVER_URL stays as persisted: the port isn't bound yet.
                        env.insert(
                            pond_api::oauth_callback::INTERNAL_TOKEN_ENV_KEY.to_string(),
                            pond_api::oauth_callback::internal_extension_token().to_string(),
                        );

                        // Old configs may carry cwd-relative args; re-anchor and persist them.
                        let mut args = srv.args.clone();
                        if pond_core::mcp::services::marketplace::anchor_asset_args(
                            &mut args,
                            &asset_root,
                        ) {
                            tracing::info!(
                                extension = %srv.name,
                                "re-anchored persisted extension args to the asset root"
                            );
                            let mut migrated = srv.clone();
                            migrated.args = args.clone();
                            if let Err(e) = repo.save(&migrated).await {
                                tracing::warn!(
                                    "failed to persist re-anchored args for '{}': {e}",
                                    srv.name
                                );
                            }
                        }

                        let req = AddExtensionRequest {
                            name: srv.name.clone(),
                            kind: srv.kind.clone(),
                            description: srv.description.clone(),
                            command: srv.command.clone(),
                            args,
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

    // Only for in-process GGUF; llamafile/Ollama manage their own memory.
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
                    // Future: warm the model here; it otherwise loads on first complete().
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
    let operational_log: Option<
        Arc<dyn pond_core::security::ports::event_log::OperationalLogRepository>,
    > = Some(Arc::new(SqliteOperationalLog::new(db.logs.clone())));

    // Audits as `Auth` events. A separate Arc from `event_log`, so it needs its own chokepoint-2
    // redactor: these rows carry a `token:<client_id>` label and a remote address.
    let security_policy: Option<Arc<dyn pond_core::security::ports::policy::SecurityPolicy>> =
        Some(Arc::new(
            pond_infra::sqlite_security_policy::SqliteSecurityPolicy::new(Arc::new(
                pond_core::security::services::redacting_event_log::RedactingEventLog::new(
                    Arc::new(SqliteEventLog::new(db.logs.clone())),
                    redactor.clone(),
                ),
            )),
        ));

    // WARN+ tracing into the SQLite log; `_file_guard` must live until run_server returns.
    let _file_guard = drain_handle.drain_into(operational_log.clone());

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

    // Bound early: AppState needs the real port for OAuth redirect URIs.
    let (listener, api_port) =
        ports::bind_with_fallback("0.0.0.0", port.unwrap_or(ports::API_SERVER)).await?;

    // For `pond pairing`, which needs the real port (maybe a fallback, or --port). Best-effort.
    let _ = std::fs::write(data_dir.join(".runtime_api_port"), api_port.to_string());

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
        embedding_provider.clone(),
        device_control.clone(),
    )));

    // Chokepoint 2: attributes are redacted on the way in, and a finding raises sensitivity
    // (hidden from audit MCP reads, shorter retention). `set_egress_sink` writes through it too.
    let event_log: Arc<dyn pond_core::security::ports::event_log::EventLog> = Arc::new(
        pond_core::security::services::redacting_event_log::RedactingEventLog::new(
            Arc::new(SqliteEventLog::new(db.logs.clone())),
            redactor.clone(),
        ),
    );
    // Before `db` moves into AppState.
    let push_token_repo: Arc<dyn pond_core::user_data::ports::push_token::PushTokenRepository> =
        Arc::new(pond_infra::sqlite_push_token::SqlitePushTokenRepository::new(db.system.clone()));

    // Fan-out to `/notifications/stream` clients, plus an offline queue and a push relay.
    let (notification_tx, _) =
        tokio::sync::broadcast::channel::<pond_core::mcp::ports::notification::Notification>(256);
    let notification_queue: Arc<
        dyn pond_core::mcp::ports::notification_queue::NotificationQueueRepository,
    > = Arc::new(
        pond_infra::sqlite_notification_queue::SqliteNotificationQueue::new(db.system.clone()),
    );

    // Last resort: otherwise nothing would ever report the slower decode.
    if drafter_wanted && !drafter_ready {
        let notice = pond_core::mcp::ports::notification::Notification {
            id: uuid::Uuid::new_v4().to_string(),
            target: "broadcast".to_string(),
            category: "info".to_string(),
            title: "Running without speculative decoding".to_string(),
            body: format!(
                "Could not fetch the helper model for {}. Chat works as usual, \
                 replies are just slower. It retries on the next start.",
                settings.chat_model
            ),
            timestamp: chrono::Utc::now().to_rfc3339(),
            data: None,
        };
        let _ = notification_queue.enqueue(notice.clone()).await;
        let _ = notification_tx.send(notice);
    }
    // FCM v1 when a service-account key exists (data-only wake pings, no content via Google).
    let fcm_key_path = std::env::var("POND_FCM_KEY_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| data_dir.join("secrets").join("fcm-service-account.json"));
    let push_relay: Arc<dyn pond_core::mcp::ports::notification_relay::NotificationRelay> =
        if fcm_key_path.exists() {
            match pond_infra::fcm_push_relay::FcmPushRelay::from_key_file(
                &fcm_key_path,
                push_token_repo.clone(),
            ) {
                Ok(relay) => Arc::new(relay),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %fcm_key_path.display(),
                        "FCM key unusable; background push falls back to the logging stub"
                    );
                    Arc::new(pond_infra::stub_push_relay::StubPushRelay::new(
                        push_token_repo.clone(),
                    ))
                }
            }
        } else {
            Arc::new(pond_infra::stub_push_relay::StubPushRelay::new(
                push_token_repo.clone(),
            ))
        };
    // Required: without it `send_to_profile` silently answers `AttributionUnavailable`.
    let device_attribution: Arc<
        dyn pond_core::user_data::ports::device_attribution::DeviceAttribution,
    > = Arc::new(
        pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(db.system.clone()),
    );

    // Concrete type kept: `send_to_profile` (profile-addressed, never broadcast) isn't on the port.
    let targeted_notification_sender = Arc::new(
        pond_infra::broadcast_notification_sender::BroadcastNotificationSender::new(
            notification_tx.clone(),
            notification_queue.clone(),
            Some(push_relay),
        )
        .with_device_attribution(device_attribution.clone()),
    );
    let notification_sender: Arc<dyn pond_core::mcp::ports::notification::NotificationSender> =
        targeted_notification_sender.clone();
    // Lets the `send_notification` MCP tool reach connected phones too.
    pond_mcp_server::init_notification_sender(notification_sender.clone());

    // Converge Matter only now: a first enable installs a controller (minutes), and the notice
    // explaining the wait needs the sender. `apply` returns immediately.
    #[cfg(feature = "goose-agent")]
    if let Some(matter) = &matter_concrete {
        matter
            .attach_notifications(notification_sender.clone())
            .await;
        matter.apply(pond_core::user_data::ports::matter_runtime::MatterConfig {
            url: settings.matter_ws_url.trim().to_string(),
            ble: settings.matter_ble_enabled,
        });

        // Bounded so Matter logs precede the "listening" banner; a first-run install isn't awaited.
        const MATTER_STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
        let settled = matter.settle(MATTER_STARTUP_GRACE).await;
        if matches!(
            settled.state,
            pond_core::user_data::ports::matter_runtime::MatterState::Connecting
        ) {
            tracing::info!(
                target: "giap::trace",
                kind = "matter_startup_deferred",
                "matter: still starting; continuing without waiting for it"
            );
        }
    }

    // Schedule results become push notifications, so reminders reach the phone too.
    {
        let mut rx = schedule_result_tx.subscribe();
        let sender = notification_sender.clone();
        tokio::spawn(async move {
            use pond_core::user_data::domain::schedule::RunStatus;
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let (category, body) = match ev.status {
                            RunStatus::Completed => ("info", ev.result.clone().unwrap_or_default()),
                            RunStatus::Failed => (
                                "alert",
                                ev.error
                                    .clone()
                                    .unwrap_or_else(|| "Task failed".to_string()),
                            ),
                            RunStatus::Running => continue, // not user-facing
                        };
                        let notification = pond_core::mcp::ports::notification::Notification {
                            id: uuid::Uuid::new_v4().to_string(),
                            target: "broadcast".to_string(),
                            category: category.to_string(),
                            title: format!("Schedule: {}", ev.schedule_label),
                            body,
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            data: None,
                        };
                        let _ = sender.broadcast(notification).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
    {
        let event_log = event_log.clone();
        let mut events = event_bus.subscribe();
        tokio::spawn(async move {
            use futures::StreamExt;
            while let Some(bus_event) = events.next().await {
                if let Err(e) = event_log.append(bus_event.to_event()).await {
                    tracing::warn!(error = %e, "failed to persist bus event to event log");
                }
            }
        });
    }

    // ── The proactive reviewer ───────────────────────────────────────────
    // A review is its own parent turn: it publishes a turn authority, whose token cascades
    // interruption. It reuses the `delegate` tool's orchestrator; no orchestrator, no review.
    {
        let review_settings = settings_repo.clone();
        let review_storage = session_storage.clone();
        let review_activity = last_user_activity.clone();
        let review_sender = targeted_notification_sender.clone();
        let review_proposals: Arc<dyn pond_core::user_data::ports::proposal::ProposalRepository> =
            Arc::new(pond_infra::sqlite_proposal::SqliteProposalRepository::new(
                db.system.clone(),
            ));

        // Ring drained by the reviewer; sized off pond-core's cap so the two can't drift.
        let observed: Arc<
            tokio::sync::Mutex<
                std::collections::VecDeque<pond_core::user_data::domain::proposal::BusEventRef>,
            >,
        > = Arc::new(tokio::sync::Mutex::new(std::collections::VecDeque::new()));
        {
            let observed = observed.clone();
            let mut events = event_bus.subscribe();
            tokio::spawn(async move {
                use futures::StreamExt;
                use pond_core::user_data::services::proactive_review::{
                    reviewable, MAX_BRIEF_EVENTS,
                };
                let ring_capacity = MAX_BRIEF_EVENTS * 16;
                while let Some(bus_event) = events.next().await {
                    // Filtered on entry so idle heartbeats can't evict real events from the ring.
                    let Some(reference) = reviewable(&bus_event) else {
                        continue;
                    };
                    let mut ring = observed.lock().await;
                    ring.push_back(reference);
                    while ring.len() > ring_capacity {
                        ring.pop_front();
                    }
                }
            });
        }

        tokio::spawn(run_proactive_reviewer(
            review_settings,
            review_storage,
            review_activity,
            review_proposals,
            review_sender,
            observed,
        ));
    }

    // ── The personal-context corpus producer ─────────────────────────────
    // `IngestPipeline::new` takes the redactor by value, so it can't be left out. Own bus
    // subscription, not the reviewer's ring: every event must be seen once, and rings wrap.
    // `account_syncer` is hoisted so the "check now" route and the timer share one syncer.
    let mut account_syncer: Option<Arc<dyn pond_core::context::ports::AccountSync>> = None;
    {
        let context_repo: Arc<dyn pond_core::context::ports::ContextRepository> = Arc::new(
            pond_infra::sqlite_context::SqliteContextRepository::new(
                db.system.clone(),
                redactor.clone(),
            )
            // Mirroring is safe: a ContextItem can only be constructed redacted.
            .with_vector_index(vector_index.clone(), vector_model_id.clone()),
        );
        let pipeline = Arc::new(
            pond_core::context::ingest::IngestPipeline::new(context_repo.clone(), redactor.clone())
                .with_embedder(embedding_provider.clone()),
        );

        // Installed unconditionally: the toggle gates registration. No embedder, no unified
        // retrieval: `recall` then answers nothing rather than degrading to context-only.
        let unified_retrieval = embedding_provider.clone().map(|emb| {
            Arc::new(
                pond_core::context::retrieval_service::PersonalContextRetrieval::new(
                    vector_index.clone(),
                    emb,
                ),
            )
        });
        pond_mcp_server::context::init_context_deps(
            context_repo.clone(),
            embedding_provider.clone(),
            unified_retrieval,
        );

        // Delayed so no first turn waits on a calendar server; calendars rarely change.
        if let Some(secrets) = secret_repo.clone() {
            let syncer = Arc::new(pond_server::account_sync::AccountSyncer::new(
                context_repo.clone(),
                pipeline.clone(),
                secrets.clone(),
            ));
            account_syncer = Some(syncer.clone());
            tokio::spawn(async move {
                const FIRST_RUN_DELAY: std::time::Duration = std::time::Duration::from_secs(90);
                const INTERVAL: std::time::Duration = std::time::Duration::from_secs(30 * 60);
                tokio::time::sleep(FIRST_RUN_DELAY).await;
                loop {
                    match syncer.run(chrono::Utc::now()).await {
                        Ok(report) if report.sources > 0 => tracing::info!(
                            sources = report.sources,
                            unchanged = report.unchanged,
                            ingested = report.ingested,
                            needs_reauth = report.needs_reauth,
                            failed = report.failed,
                            paused = report.paused,
                            "account sync"
                        ),
                        // Silent when nothing is connected, to keep the log worth reading.
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "account sync could not run"),
                    }
                    tokio::time::sleep(INTERVAL).await;
                }
            });
        }

        let ingest = Arc::new(pond_core::context::bus_ingest::BusIngest::new(
            context_repo,
            pipeline,
        ));
        let ingest_settings = settings_repo.clone();
        let mut events = event_bus.subscribe();
        tokio::spawn(async move {
            use futures::StreamExt;
            while let Some(bus_event) = events.next().await {
                // A failed settings read skips the event, never `Settings::default()`: the gate
                // must fail closed even if the default ever flips.
                let Ok(settings) = ingest_settings.get().await else {
                    continue;
                };
                match ingest
                    .absorb(&settings, &bus_event, chrono::Utc::now())
                    .await
                {
                    Ok(report) => {
                        if !report.ingested.is_empty() {
                            tracing::debug!(
                                stored = report.ingested.len(),
                                refused = report.refused,
                                "[context] bus event ingested"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "[context] could not absorb a bus event");
                    }
                }
            }
        });
    }

    // Logs built-in MCP tools' outbound calls; see `GET /api/v1/activity?category=network`.
    pond_mcp_server::set_egress_sink(event_log.clone());

    // Fires SensorTrigger schedules on matching bus events via the scheduler's run_now path.
    if let Some(sched) = scheduler.clone() {
        tokio::spawn(pond_infra_scheduler::run_rules_engine(
            event_bus.subscribe(),
            sched,
        ));
    }

    // Clock and presence publishers; both hold the bus weakly (see `run_time_ticker`).
    tokio::spawn(run_time_ticker(Arc::downgrade(&event_bus)));
    tokio::spawn(run_session_activity_observer(
        Arc::downgrade(&event_bus),
        session_storage.clone(),
        profile_repo.clone(),
        last_user_activity.clone(),
    ));

    // On-device motion detection into camera_events + the bus. Opt-in: needs a camera and ffmpeg.
    if settings.vision_enabled && !settings.vision_camera_url.trim().is_empty() {
        let capture = pond_adapters_vision::CaptureConfig {
            input: settings.vision_camera_url.trim().to_string(),
            fps: settings.vision_fps.max(1),
            ..Default::default()
        };
        let pipeline_cfg = pond_adapters_vision::VisionPipelineConfig {
            camera_id: settings.vision_camera_id.clone(),
            motion: pond_adapters_vision::MotionConfig {
                changed_fraction: settings.vision_motion_threshold.clamp(0.001, 1.0),
                ..Default::default()
            },
            // Triggering frame per event (bounded per camera), so the dashboard shows what moved.
            snapshots: Some(pond_adapters_vision::SnapshotConfig::new(
                data_dir.join("snapshots"),
            )),
            ..Default::default()
        };

        // Labels motion as person/pet/package. Empty model = auto-downloaded YOLOX-Nano; a set
        // one is operator-managed. Any failure degrades to plain "motion".
        #[cfg(feature = "vision-onnx")]
        let classifier: Option<
            std::sync::Arc<dyn pond_core::user_data::ports::vision::VisionClassifier>,
        > = {
            let configured = settings.vision_classifier_model.trim();
            let resolved = if configured.is_empty() {
                model_download::download_vision_classifier(&data_dir)
                    .await
                    .map_err(|e| {
                        tracing::warn!("vision classifier auto-download failed: {e:#}");
                    })
                    .ok()
            } else {
                let path = std::path::Path::new(configured);
                Some(if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    model_download::vision_models_dir(&data_dir).join(path)
                })
            };
            resolved.and_then(|model_path| {
                match pond_adapters_vision_onnx::OnnxVisionClassifier::new(&model_path) {
                    Ok(c) => Some(
                        std::sync::Arc::new(c)
                            as std::sync::Arc<
                                dyn pond_core::user_data::ports::vision::VisionClassifier,
                            >,
                    ),
                    Err(e) => {
                        tracing::warn!(error = %e, "vision classifier unavailable; events stay \"motion\"");
                        None
                    }
                }
            })
        };
        #[cfg(not(feature = "vision-onnx"))]
        let classifier = {
            if !settings.vision_classifier_model.trim().is_empty() {
                tracing::warn!(
                    "vision_classifier_model is set but this build lacks the `vision-onnx` \
                     feature; events stay \"motion\""
                );
            }
            None
        };

        match pond_adapters_vision::FfmpegFrameSource::spawn(&capture) {
            Ok(source) => {
                let storage = camera_storage.clone();
                let bus = event_bus.clone();
                tokio::spawn(pond_adapters_vision::run_vision_pipeline(
                    Box::new(source),
                    classifier,
                    storage,
                    bus,
                    pipeline_cfg,
                ));
            }
            Err(e) => {
                tracing::warn!(error = %e, "vision pipeline not started (camera/ffmpeg unavailable)")
            }
        }
    }

    // Before `db` moves into AppState.
    let handshake: Arc<dyn pond_core::security::ports::handshake::Handshake> =
        Arc::new(SqliteHandshakeAdapter::new(db.system.clone()));
    let pairing_hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "pond".to_string());
    let pairing_hostname = pairing_hostname
        .strip_suffix(".local")
        .unwrap_or(&pairing_hostname)
        .to_string();
    match handshake.issue_pairing_code().await {
        Ok(pc) => {
            let pair_url = format!(
                "pond://pair?host={}.local&port={}&code={}",
                pairing_hostname, api_port, pc.code
            );
            println!("\n  ┌────────────────────────────────────────────────────┐");
            println!(
                "  │  Pairing code:  {}   (valid 10 min)           │",
                pc.code
            );
            println!("  │  Scan with Goose On The Go or enter the code.     │");
            println!("  └────────────────────────────────────────────────────┘");
            print_pairing_qr(&pair_url);
            println!();
        }
        Err(e) => tracing::warn!("failed to issue pairing code at startup: {e:#}"),
    }

    let state = Arc::new(AppState {
        tts_control: tts_control.clone(),
        db,
        onboarding_repo,
        handshake: handshake.clone(),
        whisper_url: whisper_url.clone(),
        transcribe_audio,
        session_storage,
        http_client: reqwest::Client::new(),
        agent,
        warmup: Default::default(),
        llm_provider,
        llamafile_url: llamafile_url.clone(),
        tts,
        settings_repo,
        profile_repo,
        matter: matter_runtime.clone(),
        device_registry,
        memory_repo,
        embedding_provider,
        // Same handle the repos write through and the sweep repairs; never construct a second one.
        vector_index: Some(vector_index.clone()),
        // Some only if the sweep spawned, or the route would report `refilling: true` forever.
        index_reindex: index_sweep_running.then(|| index_reindex_requested.clone()),
        account_sync: account_syncer.clone(),
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
        download_tracker: download_tracker.clone(),
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
        operational_log: operational_log,
        event_bus: Some(event_bus.clone()),
        event_log: Some(event_log.clone()),
        push_token_repo: Some(push_token_repo.clone()),
        face_recognition,
        runs: Arc::new(pond_api::runs::RunSupervisor::default()),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        // Own, larger pool so long-lived phone streams never starve interactive chat SSE.
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(32)),
        answer_reviewer: answer_reviewer_for_http,
        memory_extractor: memory_extractor_for_http,
        memory_extraction_service: memory_extraction_service_for_http,
        last_user_activity: last_user_activity.clone(),
        consolidation_cancel: consolidation_cancel.clone(),
        consolidation_event_tx: consolidation_event_tx.clone(),
        consolidation_runner,
        inference_pool,
        schedule_result_tx: schedule_result_tx.clone(),
        notification_tx: notification_tx.clone(),
        notification_queue: Some(notification_queue.clone()),
        notification_sender: Some(notification_sender.clone()),
        telemetry,
        context_monitor: Arc::new(
            pond_core::models::services::context_monitor::ContextMonitor::new(),
        ),
        mcp_app_resources: pond_mcp_server::all_app_resources()
            .into_iter()
            .map(|(uri, html)| (uri.to_string(), html))
            .collect(),
        oauth_state: pond_api::oauth_callback::new_oauth_state(),
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
        security_policy,
        api_port,
        weather_provider: weather.clone(),
        peer_directory,
        credit_ledger,
        usage_tally,
        mesh_transport,
        mesh_provider: mesh_provider.clone(),
        peer_capability_query,
        mesh_rebuild: Some(mesh_rebuild),
    });

    // OAuth refresh updates secrets only; restarting extensions would break in-flight tool calls.
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

                    // On refusal skip this tick only; the user may loosen network_mode later.
                    let call = match pond_core::shared::services::egress::begin(
                        &provider.token_url,
                        "POST",
                    ) {
                        Ok(call) => call,
                        Err(denied) => {
                            tracing::warn!(
                                provider = %provider.id,
                                "OAuth auto-refresh skipped: {denied}"
                            );
                            continue;
                        }
                    };

                    let refreshed = refresh_http_client
                        .post(&provider.token_url)
                        .form(&[
                            ("grant_type", "refresh_token"),
                            ("refresh_token", refresh_token.as_str()),
                            ("client_id", client_id.as_str()),
                        ])
                        .send()
                        .await;
                    call.finish(refreshed.as_ref().ok().map(|r| r.status().as_u16()));

                    match refreshed {
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

    // Named binding, not `_`: dropping the handle deregisters the mDNS service.
    let _mdns_handle =
        match mdns_advertiser::advertise(&pairing_hostname, api_port, env!("CARGO_PKG_VERSION")) {
            Ok(h) => {
                println!("  📡 mDNS: advertising _pond._tcp.local. on port {api_port}");
                Some(h)
            }
            Err(e) => {
                tracing::warn!("mDNS advertisement failed (LAN discovery disabled): {e:#}");
                None
            }
        };

    if !pond_api::web_ui_embedded() && !static_dir.exists() {
        tracing::warn!(
            "No web UI embedded and static dir {:?} not found — the dashboard will \
             not be served. Build the UI (`cd pond-desktop && npm run build`) before \
             building the server to embed it, or pass an existing --static-dir.",
            static_dir
        );
    }

    // Load the model and prefill the static prompt prefix at boot, so turn 1 reuses it.
    pond_api::spawn_prefix_prewarm(state.clone(), false);

    let app = pond_api::build_router(state, static_dir);

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

    if open || (debug && has_display()) {
        let url = format!("http://localhost:{}", api_port);
        if webbrowser::open(&url).is_err() {
            tracing::warn!("Could not open browser — no display available or xdg-open missing");
        }
    }

    if native {
        spawn_desktop_app(api_port).await;
    }

    // Race rather than graceful shutdown: long-lived SSE streams would hold it open forever.
    tokio::select! {
        result = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        ) => {
            result?;
        }
        _ = shutdown_signal() => {
            tracing::info!("shutdown signal received — stopping");
        }
    }

    // kill_on_drop doesn't fire on the signal path; kill matter-server here or it orphans.
    if let Some(matter) = &matter_runtime {
        matter.shutdown().await;
    }

    Ok(())
}

/// Resolves on Ctrl-C, or SIGTERM on Unix (what `systemctl stop` and `docker stop` send).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Print a Unicode QR code for `url` to stdout, indented to match the startup banner.
fn print_pairing_qr(url: &str) {
    use qrcode::render::unicode;
    use qrcode::QrCode;

    match QrCode::new(url.as_bytes()) {
        Ok(code) => {
            let image = code
                .render::<unicode::Dense1x2>()
                .dark_color(unicode::Dense1x2::Light)
                .light_color(unicode::Dense1x2::Dark)
                .quiet_zone(true)
                .build();
            for line in image.lines() {
                println!("  {line}");
            }
        }
        Err(e) => tracing::warn!("QR code generation failed: {e}"),
    }
}

/// Launches the first desktop shell found, in this order:
///   1. `$GIAP_DESKTOP_BIN`                                    — explicit override
///   2. `/Applications/Goose In A Pond.app/...`                — installed
///   3. `pond-desktop/release/mac-*/Goose In A Pond.app/...`   — local package
///   4. `pond-desktop/node_modules/.bin/electron`              — dev, unpackaged
#[cfg(target_os = "macos")]
async fn spawn_desktop_app(server_port: u16) {
    const APP_SUFFIX: &str = "Goose In A Pond.app/Contents/MacOS/Goose In A Pond";

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(explicit) = std::env::var("GIAP_DESKTOP_BIN") {
        if !explicit.is_empty() {
            candidates.push(std::path::PathBuf::from(explicit));
        }
    }
    candidates.push(std::path::PathBuf::from("/Applications").join(APP_SUFFIX));
    // electron-builder writes into pond-desktop/release/mac-<arch>/.
    for arch in ["mac-arm64", "mac", "mac-x64"] {
        candidates.push(
            std::path::PathBuf::from("pond-desktop/release")
                .join(arch)
                .join(APP_SUFFIX),
        );
    }

    if let Some(path) = candidates.iter().find(|p| p.exists()) {
        tracing::info!("Launching native desktop app: {}", path.display());
        launch_desktop(path, &[], server_port).await;
        return;
    }

    // Dev Electron, unlike a packaged bundle, needs the app directory as its argument.
    let dev_electron = std::path::PathBuf::from("pond-desktop/node_modules/.bin/electron");
    if dev_electron.exists() {
        tracing::info!("Launching the desktop app through the dev Electron runtime");
        launch_desktop(&dev_electron, &["pond-desktop"], server_port).await;
        return;
    }

    tracing::warn!(
        "--native: no desktop app found. Build it first:\n  cd pond-desktop && npm run bundle:app\nor for dev:\n  cd pond-desktop && npm run dev:electron"
    );
}

#[cfg(target_os = "macos")]
async fn launch_desktop(path: &std::path::Path, args: &[&str], server_port: u16) {
    let mut child = match std::process::Command::new(path)
        .args(args)
        // Makes the shell attach to this server rather than spawn a rival on the same port.
        .env("GIAP_SERVER_PORT", server_port.to_string())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(
                "Failed to launch the desktop app at {}: {e}",
                path.display()
            );
            return;
        }
    };
    let pid = child.id();

    // Wait, then check it survived: a second instance quits silently (single-instance lock).
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    match child.try_wait() {
        Ok(Some(status)) => tracing::warn!(
            "--native: the desktop app exited immediately (pid {pid}, {status}). \
             The likely cause is that an instance is ALREADY RUNNING -- the app holds a \
             single-instance lock, so a second launch quits at once and raises the existing \
             window instead. That window is attached to whichever server started it, not to \
             this one on port {server_port}. Quit the running app and retry, or just open \
             http://127.0.0.1:{server_port} in a browser."
        ),
        Ok(None) => tracing::info!("Desktop app running (pid {pid})"),
        Err(e) => tracing::debug!("could not check on the desktop app (pid {pid}): {e}"),
    }
}

/// Off macOS there is no desktop shell; on Linux the served dashboard is the UI.
#[cfg(not(target_os = "macos"))]
async fn spawn_desktop_app(server_port: u16) {
    tracing::warn!(
        "--native: the desktop shell is macOS-only. This server's dashboard is already \
         available at http://127.0.0.1:{server_port} and on the LAN."
    );
}

/// Writes one NDJSON line; every `--json-events` emission goes through it so framing can't drift.
fn write_ndjson_line(ev: &pond_core::shared::domain::agent::WorkflowEvent) {
    use std::io::Write as _;
    if let Some(line) = ev.to_ndjson() {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

/// Returns the Silero VAD, or `None` (energy gate) on any failure: a worse VAD, never a deaf pond.
/// Diagnostics use stderr: `out!` is a no-op under `--json-events`, the desktop's only mode.
async fn build_speech_detector(
    vad_backend: &str,
    data_dir: &std::path::Path,
) -> Option<Box<dyn pond_voice::dsp::SpeechDetector + Send>> {
    if vad_backend.eq_ignore_ascii_case("rms") {
        // The escape hatch, for a board whose ONNX Runtime is broken.
        return None;
    }
    if !vad_backend.eq_ignore_ascii_case("silero") {
        // A hand-edited DB can land here: `apply_key` skips the API's `VAD_BACKENDS` validation.
        eprintln!("  Listen   unknown vad_backend \"{vad_backend}\" — using the energy gate.");
        return None;
    }

    // Fetched before `ready`, unlike Kokoro: it's 2 MB and `chat --voice` must work with no server.
    let path = model_download::ensure_silero_model(data_dir).await?;

    // Bounded: with no dylib, ort's `load-dynamic` init hangs forever instead of failing.
    const LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
    let loading = tokio::time::timeout(
        LOAD_TIMEOUT,
        tokio::task::spawn_blocking(move || pond_adapters_silero::SileroDetector::new(&path)),
    )
    .await;

    match loading {
        Ok(Ok(Ok(detector))) => {
            tracing::info!("silero VAD active");
            Some(Box::new(detector) as Box<dyn pond_voice::dsp::SpeechDetector + Send>)
        }
        Ok(Ok(Err(e))) => {
            eprintln!("  Listen   silero VAD failed to load: {e}");
            eprintln!("           Using the energy gate.");
            None
        }
        Ok(Err(e)) => {
            eprintln!("  Listen   silero VAD load panicked: {e}");
            None
        }
        Err(_) => {
            eprintln!(
                "  Listen   silero VAD load timed out after {}s — the ONNX Runtime is \
                 probably missing or version-incompatible.",
                LOAD_TIMEOUT.as_secs()
            );
            eprintln!("           Using the energy gate.");
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_chat(
    provider: Option<&str>,
    model: Option<&str>,
    voice_mode: bool,
    wake_word: Option<&str>,
    no_wake_word: bool,
    tts: Option<&str>,
    session_id_arg: Option<&str>,
    json_events: bool,
) -> Result<()> {
    // Under `--json-events` stdout is NDJSON only, so human output must go through `out!`.
    macro_rules! out {
        ($($arg:tt)*) => {
            if !json_events {
                println!($($arg)*);
            }
        };
    }

    macro_rules! eout {
        ($($arg:tt)*) => {
            eprintln!($($arg)*);
        };
    }

    out!(
        "\n  Goose in a Pond {} — voice\n",
        env!("CARGO_PKG_VERSION")
    );

    // Taken before any audio device opens: two sessions on one device fight over mic and speaker.
    let _voice_lock = match voice_lock::VoiceLock::acquire() {
        Ok(lock) => lock,
        Err(e) => {
            // `main` prints the error; the desktop shell reads only these NDJSON events.
            if json_events {
                use pond_core::shared::domain::agent::WorkflowEvent;
                write_ndjson_line(&WorkflowEvent::Error {
                    message: e.to_string(),
                });
                write_ndjson_line(&WorkflowEvent::Exit {
                    reason: "already_running".to_string(),
                });
            }
            return Err(e);
        }
    };

    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;

    // Needed on this path too, or giap-sensors fails to load in voice sessions.
    pond_mcp_server::init_sensor_deps(
        Arc::new(SqliteSensorStorage::new(db.logs.clone())),
        Arc::new(SqliteDeviceRegistry::new(db.system.clone())),
        // No Matter here: `state` bails, so replies use the stored reading, labelled with its age.
        Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
    );

    // Needed here too, or giap-context fails to load. No index or embedder on this path, so
    // `recall` returns nothing rather than degrading to context-only results.
    pond_mcp_server::context::init_context_deps(
        Arc::new(pond_infra::sqlite_context::SqliteContextRepository::new(
            db.system.clone(),
            Arc::new(pond_infra::rule_redactor::RuleRedactor::new()),
        )),
        None,
        None,
    );

    let settings_repo_chat = SqliteSettingsRepository::new(db.system.clone());
    let settings = settings_repo_chat.get().await.unwrap_or_default();

    // The mode is a process-global defaulting to `Open`; each entry point must install it.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(&settings.network_mode),
    );

    // Before any ONNX init (else ORT_DYLIB_PATH is unset and Piper::new() hangs), and after the
    // egress mode install, since it may download ~100 MB.
    ensure_onnx_runtime();

    let settings_provider = settings.chat_provider.clone();
    let effective_provider: &str = provider.unwrap_or(&settings_provider);

    let settings_model = settings.chat_model.clone();
    let effective_model: &str = model.unwrap_or(&settings_model);

    // Apply the microphone privacy setting before anything can open a device.
    pond_core::models::domain::mic_gate::set_mic_enabled(settings.mic_enabled);

    // Every capture path must use this one mic owner. 15_000 ms covers the wake-word detector's
    // ~13.4 s history with headroom (see `pond_audio::spawn`).
    let (mic_handle, _mic_owner_join) = pond_audio::spawn(
        Box::new(pond_audio::CpalCapture::new()),
        pond_audio::CAPTURE_RATE_HZ,
        15_000,
        settings.mic_enabled,
    );

    let voice_models = voice_models::resolve_voice_models(
        &settings,
        &SqliteModelRepository::new(db.system.clone()),
        &data_dir,
    )
    .await;

    // Held until after `ready`, which the NDJSON contract requires to be the first line.
    let deferred_diagnostics: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());

    let effective_tts_owned: String;
    let effective_tts: &str = match tts {
        Some(t) => t,
        None => {
            // Always Kokoro; `active_tts_model` is a voice name ("af_heart"), not an engine.
            effective_tts_owned = "kokoro".to_string();
            &effective_tts_owned
        }
    };

    // Only under --voice: a text session shouldn't wait on a 142 MB whisper download.
    let whisper_model_path: Option<std::path::PathBuf> = if voice_mode {
        match voice_models.whisper.as_ref() {
            None => {
                let name = settings.active_whisper_model.as_str();
                let reason = if name.is_empty() {
                    "no STT model configured in Settings".to_string()
                } else {
                    format!("STT model '{name}' does not match any catalog entry or file on disk")
                };
                out!("  Listen   unavailable — {reason}");
                if json_events {
                    deferred_diagnostics
                        .borrow_mut()
                        .push(format!("voice input unavailable: {reason}"));
                }
                None
            }
            Some(w) => {
                if !w.path.exists() {
                    out!("  Listen   speech model missing — downloading...");
                    match w.download.as_ref() {
                        Some(dl) => {
                            if let Some(parent) = w.path.parent() {
                                let _ = tokio::fs::create_dir_all(parent).await;
                            }
                            if let Err(e) =
                                model_download::download_file(&dl.url, &w.path, dl.size_mb).await
                            {
                                out!("  Listen   speech model download failed: {}", e);
                            }
                        }
                        None => out!("  Listen   speech model has no download URL"),
                    }
                }
                Some(w.path.clone())
            }
        }
    } else {
        None
    };

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

    // Other providers run their own process or need none.
    let mut llamafile_port = ports::llamafile_port();
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

    let session_id = session_id_arg.unwrap_or("default-session").to_string();

    // ── Build repos for GooseAdapter (before db.system is consumed) ───────────────
    let settings_repo_arc: Arc<
        dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync,
    > = Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    // Same chokepoint as the server: this backend registers `giap-memory`. Model id `None` as
    // there's no embedder here; the index is still needed so `delete` removes the vector too.
    let memory_repo: Arc<
        dyn pond_core::user_data::ports::memory_repository::MemoryRepository + Send + Sync,
    > = Arc::new(
        pond_core::user_data::services::redacting_memory_repository::RedactingMemoryRepository::new(
            Arc::new(
                SqliteMemoryRepository::new(db.system.clone()).with_vector_index(
                    Arc::new(pond_infra::sqlite_vector_index::SqliteVectorIndex::new(
                        db.vectors.clone(),
                    )),
                    None,
                ),
            ),
            Arc::new(pond_infra::rule_redactor::RuleRedactor::new()),
        ),
    );
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

    // Upsert so existing installs get updated built-ins; user templates are never touched.
    {
        use pond_core::prompts::BUILTIN_PROMPT_TEMPLATES;
        use pond_core::user_data::domain::prompt_template::PromptTemplate;
        #[allow(unused_imports)]
        use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
        for &(name, content, description) in BUILTIN_PROMPT_TEMPLATES {
            let t = PromptTemplate {
                name: name.to_string(),
                content: content.to_string(),
                description: description.to_string(),
                is_system: true,
                is_customized: false,
                factory_version: pond_core::user_data::domain::prompt_template::FACTORY_VERSION,
                updated_at: String::new(),
            };
            if let Err(e) = template_repo.seed_system_template(&t).await {
                tracing::warn!("Failed to reseed built-in prompt template '{name}': {e}");
            }
        }
        tracing::info!("Built-in prompt templates reseeded (Jinja2 general-purpose copilot)");
    }

    let weather: Option<Arc<dyn WeatherProvider>> = match (
        settings.weather_enabled,
        pond_core::user_data::services::location::resolve(&settings).weather_target(),
    ) {
        (true, Some((lat, lon, loc))) => {
            Some(Arc::new(OpenMeteoWeatherAdapter::new(lat, lon, loc)))
        }
        _ => None,
    };

    // ── Build the GooseAdapter (MCP tools + model routing) ───────────────────────
    #[cfg(feature = "goose-agent")]
    let agent: Arc<dyn Agent> = {
        // Persist CLI overrides for GooseAdapter, but never "mock": a later `serve` would use it.
        if (provider.is_some() || model.is_some()) && effective_provider != "mock" {
            let mut s = settings.clone();
            s.chat_provider = effective_provider.to_string();
            s.chat_model = effective_model.to_string();
            settings_repo_arc.update(&s).await.ok();
        }
        // `mock` runs the loop offline and deterministically, for the json-events contract test.
        let agent_backend = if effective_provider == "mock" {
            "mock"
        } else {
            "goose"
        };
        let trim_storage: Arc<dyn SessionStorage> =
            Arc::new(SqliteSessionStorage::new(db.system.clone()));
        let (a, _ext_mgr, _tc, _tr) = build_goose_backend(
            agent_backend,
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
            Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
            Some(trim_storage), // powers the trimmer's summary splice
            Some(chat_model_repo.clone()),
            voice_mode,
            Arc::new(tokio::sync::RwLock::new(None)), // mesh_provider: no mesh stack in CLI chat
        )
        .await;
        a
    };

    #[cfg(not(feature = "goose-agent"))]
    let agent: Arc<dyn Agent> = Arc::new(MockAgent::new());

    // `$DATA_DIR/prompts/system.md` is a deployment override of the built prompt.
    let system_prompt = {
        let prompt_dir = data_dir.join("prompts");
        let file_template = std::fs::read_to_string(prompt_dir.join("system.md")).ok();
        match file_template {
            Some(tmpl) => {
                out!(
                    "  Prompt:   custom ({})",
                    prompt_dir.join("system.md").display()
                );
                let name = pond_core::prompts::sanitize_field(&settings.assistant_name, 50);
                let user = pond_core::prompts::sanitize_field(&settings.user_name, 50);
                let persona =
                    pond_core::prompts::sanitize_field(&settings.assistant_personality, 200);
                let tz = pond_core::prompts::sanitize_field(&settings.timezone, 50);
                // Via the resolver, like `prompts.rs`, so both prompt paths agree on the location.
                let location =
                    match pond_core::user_data::services::location::resolve(&settings).describe() {
                        Some(place) => format!(
                            "\nLocation: {}.",
                            pond_core::prompts::sanitize_field(place, 100)
                        ),
                        None => String::new(),
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
            None => build_system_prompt(&settings),
        }
    };

    let storage: Arc<dyn SessionStorage> = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    if let Err(e) = storage.create_session(session_id.clone()).await {
        match e {
            pond_core::user_data::ports::session_storage::SessionStorageError::StorageError(_) => {
                tracing::debug!("Session already exists, reusing: {}", session_id);
            }
            other => return Err(other.into()),
        }
    }

    let warm_agent = agent.clone();
    let mut chat_service = ChatService::new(agent, session_id.clone(), storage)
        .with_system_prompt(system_prompt)
        .with_thinking_tone(settings.voice_thinking_tone_enabled);
    if let Some(model_name) = model {
        chat_service = chat_service.with_model_name(model_name);
    }
    // Voice turns land in the same turn_metrics table as the REST path.
    match SqliteTelemetry::new(db.logs.clone()).await {
        Ok(telemetry) => chat_service = chat_service.with_telemetry(Arc::new(telemetry)),
        Err(e) => tracing::warn!("voice telemetry disabled (init failed): {e}"),
    }

    // ── NDJSON event sink (--json-events) ──────────────────────────────────────
    if json_events {
        let sink: pond_core::shared::services::chat::WorkflowEventSink =
            Arc::new(|event: &pond_core::shared::domain::agent::WorkflowEvent| {
                write_ndjson_line(event);
            });
        chat_service = chat_service
            .with_event_sink(sink)
            .with_stdout_diagnostics(false);
    }

    // Drives the desktop voice orb; only the shell consumes it, so --json-events only.
    let audio_level_sink: Option<Arc<pond_adapters_whisper::ThrottledAudioLevelSink>> =
        if json_events {
            Some(Arc::new(
                pond_adapters_whisper::ThrottledAudioLevelSink::new(Box::new(|rms: f32| {
                    write_ndjson_line(
                        &pond_core::shared::domain::agent::WorkflowEvent::AudioLevel { rms },
                    );
                })),
            ))
        } else {
            None
        };

    // ── Wire LLM provider (no-goose-agent fallback only) ────────────────────────
    #[cfg(not(feature = "goose-agent"))]
    match effective_provider {
        "ollama" => {
            out!(
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
                out!("  Model    {} (local)", hf_model_id);
                let llm = Arc::new(
                    LocalInferenceLlmAdapter::new_with_data_dir(&hf_model_id, &data_dir).await?,
                );
                chat_service = chat_service.with_provider(llm);
            }
            #[cfg(not(feature = "local-inference"))]
            {
                eout!(
                    "  WARN: --provider local requires the `local-inference` feature (not compiled in).\n\
                     Falling back to llamafile. Rebuild with:\n  \
                     cargo run -p pond-server --features local-inference -- chat --provider local"
                );
                out!(
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
            out!(
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
    let whisper_backend: Option<Arc<WhisperRsInput>> = if voice_mode {
        match &whisper_model_path {
            Some(p) => match WhisperRsInput::new(p.clone(), mic_handle.clone()) {
                Ok(w) => {
                    let w = match &audio_level_sink {
                        Some(sink) => w.with_audio_level_sink(sink.clone()),
                        None => w,
                    };
                    Some(Arc::new(w))
                }
                Err(e) => {
                    // NDJSON error too: the shell never writes stdin, so this session is deaf.
                    eout!("  Listen   FAILED to load speech model: {}", e);
                    eout!("           Falling back to typed input.");
                    out!("  Listen   FAILED to load speech model: {}", e);
                    out!("           Falling back to typed input.");
                    if json_events {
                        write_ndjson_line(
                            &pond_core::shared::domain::agent::WorkflowEvent::Error {
                                message: format!(
                                    "voice input unavailable: whisper model failed to load ({e}); falling back to stdin"
                                ),
                            },
                        );
                    }
                    None
                }
            },
            None => {
                // No usable model (warned above via out!); same deaf-session hazard.
                eout!("  Listen   speech model unavailable — falling back to typed input.");
                if json_events {
                    write_ndjson_line(&pond_core::shared::domain::agent::WorkflowEvent::Error {
                        message:
                            "voice input unavailable: whisper model missing; falling back to stdin"
                                .to_string(),
                    });
                }
                None
            }
        }
    } else {
        None
    };

    // Set on the shared backend, so both voice input and the wake-word detector use it.
    if let Some(backend) = &whisper_backend {
        if let Some(detector) = build_speech_detector(&settings.vad_backend, &data_dir).await {
            backend.set_speech_detector(detector);
        }
    }

    let voice: Arc<dyn VoiceInput> = match &whisper_backend {
        Some(backend) => {
            out!(
                "  Listen   {}",
                voice_models
                    .whisper
                    .as_ref()
                    .and_then(|w| w.path.file_stem())
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "whisper".to_string())
            );
            backend.clone() as Arc<dyn VoiceInput>
        }
        None => {
            out!("  Listen   typed input (no speech model)");
            Arc::new(StdinInput::new())
        }
    };
    chat_service = chat_service.with_voice_input(voice);

    // ── Wire wake word detector ──
    if no_wake_word || whisper_backend.is_none() {
        chat_service = chat_service.with_wake_word_detector(Arc::new(InstantActivation));
    } else {
        let backend = whisper_backend.clone().expect("checked above");
        let trigger = wake_word.unwrap_or(settings.voice_wake_word.as_str());
        let transcriptions = settings.voice_wake_word_transcriptions.clone();

        if transcriptions.is_empty() {
            out!("  Wake     \"{}\"", trigger);
        } else {
            out!(
                "  Wake     \"{}\" ({} calibrated variants)",
                trigger,
                transcriptions.len()
            );
        }
        use pond_adapters_whisper::{KeywordDetectorConfig, WhisperBackend};
        let kws_config = KeywordDetectorConfig {
            energy_threshold: settings.voice_kws_energy_threshold,
            post_trigger_silence_ms: settings.voice_kws_post_trigger_silence_ms,
            cooldown_ms: settings.voice_kws_cooldown_ms,
            ..KeywordDetectorConfig::default()
        };

        let mut detector_builder = WhisperKeywordDetector::new(
            backend.clone() as Arc<dyn WhisperBackend>,
            trigger,
            mic_handle.clone(),
        )
        .with_transcriptions(transcriptions)
        .with_config(kws_config);
        if let Some(sink) = &audio_level_sink {
            detector_builder = detector_builder.with_audio_level_sink(sink.clone());
        }
        let detector = Arc::new(detector_builder);
        // The command clip contains the wake word; strip exactly the triggers the detector used.
        backend.set_wake_words(detector.triggers());
        chat_service = chat_service.with_wake_word_detector(detector);
    };

    // ── Wire barge-in energy ──
    // Required: the default `NoEnergy` is inert, so barge-in by speaking would never fire.
    chat_service =
        chat_service.with_speech_energy(Arc::new(pond_audio::MicEnergy::new(&mic_handle)));

    // ── Wire TTS output ──
    // Never stdout under --json-events: the text already streams as NDJSON `token` events.
    let text_fallback = || -> Arc<dyn VoiceOutput> {
        if json_events {
            Arc::new(SilentOutput)
        } else {
            Arc::new(PrintOutput)
        }
    };
    // Also reports why: under --json-events silent TTS otherwise looks exactly like working TTS.
    let tts_unavailable = |reason: &str| -> Arc<dyn VoiceOutput> {
        if json_events {
            deferred_diagnostics.borrow_mut().push(format!(
                "voice output unavailable: {reason}; response is text-only"
            ));
        }
        text_fallback()
    };
    let voice_out: Arc<dyn VoiceOutput> = match effective_tts {
        // Voice mode runs in-process, not via the server, so Kokoro is wired here as in `serve`.
        "kokoro" => {
            // Never downloads (`serve` does): 92 MB before `ready` would look like a hang.
            let kdir = model_download::kokoro_dir(&data_dir);
            let espeak_data_dir = {
                let p = model_download::piper_espeak_data_path(&data_dir);
                p.exists().then_some(p)
            };
            let cfg = pond_adapters_kokoro::KokoroConfig {
                // Resolved as in `serve`: never load a tier that is silent here.
                model_path: kdir.join(pond_adapters_kokoro::model_filename(
                    pond_adapters_kokoro::usable_quality(&settings.voice_tts_quality),
                )),
                voices_dir: kdir.join("voices"),
                tokenizer_path: kdir.join("tokenizer.json"),
                intra_threads: Some(pond_adapters_kokoro::default_intra_threads()),
                espeak_data: espeak_data_dir,
            };

            if !cfg.tokenizer_path.exists() || !cfg.model_path.exists() {
                out!("  Speak    voice engine not installed — start the pond once to fetch it");
                tts_unavailable("kokoro engine is not installed in this data dir")
            } else {
                match pond_adapters_kokoro::KokoroOutput::new(cfg) {
                    Ok(out) => {
                        let voice = settings.voice_tts_voice.trim();
                        if !voice.is_empty() && out.set_voice(voice).await.is_err() {
                            // The default voice is always fetched; this never degrades to silence.
                            out!(
                                "  Speak    voice '{}' unavailable — using the default",
                                voice
                            );
                        }
                        out.set_speed(settings.voice_tts_speed);
                        let out = match &audio_level_sink {
                            Some(sink) => out.with_audio_level_sink(sink.clone()),
                            None => out,
                        };
                        out!("  Speak    {} @ {:.2}x", out.voice().await, out.speed());
                        Arc::new(out) as Arc<dyn VoiceOutput>
                    }
                    Err(e) => {
                        out!("  Speak    voice failed to load ({e}) — printing replies instead");
                        tts_unavailable(&format!("kokoro load failed: {e}"))
                    }
                }
            }
        }

        // `--tts none` is a documented choice, not a failure. Stay quiet.
        "none" => {
            out!("  Speak    off (--tts none) — replies are printed");
            text_fallback()
        }
        other => {
            // Not a user choice (the desktop never picks one), so report it rather than go silent.
            out!("  Speak    off — replies are printed");
            // The voice comes from `voice_tts_voice`, so blame that setting before `other`.
            let configured_voice = settings.voice_tts_voice.trim();
            let reason = if !configured_voice.is_empty() {
                format!("voice '{configured_voice}' is not installed and matches no catalog entry")
            } else if !other.is_empty() {
                format!("TTS '{other}' did not resolve to an installed piper voice")
            } else {
                "no TTS voice configured".to_string()
            };
            tts_unavailable(&reason)
        }
    };
    chat_service = chat_service.with_voice_output(voice_out.clone());

    // The console is pinned to WARN, so point at the full log file.
    out!(
        "  Log      {}",
        data_dir.join("logs").join("pond.log").display()
    );

    // ── Prefix warm-up + spoken readiness ─────────────────────────────────────
    // Pay model load and prefill before the first utterance; the greeting signals "speak now".
    {
        use pond_core::models::ports::agent::WarmupPhase;
        use pond_core::shared::domain::agent::WorkflowEvent;
        let will_warm = effective_provider != "mock"
            && matches!(settings.chat_provider.as_str(), "local" | "gguf")
            && std::env::var("POND_DISABLE_PREWARM").as_deref() != Ok("1");
        if will_warm {
            if json_events {
                write_ndjson_line(&WorkflowEvent::Warmup {
                    state: "warming".to_string(),
                });
            }
            let _ = voice_out.speak("Warming up.").await;
            let last = Arc::new(std::sync::Mutex::new(None::<WarmupPhase>));
            let sink = last.clone();
            warm_agent
                .prewarm(
                    true, // voice prompt: the warmed prefix must match voice turns
                    Arc::new(move |phase| {
                        *sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(phase);
                    }),
                )
                .await;
            let state = match last.lock().unwrap_or_else(|e| e.into_inner()).take() {
                Some(WarmupPhase::Ready) => "ready",
                Some(WarmupPhase::Skipped { .. }) => "skipped",
                _ => "failed",
            };
            if json_events {
                write_ndjson_line(&WorkflowEvent::Warmup {
                    state: state.to_string(),
                });
            }
        }
        let name = settings.user_name.trim();
        let greeting = if name.is_empty() {
            "Hi, ready to take your first request.".to_string()
        } else {
            format!("Hi {name}, ready to take your first request.")
        };
        let _ = voice_out.speak(&greeting).await;
    }

    // ── Emit `ready` (contract) ────────────────────────────────────────────────
    // run_loop emits `exit` itself; only an unexpected loop error needs one here.
    if json_events {
        use pond_core::shared::domain::agent::WorkflowEvent;
        write_ndjson_line(&WorkflowEvent::Ready {
            session_id: session_id.clone(),
        });

        // Setup diagnostics, now that there is a session to attach them to.
        for message in deferred_diagnostics.borrow().iter() {
            write_ndjson_line(&WorkflowEvent::Error {
                message: message.clone(),
            });
        }

        if let Err(e) = chat_service.run_loop().await {
            write_ndjson_line(&WorkflowEvent::Error {
                message: e.to_string(),
            });
            write_ndjson_line(&WorkflowEvent::Exit {
                reason: "error".to_string(),
            });
            mic_handle.shutdown();
            return Err(e);
        }
    } else {
        chat_service.run_loop().await?;
    }

    mic_handle.shutdown();
    Ok(())
}

// ── Adversarial Answer Reviewer ───────────────────────────────────────────────

/// Critiques answers with the main LLM and sends below-threshold ones back for revision.
struct GiapAnswerReviewer {
    /// Read per review, so it always uses the currently loaded model.
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

            // This bypasses the SSE ThoughtFilter, so strip reasoning tags here.
            current_answer = strip_thinking_tags(&revision_response.content);
            last_verdict = Some(verdict);
        }

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

/// Strips reasoning from model output: Gemma 4 channels, Qwen3/DeepSeek `<think>`, `<thought>`.
fn strip_thinking_tags(raw: &str) -> String {
    let mut text = raw.to_string();

    // Gemma 4: everything after last <channel|>
    if let Some(pos) = text.rfind("<channel|>") {
        text = text[pos + "<channel|>".len()..].trim().to_string();
    }

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

/// Parses the verdict JSON; unparseable output passes, since review must never block the user.
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

/// Newest human `sessions.updated_at`, the only activity signal from the out-of-process voice
/// loop; pond-minted `sched-*` sessions are excluded. `None` means no observable activity.
async fn newest_session_activity(
    storage: &dyn pond_core::user_data::ports::session_storage::SessionStorage,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let sessions = storage.list_sessions().await.ok()?;
    pond_core::shared::domain::session_activity::human_activity(&sessions).newest_activity
}

// ── Clock and session-activity publishers on the event bus ──────────────────
//
// The rules engine skips these events (no `trigger_view`); the event-log bridge records them.

/// Publishes one [`BusEvent::Time`] per local hour, re-read from the wall clock (no backlog
/// after sleep). Holds the bus weakly so this timer can't keep it alive past shutdown.
async fn run_time_ticker(bus: std::sync::Weak<dyn pond_core::shared::ports::event_bus::EventBus>) {
    use chrono::Timelike;
    use pond_core::shared::domain::time_tick::{secs_to_next_hour_from, TimeBoundary, TimeTick};
    use pond_core::shared::ports::event_bus::BusEvent;

    loop {
        // Pass the reading whole: separate minute/second arguments can be swapped unnoticed.
        let wait = secs_to_next_hour_from(&chrono::Local::now());
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;

        // Upgrade per iteration and drop it again before the next sleep.
        let Some(bus) = bus.upgrade() else {
            tracing::debug!("time ticker: event bus dropped — stopping");
            break;
        };
        let local = chrono::Local::now();
        bus.publish(BusEvent::Time(TimeTick {
            boundary: TimeBoundary::Hour,
            at: chrono::Utc::now(),
            local_hour: local.hour() as u8,
        }));
    }
}

/// Publishes [`BusEvent::Session`] and [`BusEvent::Presence`] transitions off one shared read
/// so they can't disagree. Keep decisions in pond-core: nothing tests this loop.
async fn run_session_activity_observer(
    bus: std::sync::Weak<dyn pond_core::shared::ports::event_bus::EventBus>,
    storage: Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage>,
    profiles: Arc<dyn pond_core::user_data::ports::profile::ProfileRepository + Send + Sync>,
    last_user_activity: Arc<tokio::sync::RwLock<std::time::Instant>>,
) {
    use pond_core::shared::domain::session_activity::{
        attribution_candidates, household_has_multiple_members, ActivityObserver, PollClock,
        PresenceEvidence, PresenceInputs, PresenceObserver,
    };
    use pond_core::shared::ports::event_bus::BusEvent;
    use pond_core::user_data::services::consolidation_schedule as sched;

    const POLL_SECS: u64 = 60;
    let idle_threshold = std::time::Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS);

    // Captured before the first poll so the boot-time clock never passes for activity.
    let started_at = std::time::Instant::now();
    let started_at_utc = chrono::Utc::now();

    let mut observer = ActivityObserver::seeded_from(storage.list_sessions().await);
    let mut presence = PresenceObserver::awaiting_baseline();

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(POLL_SECS)).await;

        let Some(bus) = bus.upgrade() else {
            tracing::debug!("session activity observer: event bus dropped — stopping");
            break;
        };

        let sessions = match storage.list_sessions().await {
            Ok(sessions) => sessions,
            Err(e) => {
                tracing::debug!(error = %e, "session activity observer: session read failed");
                continue;
            }
        };
        let now = chrono::Utc::now();

        let transitions = observer.poll(
            &sessions,
            PollClock {
                started_at,
                started_at_utc,
                in_process_at: *last_user_activity.read().await,
                now,
                idle_threshold,
            },
        );
        for transition in transitions {
            tracing::debug!(
                phase = transition.phase.as_str(),
                session_id = transition.session_id.as_deref().unwrap_or("-"),
                idle_secs = transition.idle_secs,
                "session lifecycle"
            );
            bus.publish(BusEvent::Session(transition));
        }

        // ── Presence ─────────────────────────────────────────────────────
        //
        // Read-avoidance only (the observer re-checks); `list_sessions` is unbounded.
        let mut attributed = Vec::new();
        let mut unreadable: Option<String> = None;
        for session in attribution_candidates(&sessions, now) {
            match storage.get_session_identity(&session.id).await {
                Ok(identity) => attributed.push((session, identity)),
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        session_id = %session.id,
                        "presence observer: identity read failed; skipping this poll"
                    );
                    unreadable = Some(e.to_string());
                    break;
                }
            }
        }

        let household = profiles.list().await;
        if let Err(e) = &household {
            tracing::debug!(
                error = %e,
                "presence observer: could not count household members"
            );
        }

        // Pass read failures through: what they imply is pond-core's call, not this loop's.
        let evidence: Vec<PresenceEvidence<'_>> = attributed
            .iter()
            .map(|(session, identity)| PresenceEvidence::of(session, identity))
            .collect();
        let read = match unreadable {
            Some(e) => Err(e),
            None => Ok(PresenceInputs::for_poll(
                &evidence,
                household_has_multiple_members(&household),
                now,
            )),
        };
        for transition in presence.observe_read(read) {
            tracing::debug!(
                profile_id = %transition.profile_id,
                transition = transition.transition.as_str(),
                source = transition.source.as_str(),
                session_id = %transition.session_id,
                "profile presence"
            );
            bus.publish(BusEvent::Presence(transition));
        }
    }
}

/// Idle-time proactive review; every failed read fails closed (skips the tick or hits the cap).
async fn run_proactive_reviewer(
    settings_repo: Arc<dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync>,
    storage: Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage>,
    last_user_activity: Arc<tokio::sync::RwLock<std::time::Instant>>,
    proposals: Arc<dyn pond_core::user_data::ports::proposal::ProposalRepository>,
    sender: Arc<pond_infra::broadcast_notification_sender::BroadcastNotificationSender>,
    observed: Arc<
        tokio::sync::Mutex<
            std::collections::VecDeque<pond_core::user_data::domain::proposal::BusEventRef>,
        >,
    >,
) {
    use pond_core::mcp::ports::notification::Notification;
    use pond_core::user_data::services::consolidation_schedule as sched;
    use pond_core::user_data::services::proactive_review as review;

    const POLL_SECS: u64 = 60;

    // The `delegate` tool's own orchestrator: a second registry compiles but refuses every spawn.
    let Some(deps) = pond_mcp_server::installed_orchestrator_deps() else {
        tracing::info!(
            "proactive reviewer: no orchestrator was installed (mock agent?) — not starting"
        );
        return;
    };
    let orchestrator = deps.orchestrator();
    let authorities = deps.authorities();

    // Checked once: a recipe typo should stop the reviewer loudly, not log every minute.
    let role = match review::proactive_reviewer_role() {
        Ok(role) => role,
        Err(e) => {
            tracing::error!(error = %e, "proactive reviewer: shipped role does not parse");
            return;
        }
    };

    // Baselines for the never-at-startup guard, captured before the first poll.
    let started_at = std::time::Instant::now();
    let started_at_utc = chrono::Utc::now();
    let idle_threshold = std::time::Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS);
    let mut last_run: Option<std::time::Instant> = None;

    tracing::info!(
        "proactive reviewer active — off unless both `proactive_review_enabled` and \
         `ext_orchestrator_enabled` are set"
    );

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(POLL_SECS)).await;

        let settings = match settings_repo.get().await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "proactive reviewer: settings read failed");
                continue;
            }
        };
        let sessions = match storage.list_sessions().await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "proactive reviewer: session read failed");
                continue;
            }
        };
        let now = chrono::Utc::now();

        // Optional for reads (SQL filters expiry) but the ledger learns from the terminal status.
        if let Err(e) = proposals.expire_due(now).await {
            tracing::debug!(error = %e, "proactive reviewer: expiry sweep failed");
        }

        // Nobody addressable means no review; traced, as silence would look like a broken feature.
        let Some(audience) = review::audience_for_review(&sessions, now) else {
            tracing::trace!(
                sessions = sessions.len(),
                "proactive reviewer: nobody to address — no member has been identified inside \
                 the audience window, so there is no review to run"
            );
            continue;
        };

        let db_activity =
            pond_core::shared::domain::session_activity::human_activity(&sessions).newest_activity;
        let in_process_at = *last_user_activity.read().await;

        // Rolling 24 h rather than a calendar day: the cap is about interruption frequency.
        let proposals_today = proposals
            .count_made_since(audience.profile_id(), now - chrono::Duration::days(1))
            .await
            .unwrap_or(review::MAX_PROPOSALS_PER_DAY);

        let decision = review::should_review(&review::ReviewInputs::for_tick(
            sched::GateInputs {
                enabled: settings.proactive_review_enabled,
                saw_activity_since_start: sched::saw_activity_since_start(
                    started_at,
                    in_process_at,
                    started_at_utc,
                    db_activity,
                ),
                idle_for: sched::combined_idle_for(in_process_at, db_activity, now),
                idle_threshold,
                since_last_run: last_run.map(|t| t.elapsed()),
                interval_floor: sched::interval_floor_from_hours(
                    settings.memory_consolidation_interval_hours,
                ),
            },
            settings.ext_orchestrator_enabled,
            proposals_today,
            // This loop awaits its own run, so runs never overlap here.
            false,
        ));
        if let review::ReviewDecision::Skip(reason) = decision {
            tracing::trace!(
                reason = reason.as_str(),
                "proactive reviewer: skipping tick"
            );
            continue;
        }

        // Skip on failure: an empty ledger would re-propose what a member already declined.
        let decisions = match proposals
            .decisions_since(audience.profile_id(), now - review::SUPPRESSION_WINDOW)
            .await
        {
            Ok(decisions) => decisions,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "proactive reviewer: could not read prior decisions — skipping rather than \
                     re-proposing what was already declined"
                );
                continue;
            }
        };
        let ledger = review::FeedbackLedger::from_decisions(&decisions, now);

        // Drained up front: a failed run loses these rather than re-reviewing them forever.
        let events = {
            let mut ring = observed.lock().await;
            let drained: Vec<pond_core::user_data::domain::proposal::BusEventRef> =
                ring.drain(..).collect();
            review::brief_events(&audience, &drained)
        };

        let run_id = uuid::Uuid::new_v4().to_string();
        let session_id = review::review_session_id(&run_id);
        let brief = review::review_brief(now, &events, &ledger);
        let spec =
            match review::plan_review(&role, &audience, &session_id, &brief, serde_json::json!({}))
            {
                Ok(spec) => spec,
                Err(e) => {
                    tracing::warn!(error = ?e, "proactive reviewer: delegation refused");
                    continue;
                }
            };

        tracing::info!(
            member = %audience.profile_id(),
            events = events.len(),
            made_today = proposals_today,
            "idle after user activity — starting a proactive review"
        );

        // The lease revokes on drop, so an ended review can no longer delegate.
        let cancel = tokio_util::sync::CancellationToken::new();
        let lease = authorities.publish(
            &session_id,
            review::review_authority(&audience, &session_id),
            cancel.clone(),
        );

        // Cancel on user activity: the review holds the only GPU the next user turn needs.
        let watcher = {
            let watcher_activity = last_user_activity.clone();
            let watcher_storage = storage.clone();
            let watcher_cancel = cancel.clone();
            let run_started = std::time::Instant::now();
            tokio::spawn(async move {
                const TICK_MS: u64 = 500;
                const DB_EVERY_N_TICKS: u32 = 4;
                let run_started_utc = chrono::Utc::now();
                let mut tick: u32 = 0;
                let mut db_seen: Option<chrono::DateTime<chrono::Utc>> = None;
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
                    if watcher_cancel.is_cancelled() {
                        break;
                    }
                    tick = tick.wrapping_add(1);
                    if tick % DB_EVERY_N_TICKS == 0 {
                        // A failed read yields `None`, which must never count as activity.
                        db_seen = newest_session_activity(watcher_storage.as_ref()).await;
                    }
                    if review::cancelled_by_activity(
                        run_started,
                        *watcher_activity.read().await,
                        run_started_utc,
                        db_seen,
                    ) {
                        tracing::info!("user activity resumed — cancelling the proactive review");
                        watcher_cancel.cancel();
                        break;
                    }
                }
            })
        };

        let run = orchestrator.spawn(spec).await;
        watcher.abort();
        drop(lease);
        // Failed attempts count too, or expensive retries would churn every idle window.
        last_run = Some(std::time::Instant::now());

        let run = match run {
            Ok(run) => run,
            Err(e) => {
                tracing::warn!(error = %e, "proactive reviewer: the run did not start");
                continue;
            }
        };

        // A cancelled or turn-exhausted run yields nothing here, never partial opinions.
        let yielded = review::interpret_answer(
            &run,
            &audience,
            chrono::Utc::now(),
            &ledger,
            proposals_today,
        );
        for refusal in &yielded.refusals {
            tracing::debug!(refusal = ?refusal, "proactive reviewer: impulse refused");
        }

        // WARN: all-refused is always a defect; a run with nothing to say returns an empty array.
        if yielded.proposals.is_empty() && !yielded.refusals.is_empty() {
            tracing::warn!(
                refused = yielded.refusals.len(),
                first = ?yielded.refusals.first(),
                "proactive review produced nothing: every impulse was refused. The review ran and \
                 cost a model turn, so this is a defect rather than a quiet day."
            );
        }

        for proposal in &yielded.proposals {
            if let Err(e) = proposals.save(proposal).await {
                tracing::warn!(error = %e, "proactive reviewer: could not persist a proposal");
                continue;
            }
            // Only the member's paired devices, never a broadcast. `action_required`, not `alert`:
            // the speech gate voices only `alert`, and suggestions shouldn't be read aloud.
            let report = sender
                .send_to_profile(
                    audience.profile_id(),
                    Notification {
                        id: format!("proposal-{}", proposal.id()),
                        target: String::new(),
                        category: "action_required".to_string(),
                        title: "A suggestion".to_string(),
                        body: proposal.summary(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        data: Some(serde_json::json!({
                            "proposal_id": proposal.id(),
                            "rationale": proposal.rationale(),
                        })),
                    },
                )
                .await;
            tracing::info!(
                proposal = %proposal.id(),
                member = %audience.profile_id(),
                queued = report.queued.len(),
                failed = report.failed.len(),
                "proposal made"
            );
        }
    }
}

/// Runs one consolidation pass; both modes apply through `apply_actions` so its guards can't drift.
async fn run_consolidation_pipeline(
    repo: Arc<dyn pond_core::user_data::ports::memory_repository::MemoryRepository + Send + Sync>,
    provider: Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>,
    broadcast_tx: tokio::sync::broadcast::Sender<
        pond_core::user_data::ports::memory_consolidator::ConsolidationEvent,
    >,
    cancel: tokio_util::sync::CancellationToken,
    mode: &str,
    batch_size: usize,
) {
    use pond_core::user_data::ports::memory_consolidator::ConsolidationEvent;
    use pond_core::user_data::services::consolidation_schedule::MIN_MEMORIES_TO_CONSOLIDATE;
    use pond_core::user_data::services::memory_consolidation as consolidation;

    let memories = match repo.search_scoreable(&ProfileScope::Household).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("consolidation: failed to load memories: {e}");
            let _ = broadcast_tx.send(ConsolidationEvent::Error {
                message: e.to_string(),
            });
            return;
        }
    };

    // Oldest-first: duplicates cluster in time, so a contiguous window catches both halves.
    let selection = consolidation::select_batch(memories, batch_size);

    if selection.below_minimum() {
        tracing::debug!(
            "consolidation: skipped — only {} eligible memories (need >= {})",
            selection.batch.len(),
            MIN_MEMORIES_TO_CONSOLIDATE
        );
        let _ = broadcast_tx.send(ConsolidationEvent::Error {
            message: format!(
                "Need at least {} memories to consolidate, found {}",
                MIN_MEMORIES_TO_CONSOLIDATE,
                selection.batch.len()
            ),
        });
        return;
    }

    if selection.deferred > 0 {
        tracing::info!(
            mode = %mode,
            considered = selection.considered,
            batch = selection.batch.len(),
            deferred = selection.deferred,
            "consolidation: batch cap applied — remaining memories deferred to the next run"
        );
    }

    let (mpsc_tx, mut mpsc_rx) = tokio::sync::mpsc::channel::<ConsolidationEvent>(64);
    let bridge_tx = broadcast_tx.clone();
    tokio::spawn(async move {
        while let Some(event) = mpsc_rx.recv().await {
            let _ = bridge_tx.send(event);
        }
    });

    let batch_len = selection.batch.len();
    let resolved_mode = consolidation::mode_from_setting(mode);
    let mode_label = resolved_mode.as_str();

    let result = if resolved_mode.is_adversarial() {
        three_stage_consolidator::ThreeStageConsolidator::new(provider)
            .run(&selection.batch, cancel, Some(mpsc_tx))
            .await
    } else {
        llm_memory_consolidator::run_single_pass(provider, &selection.batch, cancel, Some(mpsc_tx))
            .await
    };

    match result {
        Ok(ref result) => {
            // Collected, not lazy: a borrowing iterator across the await breaks HRTB inference.
            let accepted: Vec<
                pond_core::user_data::ports::memory_consolidator::ConsolidationAction,
            > = result
                .exchanges
                .iter()
                .filter(|e| e.judgment.accepted)
                .map(|e| e.proposal.action.clone())
                .collect();

            let outcome =
                match consolidation::apply_actions(repo.as_ref(), &selection.batch, accepted).await
                {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::warn!("consolidation: applying actions failed: {e}");
                        let _ = broadcast_tx.send(ConsolidationEvent::Error {
                            message: e.to_string(),
                        });
                        return;
                    }
                };

            if result.accepted_count > 0 || result.rejected_count > 0 {
                tracing::info!(
                    mode = %mode_label,
                    accepted = result.accepted_count,
                    rejected = result.rejected_count,
                    merged = outcome.merged,
                    pruned = outcome.pruned,
                    split = outcome.split,
                    recategorized = outcome.recategorized,
                    blocked = outcome.blocked,
                    duration_ms = result.duration_ms,
                    "memory consolidation complete"
                );
                let details_json = serde_json::to_string(&result).ok();
                let _ = repo
                    .log_consolidation_run(
                        mode_label,
                        batch_len,
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
// Needs `api-24` (fastembed's `ort` defaults). 1.24.x has no osx-x86_64 build, so Intel Macs
// need a system ORT or `ORT_DYLIB_PATH`; 1.24.0 lacks linux-aarch64 (Jetson). Re-check to bump.
const ORT_VERSION: &str = "1.24.2";

/// Approximate size of the platform library in MB (for the progress message).
const ORT_APPROX_SIZE_MB: u64 = 30;

/// Points `ORT_DYLIB_PATH` at an ONNX Runtime, downloading one if needed; failure is non-fatal.
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

    // The versioned file is the real binary; the bare name covers a manual install.
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

    // stderr, not stdout: under `chat --json-events` stdout is the NDJSON stream.
    eprintln!("  Setup    downloading ONNX Runtime v{ORT_VERSION} (~{ORT_APPROX_SIZE_MB} MB), one time...");

    match download_and_extract_ort(&url, &lib_dir, &archive_stem) {
        Ok(lib_path) => {
            eprintln!("  Setup    ONNX Runtime ready");
            tracing::info!("ONNX Runtime installed to {}", lib_path.display());
            // SAFETY: single-threaded setup, before any worker spawns.
            unsafe { std::env::set_var("ORT_DYLIB_PATH", &lib_path) };
        }
        Err(e) => {
            eprintln!("  Setup    FAILED to download ONNX Runtime: {e}");
            eprintln!(
                "           Voice output needs it. Install manually: brew install onnxruntime"
            );
        }
    }
}

/// Returns `(lib_name, versioned_name)` for the current platform.
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

/// Returns `(os_tag, arch_tag)` as ONNX Runtime's GitHub release archives name them.
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

    std::fs::create_dir_all(lib_dir)?;

    let tmp_dir = std::env::temp_dir().join("giap-ort-download");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir)?;

    let tgz = tmp_dir.join("ort.tgz");

    // ── Download ─────────────────────────────────────────────────────────
    // The curl subprocess is egress too. `check_egress`, not `EgressCall`: this fn is sync and
    // `finish` reaches `tokio::spawn`, which panics with no runtime on the thread.
    pond_core::shared::services::egress::check_egress(url)
        .map_err(|denied| anyhow::anyhow!("{denied}"))?;

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

    let _ = std::fs::remove_dir_all(&tmp_dir);

    Ok(dest)
}

/// Defaults unset face env vars for the shipped models; run `ensure_onnx_runtime()` first.
fn apply_face_recognition_defaults() {
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

    // Shipped 3-class Silent-Face export: live is slot 2 and it expects [0, 1] pixels.
    if std::env::var_os("POND_FACE_ANTISPOOF_LIVE_INDEX").is_none() {
        unsafe { std::env::set_var("POND_FACE_ANTISPOOF_LIVE_INDEX", "2") };
    }
    if std::env::var_os("POND_FACE_ANTISPOOF_PIXEL_SCALE").is_none() {
        unsafe { std::env::set_var("POND_FACE_ANTISPOOF_PIXEL_SCALE", "unit") };
    }

    // Secondary PAD, duplicated in `build_face_recognition`: `setup` and `serve` each need it.
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
/// Records wake-word samples and stores Whisper's transcriptions as calibration variants.
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

    // Own mic owner: calibration never runs alongside the wake-word detector.
    let (mic_handle, _mic_owner_join) = pond_audio::spawn(
        Box::new(pond_audio::CpalCapture::new()),
        pond_audio::CAPTURE_RATE_HZ,
        15_000,
        true,
    );
    let whisper = WhisperRsInput::new(whisper_model_path.clone(), mic_handle.clone())
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
    println!("  Run `pond-server chat --voice` to test it.");
    println!();

    mic_handle.shutdown();
    Ok(())
}

fn default_data_dir() -> std::path::PathBuf {
    // `POND_DATA_DIR` redirects all storage, e.g. for tests.
    if let Ok(dir) = std::env::var("POND_DATA_DIR") {
        return std::path::PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("goose-in-a-pond")
}

/// Returns `None` (face endpoints then 503) with no feature, no model, or no loadable ORT.
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

    // AdaFace first for its low-light tolerance; `arcface.onnx` is the legacy name.
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

    if std::env::var_os("POND_FACE_ANTISPOOF_PATH").is_none() {
        let antispoof_default = data_dir.join("models/face/antispoof.onnx");
        if antispoof_default.exists() {
            // SAFETY: single-threaded init phase before any task scheduling.
            unsafe {
                std::env::set_var("POND_FACE_ANTISPOOF_PATH", antispoof_default.as_os_str());
            }
        }
    }
    // The shipped MiniFASNetV2 export is [fake_2D, fake_3D, live]; the `auto` default assumes 0.
    if std::env::var_os("POND_FACE_ANTISPOOF_LIVE_INDEX").is_none() {
        unsafe {
            std::env::set_var("POND_FACE_ANTISPOOF_LIVE_INDEX", "2");
        }
    }

    // Safe to auto-enable: the ensemble takes `max(spoof_score)`, so a dud secondary is harmless.
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

    // ort panics, not errs, on first `Session::builder()` without its dylib; degrade instead.
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

    // SCRFD (34G finds smaller faces, 10G is lighter) gives landmarks for alignment; UltraFace is
    // bbox-only; with neither, center-square crops risk "everyone matches".
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

    // ArcFace on aligned 112×112 crops: strangers can reach 0.55–0.65, so aligned floors sit in
    // the empirically safe 0.68–0.75 band.
    let threshold = match (&detector, model_kind) {
        (Some(d), EmbeddingModel::ArcFace512) if d.produces_landmarks() => 0.70,
        (Some(_), EmbeddingModel::ArcFace512) => 0.72,
        (None, EmbeddingModel::ArcFace512) => 0.85,
        (Some(d), EmbeddingModel::MobileFaceNet128) if d.produces_landmarks() => 0.70,
        (Some(_), EmbeddingModel::MobileFaceNet128) => 0.72,
        (None, EmbeddingModel::MobileFaceNet128) => 0.85,
    };
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
                run_chat(None, None, false, None, true, Some("none"), None, false).await?;
            }
            "2" => {
                let data_dir = default_data_dir();
                let drain = tracing_setup::init_tracing(false, &data_dir);
                run_server(
                    std::path::PathBuf::from("pond-desktop/dist"),
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

// ── Private mesh ──────────────────────────────────────────────────────────────

/// Starts the mesh if enabled; its key persists in the raw settings KV (a secret, not a field).
#[cfg(feature = "mesh")]
async fn build_mesh_transport(
    settings: &pond_core::user_data::domain::settings::Settings,
    settings_repo: &Arc<
        dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync,
    >,
    peer_directory: Arc<dyn pond_core::mesh::ports::peer_directory::PeerDirectory>,
) -> Option<Arc<dyn pond_core::mesh::ports::mesh_transport::MeshTransport>> {
    use pond_mesh_protocol::identity::MeshKeypair;

    if !settings.mesh_enabled {
        tracing::info!("mesh disabled — enable via PUT /api/v1/settings (mesh_enabled)");
        return None;
    }

    let secret_hex = match settings_repo.get_key("mesh_identity_secret").await {
        Ok(Some(hex)) => hex,
        _ => {
            let keypair = MeshKeypair::generate();
            let hex = hex_encode_32(&keypair.secret_bytes());
            if let Err(err) = settings_repo
                .set_key("mesh_identity_secret", hex.clone())
                .await
            {
                tracing::error!("failed to persist mesh identity secret: {err}");
            }
            hex
        }
    };
    let secret_bytes = match hex_decode_32(&secret_hex) {
        Some(bytes) => bytes,
        None => {
            tracing::error!(
                "stored mesh_identity_secret is malformed — mesh transport not started"
            );
            return None;
        }
    };
    let keypair = MeshKeypair::from_bytes(secret_bytes);
    tracing::info!("mesh enabled: peer_id={}", keypair.peer_id());

    // Identity-derived, so the port survives restarts and saved peer invites keep working.
    let listen_port = 40000 + (u16::from_be_bytes([secret_bytes[0], secret_bytes[1]]) % 10000);
    tracing::info!("mesh listening on a stable, identity-derived port: {listen_port}");

    // Placeholder hashes until harness/model attestation lands; any two dev Ponds can pair.
    let config = Libp2pMeshTransportConfig {
        listen_addr: format!("/ip4/0.0.0.0/tcp/{listen_port}")
            .parse()
            .expect("valid multiaddr literal"),
        harness_hash: pond_mesh_protocol::hashing::hash_harness(b"pond-mesh-v1-dev"),
        model_hash: pond_mesh_protocol::hashing::hash_model(b"pond-mesh-v1-dev"),
        keypair,
        peer_directory,
    };
    match Libp2pMeshTransport::new(config).await {
        Ok(transport) => {
            Some(Arc::new(transport)
                as Arc<
                    dyn pond_core::mesh::ports::mesh_transport::MeshTransport,
                >)
        }
        Err(err) => {
            tracing::error!("failed to start mesh transport: {err}");
            None
        }
    }
}

#[cfg(not(feature = "mesh"))]
async fn build_mesh_transport(
    settings: &pond_core::user_data::domain::settings::Settings,
    _settings_repo: &Arc<
        dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync,
    >,
    _peer_directory: Arc<dyn pond_core::mesh::ports::peer_directory::PeerDirectory>,
) -> Option<Arc<dyn pond_core::mesh::ports::mesh_transport::MeshTransport>> {
    if settings.mesh_enabled {
        tracing::warn!(
            "settings.mesh_enabled is true but this pond-server binary was built without \
             the `mesh` feature — mesh transport not started"
        );
    }
    None
}

#[cfg(feature = "mesh")]
fn hex_encode_32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(feature = "mesh")]
fn hex_decode_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Connects to Breez/Spark for Lightning settlement; the wallet mnemonic persists in the raw KV.
#[cfg(feature = "lightning")]
async fn build_payment_rail(
    settings: &pond_core::user_data::domain::settings::Settings,
    settings_repo: &Arc<
        dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync,
    >,
    data_dir: &std::path::Path,
) -> Option<Arc<dyn pond_core::mesh::ports::payment_rail::PaymentRail>> {
    use pond_adapters_lightning::{LightningConfig, LightningPaymentRail};

    if !settings.lightning_enabled {
        tracing::info!("lightning disabled — enable via PUT /api/v1/settings (lightning_enabled)");
        return None;
    }

    let storage_dir = data_dir.join("lightning").to_string_lossy().to_string();
    let mut config = match LightningConfig::from_env(storage_dir) {
        Ok(config) => config,
        Err(err) => {
            tracing::warn!(
                "settings.lightning_enabled is true but {err} — Lightning settlement not started"
            );
            return None;
        }
    };
    if let Ok(Some(saved)) = settings_repo.get_key("lightning_wallet_mnemonic").await {
        config.mnemonic = Some(saved);
    }

    match LightningPaymentRail::connect(config).await {
        Ok((rail, generated_mnemonic)) => {
            if let Some(mnemonic) = generated_mnemonic {
                if let Err(err) = settings_repo
                    .set_key("lightning_wallet_mnemonic", mnemonic)
                    .await
                {
                    tracing::error!("failed to persist lightning wallet mnemonic: {err}");
                }
            }
            tracing::info!("lightning enabled — connected to Breez/Spark");
            Some(Arc::new(rail) as Arc<dyn pond_core::mesh::ports::payment_rail::PaymentRail>)
        }
        Err(err) => {
            tracing::error!("failed to connect to Breez/Spark: {err}");
            None
        }
    }
}

#[cfg(not(feature = "lightning"))]
async fn build_payment_rail(
    settings: &pond_core::user_data::domain::settings::Settings,
    _settings_repo: &Arc<
        dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync,
    >,
    _data_dir: &std::path::Path,
) -> Option<Arc<dyn pond_core::mesh::ports::payment_rail::PaymentRail>> {
    if settings.lightning_enabled {
        tracing::warn!(
            "settings.lightning_enabled is true but this pond-server binary was built without \
             the `lightning` feature — Lightning settlement not started"
        );
    }
    None
}

/// A fixed provider handle for the mesh responder that follows `AppState.llm_provider` hot-swaps.
#[cfg(feature = "mesh")]
struct SharedLlmProvider(Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>);

#[cfg(feature = "mesh")]
#[async_trait::async_trait]
impl LlmProvider for SharedLlmProvider {
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<pond_core::models::domain::message::ChatMessage>,
    ) -> anyhow::Result<pond_core::models::domain::message::ChatMessage> {
        match self.0.read().await.clone() {
            Some(provider) => provider.complete(system_prompt, messages).await,
            None => Err(anyhow::anyhow!("no local provider is configured yet")),
        }
    }

    fn model_name(&self) -> String {
        "mesh-backing-provider".to_string()
    }
}

use pond_core::models::ports::provider::UnavailableProvider;

/// Must run once: a second `MeshInferenceService` would split the transport's one `recv()` queue.
#[cfg(feature = "mesh")]
fn build_mesh_provider(
    mesh_transport: &Option<Arc<dyn pond_core::mesh::ports::mesh_transport::MeshTransport>>,
    peer_directory: Arc<dyn pond_core::mesh::ports::peer_directory::PeerDirectory + Send + Sync>,
    credit_ledger: Arc<dyn pond_core::mesh::ports::credit_ledger::CreditLedger + Send + Sync>,
    usage_tally: Arc<dyn pond_core::mesh::ports::usage_tally::UsageTally + Send + Sync>,
    settings_repo: Arc<dyn pond_core::user_data::ports::settings::SettingsRepository>,
    llm_provider: Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>,
    payment_rail: Option<Arc<dyn pond_core::mesh::ports::payment_rail::PaymentRail>>,
) -> (
    Option<Arc<dyn LlmProvider>>,
    Option<Arc<dyn pond_core::mesh::ports::peer_capability_query::PeerCapabilityQuery>>,
    Option<Arc<dyn pond_core::mesh::ports::invoice_requester::InvoiceRequester>>,
) {
    let Some(transport) = mesh_transport.clone() else {
        return (None, None, None);
    };
    let backing_provider: Arc<dyn LlmProvider> = Arc::new(SharedLlmProvider(llm_provider));
    let service = pond_adapters_mesh_inference::MeshInferenceService::spawn(
        transport,
        peer_directory,
        credit_ledger,
        usage_tally,
        settings_repo,
        backing_provider,
        // Max inter-chunk gap, not total time: slow lenders (~1.7 tok/s) legitimately pause long.
        std::time::Duration::from_secs(180),
        std::time::Duration::from_secs(15 * 60),
        payment_rail,
    );
    (
        Some(Arc::new(service.provider()) as Arc<dyn LlmProvider>),
        Some(service.clone()
            as Arc<dyn pond_core::mesh::ports::peer_capability_query::PeerCapabilityQuery>),
        Some(service as Arc<dyn pond_core::mesh::ports::invoice_requester::InvoiceRequester>),
    )
}

#[cfg(not(feature = "mesh"))]
fn build_mesh_provider(
    mesh_transport: &Option<Arc<dyn pond_core::mesh::ports::mesh_transport::MeshTransport>>,
    _peer_directory: Arc<dyn pond_core::mesh::ports::peer_directory::PeerDirectory + Send + Sync>,
    _credit_ledger: Arc<dyn pond_core::mesh::ports::credit_ledger::CreditLedger + Send + Sync>,
    _usage_tally: Arc<dyn pond_core::mesh::ports::usage_tally::UsageTally + Send + Sync>,
    _settings_repo: Arc<dyn pond_core::user_data::ports::settings::SettingsRepository>,
    _llm_provider: Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>,
    _payment_rail: Option<Arc<dyn pond_core::mesh::ports::payment_rail::PaymentRail>>,
) -> (
    Option<Arc<dyn LlmProvider>>,
    Option<Arc<dyn pond_core::mesh::ports::peer_capability_query::PeerCapabilityQuery>>,
    Option<Arc<dyn pond_core::mesh::ports::invoice_requester::InvoiceRequester>>,
) {
    debug_assert!(
        mesh_transport.is_none(),
        "mesh_transport should only ever be Some when built with --features mesh"
    );
    (None, None, None)
}

/// Pays down trusted peers' usage tallies each interval; idles until mesh and lightning are on.
fn spawn_settlement_job(
    peer_directory: Arc<dyn pond_core::mesh::ports::peer_directory::PeerDirectory + Send + Sync>,
    usage_tally: Arc<dyn pond_core::mesh::ports::usage_tally::UsageTally + Send + Sync>,
    payment_rail: Option<Arc<dyn pond_core::mesh::ports::payment_rail::PaymentRail>>,
    invoice_requester: Option<Arc<dyn pond_core::mesh::ports::invoice_requester::InvoiceRequester>>,
) {
    const SETTLEMENT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15 * 60);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SETTLEMENT_INTERVAL);
        loop {
            interval.tick().await;

            let (Some(payment_rail), Some(invoice_requester)) =
                (payment_rail.clone(), invoice_requester.clone())
            else {
                continue; // mesh/lightning not enabled on this Pond — nothing to settle
            };
            let rate = pond_core::mesh::domain::settlement::MESH_SETTLEMENT_MILLISATS_PER_TOKEN;

            let service = pond_core::mesh::services::settlement::SettlementService::new(
                peer_directory.clone(),
                usage_tally.clone(),
                payment_rail,
                invoice_requester,
            );
            match service.run_once(rate).await {
                Ok(outcomes) => {
                    for outcome in outcomes {
                        match outcome {
                            pond_core::mesh::services::settlement::SettlementOutcome::Settled {
                                peer,
                                amount,
                                ..
                            } => {
                                tracing::info!("settlement: paid {peer} {amount}");
                            }
                            pond_core::mesh::services::settlement::SettlementOutcome::Failed {
                                peer,
                                error,
                            } => {
                                tracing::warn!("settlement: failed for {peer}: {error}");
                            }
                            pond_core::mesh::services::settlement::SettlementOutcome::NothingPending {
                                ..
                            } => {}
                            pond_core::mesh::services::settlement::SettlementOutcome::BelowSettlementMinimum {
                                peer,
                                amount,
                            } => {
                                tracing::info!(
                                    "settlement: {peer} owes {amount}, below the 1-sat minimum — carried to next pass"
                                );
                            }
                        }
                    }
                }
                Err(err) => tracing::warn!("settlement: pass failed to start: {err}"),
            }
        }
    });
    tracing::info!("settlement worker started — runs every 15 minutes");
}

// ── Goose agent backend ───────────────────────────────────────────────────────

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
    device_control: Arc<
        dyn pond_core::user_data::ports::device_control::DeviceControlPort + Send + Sync,
    >,
    session_storage: Option<Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage>>,
    // Supply it: without the catalog an Ollama model's context window is guessed from its name.
    model_repo: Option<Arc<dyn ModelRepository>>,
    voice_mode: bool,
    // Locked so mesh enabled at runtime applies next turn; CLI callers pass a permanent `None`.
    mesh_provider: Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>,
) -> (
    Arc<dyn Agent>,
    Option<Arc<dyn pond_core::mcp::ports::extension_manager::ExtensionManagerPort>>,
    Option<Arc<dyn pond_core::mcp::ports::tools::tool_caller::ToolCaller>>,
    Arc<dyn pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort>,
) {
    use pond_adapters_goose::GooseAdapter;
    #[cfg(feature = "local-inference")]
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

        let engine = match pond_inference::LlamaCppEngine::new(data_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("LlamaCppEngine init failed: {e} — falling back to mock");
                return (Arc::new(MockAgent::new()), None, None, default_registry);
            }
        };

        let model_id = &settings.chat_model;
        if !model_id.is_empty() {
            if let Err(e) = engine.load_model(model_id, 99, true).await {
                tracing::error!("Failed to load model '{}': {e}", model_id);
                return (Arc::new(MockAgent::new()), None, None, default_registry);
            }
        }

        let dispatcher = pond_mcp_server::McpToolDispatcher::new(
            memory_repo.clone(),
            weather,
            scheduler,
            settings_repo.clone(),
            device_registry.clone(),
            skill_repo.clone(),
            embedding_provider,
            device_control.clone(),
        );

        let dispatcher: Arc<dyn pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher> =
            Arc::new(dispatcher);

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
            ss,
            Some(dispatcher),
        );

        tracing::info!("PondAgent ready — independent inference with KV-cache reuse");
        return (Arc::new(agent), None, None, default_registry);
    }

    if agent_backend != "goose" {
        return (Arc::new(MockAgent::new()), None, None, default_registry);
    }

    // Optional FunctionGemma tool-calling specialist; without it the main LLM calls tools via MCP.
    #[cfg(feature = "local-inference")]
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
    #[cfg(not(feature = "local-inference"))]
    let tool_caller: Option<Arc<dyn ToolCaller>> = None;

    let settings = settings_repo.get().await.unwrap_or_default();
    match pond_adapters_goose::register_giap_extensions(
        &settings,
        memory_repo.clone(),
        embedding_provider.clone(),
        scheduler,
        weather,
        settings_repo.clone(),
        device_registry.clone(),
        skill_repo.clone(),
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

    match GooseAdapter::new(
        settings_repo,
        template_repo,
        extras_repo,
        skill_repo,
        memory_repo,
        llamafile_url.to_string(),
        Some(data_dir.to_path_buf()),
        Some(default_registry.clone()),
    )
    .await
    {
        Ok(adapter) => {
            // Powers the turn trimmer's rolling-summary splice.
            let adapter = match session_storage {
                Some(storage) => adapter.with_giap_session_storage(storage),
                None => adapter,
            };
            // Without it, per-turn memory retrieval falls back to keyword LIKE search.
            let adapter = match embedding_provider {
                Some(provider) => adapter.with_embedding_provider(provider),
                None => adapter,
            };
            // Else an Ollama model's context window is guessed from its name.
            let adapter = match model_repo {
                Some(repo) => adapter.with_model_repo(repo),
                None => adapter,
            };
            // Read per turn, so mesh_rebuild filling it later needs no adapter rebuild.
            let adapter = adapter.with_mesh_provider(mesh_provider);
            if voice_mode {
                adapter.set_voice_mode(true);
            }
            let ext_mgr: Arc<dyn ExtensionManagerPort> = adapter.extension_manager();
            tracing::info!("Goose agent active — GIAP MCP extension registered");
            let adapter = Arc::new(adapter);
            // giap-toolkit calls back into the adapter, which didn't exist at registration time.
            pond_mcp_server::init_toolkit_deps(Some(adapter.clone()
                as Arc<
                    dyn pond_core::mcp::ports::tools::tool_selection_control::ToolSelectionControl,
                >));
            // Must be `turn_authorities()`: a fresh registry compiles, then refuses every lookup.
            // Unconditional: the toggle gates registration, so with it off these go unused.
            {
                let runner: Arc<dyn pond_adapters_goose::orchestrator::ChildRunner> =
                    Arc::new(pond_adapters_goose::GooseChildRunner::new(adapter.clone()));
                let orchestrator: Arc<dyn pond_core::shared::ports::orchestrator::Orchestrator> =
                    Arc::new(pond_adapters_goose::GooseOrchestrator::new(
                        runner,
                        adapter.turn_authorities(),
                    ));
                pond_mcp_server::init_orchestrator_deps(pond_mcp_server::OrchestratorDeps::new(
                    orchestrator,
                    adapter.turn_authorities(),
                    recipe_repo,
                ));
            }
            let agent: Arc<dyn Agent> = adapter;
            (agent, Some(ext_mgr), tool_caller, default_registry)
        }
        Err(e) => {
            tracing::error!("GooseAdapter init failed: {e} — falling back to mock agent");
            (Arc::new(MockAgent::new()), None, None, default_registry)
        }
    }
}

// ── Model catalog helpers ─────────────────────────────────────────────────────

/// Assigns a default TTS voice on first run; never overwrites an existing assignment.
async fn ensure_tts_is_set_up(
    repo: &dyn ModelRepository,
    settings_repo: &dyn pond_core::user_data::ports::settings::SettingsRepository,
) {
    let assignments = repo.list_assignments().await.unwrap_or_default();
    if assignments.iter().any(|a| a.role == "tts") {
        return;
    }

    // Prefer a voice already in settings (upgrades from before roles), else Kokoro's default.
    let stored = settings_repo.get().await.ok();
    let configured = stored
        .as_ref()
        .map(|s| s.voice_tts_voice.clone())
        .unwrap_or_default();
    let voice =
        if pond_adapters_kokoro::voices::voice_path(std::path::Path::new("/"), configured.trim())
            .is_ok()
        {
            configured.trim().to_string()
        } else {
            pond_adapters_kokoro::DEFAULT_VOICE.to_string()
        };

    let model_id = ModelRecord::id_for(&ModelCategory::TtsKokoro, &voice);
    if let Err(e) = repo.set_assignment("tts", &model_id).await {
        tracing::warn!("could not assign a default voice: {e}");
        return;
    }
    let _ = settings_repo
        .set_key("voice_tts_voice", voice.clone())
        .await;
    let _ = settings_repo
        .set_key("active_tts_model", voice.clone())
        .await;
    println!("  ✅ Voice: {voice} assigned (first run)");
}

/// Adopts the speech tier this machine can keep up with. Kept out of `ensure_tts_is_set_up`,
/// which returns early once a voice is assigned; idempotent via `tier_to_adopt`.
async fn ensure_host_tts_tier(
    settings_repo: &dyn pond_core::user_data::ports::settings::SettingsRepository,
) {
    let stored = settings_repo.get().await.ok();
    let stored_tier = stored
        .as_ref()
        .map(|s| s.voice_tts_quality.as_str())
        .unwrap_or("");
    let untouched = pond_core::user_data::domain::settings::Settings::default().voice_tts_quality;

    let Some(host_tier) = pond_adapters_kokoro::tier_to_adopt(stored_tier, &untouched) else {
        return;
    };
    if let Err(e) = settings_repo
        .set_key("voice_tts_quality", host_tier.to_string())
        .await
    {
        tracing::warn!("could not set the speech tier for this machine: {e}");
        return;
    }
    println!("  ✅ Voice quality: {host_tier} (chosen for this machine)");
}

/// Seeds the model catalog from the static list and local Ollama; fetch failure is non-fatal.
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

/// Mirrors role assignments, the source of truth, into settings; unassigned roles are untouched.
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
                // Skip `tts_piper`: Kokoro rejects Piper filenames; syncing flip-flops each boot.
                // Not migrated here either; the Kokoro bootstrap owns choosing the voice.
                if category == "tts_piper" {
                    tracing::info!(
                        model = %model_name,
                        "ignoring a Piper TTS assignment left over from before the \
                         engine swap; Kokoro will choose the voice"
                    );
                    continue;
                }
                // Idempotent: the value round-trips, so a bare format! would stack prefixes.
                let stored_active_model = if category == "tts_piper" {
                    if model_name.starts_with("piper-") {
                        model_name.to_string()
                    } else {
                        format!("piper-{model_name}")
                    }
                } else {
                    model_name.to_string()
                };
                let _ = settings_repo
                    .set_key("active_tts_model", stored_active_model)
                    .await;
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

    // This entry point downloads, so it must install the mode or "offline" fails open.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(
            &SqliteSettingsRepository::new(db.system.clone())
                .get()
                .await
                .unwrap_or_default()
                .network_mode,
        ),
    );

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

/// `auto` maps to `chat`: the LLM routes tools itself via MCP.
fn resolve_role(role_arg: &str, _message: &str) -> String {
    match role_arg {
        "auto" | "chat" => "chat".to_string(),
        other => other.to_string(),
    }
}

/// Streams text to stdout; tool calls and status go to stderr so piped output stays clean.
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
                eprintln!(
                    "\r\x1b[K\x1b[33m  📝 Revised (score: {score}/5, rounds: {rounds})\x1b[0m"
                );
                println!("{content}");
                printed_newline = content.ends_with('\n');
            }
            AgentStreamEvent::TurnLimitReached { max_turns } => {
                // The cap sentence already printed as Text; just say how to continue.
                eprintln!(
                    "\r\x1b[K\x1b[2m  (turn budget of {max_turns} reached — \
                     send \"continue\" to resume)\x1b[0m"
                );
            }
            // On stderr; `detail` is a tool name or GIAP reason, never the child's own text.
            AgentStreamEvent::SubagentProgress {
                role,
                status,
                detail,
                ..
            } => {
                eprint!("\r\x1b[K\x1b[2m  [{role}] {}", status.as_str());
                if let Some(detail) = detail {
                    eprint!(" {detail}");
                }
                eprintln!("\x1b[0m");
            }
            AgentStreamEvent::Error { content } => {
                eprintln!("\n  error: {content}");
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// One-shot or REPL Goose agent chat on the same backend as `run_server`.
async fn run_agent_cmd(action: AgentAction) -> Result<()> {
    let data_dir = default_data_dir();
    let db = Database::init(&data_dir).await?;

    let settings_repo: Arc<
        dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync,
    > = Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    // As in `run_chat`: redacted, and indexed with model id `None` since no embedder exists here.
    let memory_repo: Arc<
        dyn pond_core::user_data::ports::memory_repository::MemoryRepository + Send + Sync,
    > = Arc::new(
        pond_core::user_data::services::redacting_memory_repository::RedactingMemoryRepository::new(
            Arc::new(
                SqliteMemoryRepository::new(db.system.clone()).with_vector_index(
                    Arc::new(pond_infra::sqlite_vector_index::SqliteVectorIndex::new(
                        db.vectors.clone(),
                    )),
                    None,
                ),
            ),
            Arc::new(pond_infra::rule_redactor::RuleRedactor::new()),
        ),
    );
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
    // CLI turns must budget history with the same real context window as dashboard turns.
    let cli_model_repo: Arc<dyn ModelRepository + Send + Sync> =
        Arc::new(SqliteModelRepository::new(db.system.clone()));

    let settings = settings_repo.get().await.unwrap_or_default();
    // The mode is a process-global defaulting to `Open`; each entry point must install it.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(&settings.network_mode),
    );
    // Used only when chat_provider = "llamafile"; other providers route themselves.
    let llamafile_url = format!("http://127.0.0.1:{}", ports::llamafile_port());

    let weather: Option<Arc<dyn WeatherProvider>> = match (
        settings.weather_enabled,
        pond_core::user_data::services::location::resolve(&settings).weather_target(),
    ) {
        (true, Some((lat, lon, loc))) => {
            Some(Arc::new(OpenMeteoWeatherAdapter::new(lat, lon, loc)))
        }
        _ => None,
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
                Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
                None, // session_storage — not needed for goose backend
                Some(cli_model_repo.clone()),
                false,
                Arc::new(tokio::sync::RwLock::new(None)), // mesh_provider: none on CLI paths
            )
            .await;

            let request = AgentRequest {
                message,
                session_id: session,
                model_role,
                images: Vec::new(),
                voice_mode: false,
                canvas_mode: false,
                // The CLI user already has shell access, so a narrower scope would be theatre.
                profile_scope: ProfileScope::Household,
                profile_context: None,
                tool_group_allowlist: None,
                warmup: false,
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
                Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
                None, // session_storage — not needed for goose backend
                Some(cli_model_repo.clone()),
                false,
                Arc::new(tokio::sync::RwLock::new(None)), // mesh_provider: none on CLI paths
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
                    // Same as the single-shot arm above.
                    profile_scope: ProfileScope::Household,
                    profile_context: None,
                    tool_group_allowlist: None,
                    warmup: false,
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
                Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
                None, // session_storage — not needed for goose backend
                Some(cli_model_repo.clone()),
                false,
                Arc::new(tokio::sync::RwLock::new(None)), // mesh_provider: none on CLI paths
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
            use pond_core::prompts::builtin_template_content;
            use pond_core::user_data::domain::prompt_template::PromptTemplate;

            // Same lookup as the REST reset handler, so both paths restore identical factory text.
            let Some((content, description)) = builtin_template_content(&name) else {
                eprintln!("'{name}' is not a built-in template. Only balanced | concise | technical | warm can be reset.");
                std::process::exit(1);
            };
            let t = PromptTemplate {
                name: name.clone(),
                content: content.to_string(),
                description: description.to_string(),
                is_system: true,
                // A reset returns the row to factory ownership, including the current generation.
                is_customized: false,
                factory_version: pond_core::user_data::domain::prompt_template::FACTORY_VERSION,
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

        SkillAction::Add {
            name,
            description,
            icon,
            content,
        } => {
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
                description,
                icon,
                content,
                active: true,
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            if let Err(e) = skill.validate() {
                eprintln!("Invalid skill: {e}");
                std::process::exit(1);
            }
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
    // Redacted, since `memories add` content comes from argv (think pasted credentials). Indexed
    // so `remove` drops the vector too; model id `None` as there's no embedder here.
    let repo =
        pond_core::user_data::services::redacting_memory_repository::RedactingMemoryRepository::new(
            Arc::new(
                SqliteMemoryRepository::new(db.system.clone()).with_vector_index(
                    Arc::new(pond_infra::sqlite_vector_index::SqliteVectorIndex::new(
                        db.vectors.clone(),
                    )),
                    None,
                ),
            ),
            Arc::new(pond_infra::rule_redactor::RuleRedactor::new()),
        );

    match action {
        MemoryAction::List { limit } => {
            let fragments = repo.search_recent(&ProfileScope::Household, limit).await?;
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

/// Shows the pairing code from the running server over loopback. Never mint locally: codes
/// live in the server's process-local `ISSUED_CODE_CACHE`, so a CLI-minted one can't verify.
async fn run_pairing(refresh: bool) -> Result<()> {
    let data_dir = default_data_dir();

    // The server writes its bound port here; if missing, the HTTP call reports it isn't running.
    let port = std::fs::read_to_string(data_dir.join(".runtime_api_port"))
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(ports::API_SERVER);

    let base = format!("http://127.0.0.1:{port}/api/v1/handshake/pairing-code");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    // GET returns the current unconsumed code; POST mints a fresh one.
    let resp = if refresh {
        client.post(&base).send().await
    } else {
        client.get(&base).send().await
    }
    .map_err(|e| {
        anyhow::anyhow!(
            "failed to reach the running pond server on 127.0.0.1:{port}: {e}. \
             Is the server running? Start it with `pond serve` before `pond pairing`."
        )
    })?;

    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "server returned {} for pairing-code request",
            resp.status()
        ));
    }

    #[derive(serde::Deserialize)]
    struct CodeResp {
        code: Option<String>,
        expires_at: Option<String>,
    }
    let body: CodeResp = resp.json().await?;

    // A GET can return {"code": null} when no live code exists — mint one via POST.
    let (code, expires_at) = match (body.code, body.expires_at) {
        (Some(c), Some(e)) => (c, e),
        _ => {
            let minted: CodeResp = client.post(&base).send().await?.json().await?;
            (
                minted
                    .code
                    .ok_or_else(|| anyhow::anyhow!("server did not return a pairing code"))?,
                minted.expires_at.unwrap_or_default(),
            )
        }
    };

    // Derive the hostname the same way run_server does.
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "pond".to_string());
    let hostname = hostname
        .strip_suffix(".local")
        .unwrap_or(&hostname)
        .to_string();

    let pair_url = format!("pond://pair?host={hostname}.local&port={port}&code={code}");

    println!("\n  ┌────────────────────────────────────────────────────┐");
    println!(
        "  │  Pairing code:  {}   (expires: {})  │",
        code,
        &expires_at[..16.min(expires_at.len())]
    );
    println!("  │  Scan with Goose On The Go or enter the code.     │");
    println!("  └────────────────────────────────────────────────────┘");
    print_pairing_qr(&pair_url);
    println!("\n  URL: {pair_url}\n");
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

        // These are KV-only, not `Settings` fields, hence get_key().
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

    // CI only `cargo check`s this crate, so tests here never run there; put guards in pond-core.
}
