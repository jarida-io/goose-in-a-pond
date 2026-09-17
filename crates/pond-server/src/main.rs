//! Goose In A Pond — Server Entry Point
//!
//! Usage:
//!   pond-server setup [--model tiny|base|small]
//!   pond-server serve [--port PORT] [--open]
//!   pond-server chat  [--voice] [--provider mock|llamafile|ollama] [--model MODEL]
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

mod asset_root;
mod composite_model_catalog_provider;
mod conversation_extractor;
mod filesystem_model_storage;
mod http_model_downloader;
mod inference_lane_runner;
mod kokoro_control;
mod llamafile_process;
mod llm_memory_consolidator;
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
    // Before the runtime exists, so this is genuinely single-threaded, and
    // before any child is spawned — which is the only moment it can help. A
    // GUI-launched process inherits launchd's bare PATH, so nvm's node is
    // invisible to it and both the Matter controller and the stdio extensions
    // fail with "not found in PATH". Logged rather than reported: nothing is
    // wrong yet, and the subsystems that need Node say so themselves if it
    // turns out not to be there at all.
    node_path::ensure_node_on_path();
    // Name the TLS provider before anything can ask rustls to guess.
    //
    // This workspace enables BOTH of rustls' crypto backends without meaning
    // to: `aws_lc_rs` from the root Cargo.toml and `ring` from hyper-rustls via
    // reqwest. rustls refuses to pick between them, and every entry point that
    // infers a provider panics rather than returning an error — on whatever
    // background worker happened to touch TLS first, which is a crash with no
    // relationship to the code that caused it.
    //
    // Installing one here makes the answer deterministic for the whole process,
    // including dependencies that will hit the inferring path later. `Err` means
    // somebody already installed one, which is equally fine and not worth
    // failing a boot over.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

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

/// Keep Goose's own on-disk state inside the pond's data directory.
///
/// Goose resolves every directory it owns through `Paths::get_dir`, which honours
/// an absolute `GOOSE_PATH_ROOT` and otherwise falls back to the platform's app
/// dir for "Block/goose". Nothing in GIAP was setting it, so the engine's
/// `sessions.db` lived outside `POND_DATA_DIR` entirely, with three consequences:
///
///  1. `scripts/live-test.sh` promises a scratch run "can never touch a real
///     pond". That was true of both pond databases and false of engine state.
///  2. Two pond-server instances on one machine shared a single session store
///     and a single `YYYYMMDD_N` id namespace.
///  3. On a machine that also runs Goose CLI or Desktop, `GooseAdapter::new`
///     adopts `list_sessions().first()` — ordered by `sort_timestamp DESC`, i.e.
///     the user's most recent unrelated conversation — repoints its working_dir
///     at pond-server's cwd and loads 15 `giap-*` extensions onto it.
///
/// Must run before anything touches `SESSION_STORAGE`, which is a `LazyLock` over
/// `Paths::data_dir()` and therefore latches the first answer it gets. Being the
/// first statement of `async_main` is what guarantees that.
///
/// Upgrading an existing pond does not lose history: `pond_system.db` is
/// authoritative, `resolve_goose_session` re-validates a stored pairing against
/// the engine store and finds nothing, and `hydrate_goose_session` replays the
/// conversation from pond history into a fresh engine session.
fn pin_goose_state_under(data_dir: &std::path::Path) {
    // `validated_path_root` silently ignores a relative path, which would put us
    // back on the platform default without saying so.
    let root = match std::fs::canonicalize(data_dir) {
        Ok(abs) => abs,
        Err(_) => {
            // The directory may not exist on a first run; absolute is all Goose
            // requires, so fall back to making the configured path absolute.
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
            // In --json-events mode, route diagnostics to stderr so stdout is
            // reserved for NDJSON contract lines only.
            let console = if json_events {
                tracing_setup::ConsoleSink::Stderr
            } else {
                tracing_setup::ConsoleSink::Stdout
            };
            // Interactive voice/text chat: keep the console clean — WARN+ only
            // for tracing; the curated turn lines + inference summary print via
            // diag!/out!, and full detail still lands in the rolling log file.
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
            // Default: run interactive chat (backward compat) — provider comes from Settings
            let _log = tracing_setup::init_tracing(false, &data_dir);
            run_chat(None, None, false, None, true, Some("none"), None, false).await
        }
    }
}

async fn run_setup(model: &str) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose In A Pond — Setup         ║");
    println!("  ╚═══════════════════════════════════════╝");

    // Before anything loads a model, so the line is above the noise rather than
    // buried under provider init.
    report_acceleration();

    let data_dir = default_data_dir();

    println!("\n  📂 Data directory: {}", data_dir.display());

    // Step 1: Check + auto-install system dependencies (Linux/macOS only)
    println!("\n  [1/8] Checking system dependencies...");
    if system_deps::ensure_system_deps().await {
        println!("  ✅ System dependencies OK");
    } else {
        println!(
            "  ⚠  Some system deps could not be installed — see docs/developer/linux-setup.md"
        );
        println!("     Continuing setup; some features may not work until deps are installed.");
    }

    // Step 2: Initialize databases + seed model catalog
    println!("\n  [2/8] Initializing databases...");
    let db_setup = Database::init(&data_dir).await?;
    println!("  ✅ Databases ready");

    // PAI-2 P6a: `pond setup` is almost entirely downloads — whisper, piper,
    // the chat model, the ONNX runtime — and it ran with the egress gate at its
    // `Open` default because `set_network_mode` was only ever called by
    // `run_server`. Installed here, before the first fetch in step 3, so a
    // household that stored `offline` gets refusals it can act on instead of
    // a setup command that quietly ignores the setting.
    //
    // This runs after the DB init because the setting lives in it; on a first
    // run there are no rows and `Settings::default()` gives `open`, which is
    // the same answer as before and the right one — nobody has asked for
    // anything narrower yet.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(
            &SqliteSettingsRepository::new(db_setup.system.clone())
                .get()
                .await
                .unwrap_or_default()
                .network_mode,
        ),
    );

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
    let settings_repo_setup = SqliteSettingsRepository::new(db_setup.system.clone());
    println!("  📋 Fetching model catalog from upstream sources...");
    seed_model_catalog(&setup_model_repo, &data_dir).await;
    ensure_tts_is_set_up(&setup_model_repo, &settings_repo_setup).await;

    // Seed built-in prompt templates (INSERT OR IGNORE — never overwrites user edits)
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

    // Step 4: Whisper runs in-process via whisper-rs — no binary to fetch.
    {
        println!("\n  [4/8] Whisper runs in-process — no binary download needed.");
    }

    // Step 5: Piper runs in-process via piper-rs — only the voice model is fetched.
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

    // Step 6: ONNX Runtime — detect or auto-download
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

    // Step 7: espeak-ng-data — required by piper-rs for phonemization
    println!("\n  [7/8] Checking espeak-ng-data...");
    {
        let espeak_path = data_dir.join("bin").join("espeak-ng-data");
        if espeak_path.exists() {
            println!("  ✅ espeak-ng-data found at {}", espeak_path.display());
        } else {
            // Check if espeak-ng is installed via Homebrew and create the symlink
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

    // Step 8: Microphone access check
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

    // Step 9 (face-onnx feature only): face recognition models
    #[cfg(feature = "face-onnx")]
    {
        println!("\n  [9/9] Setting up face recognition models...");
        if let Err(e) = model_download::download_face_models(&data_dir).await {
            println!("  ⚠  Face model setup failed: {} — face recognition will be disabled until you add the files manually", e);
        }
    }

    // Private mesh (mesh feature only): pre-generate the identity keypair.
    // No download involved — this is compiled-in libp2p, not a model — so it
    // is not numbered alongside the download steps above. `build_mesh_transport`
    // would otherwise generate this lazily the first time mesh_enabled flips
    // on, which is fine on its own but means the very first enable (whether
    // at startup or hot-reloaded via PUT /api/v1/settings — see
    // AppState::mesh_rebuild) pays a one-time keypair-generation cost this
    // step moves here instead, onto a run the operator expects to take a
    // while anyway.
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

/// How long after boot the personal-context index first considers a pass.
///
/// Not zero: the design's rule is that backfill is deferred to idle rather than
/// run at startup, because a household's first turn after an upgrade must not be
/// slow because the pond chose that moment to index itself. Sixty seconds is
/// enough for a boot to settle and short enough that a pond left alone is
/// repaired within the minute.
const INDEX_MAINTENANCE_DELAY_SECS: u64 = 60;

/// How often a pass is CONSIDERED after that. The lane decides whether one runs.
///
/// This used to be a one-shot: the sweep fired once, sixty seconds after boot,
/// and never again for the life of the process. A pond left running for a week
/// indexed nothing it learned during that week, and the only way to repair the
/// index was to restart the server — which is not something a member of a
/// household does, or should have to.
const INDEX_MAINTENANCE_POLL_SECS: u64 = 15 * 60;

/// How often the batch memory-extraction engine CONSIDERS a pass.
///
/// A minute, which is also the floor on how often a pass may actually run. The
/// gate in front of it is much longer -- fifteen minutes of household quiet --
/// so this is not "every minute", it is "within a minute of the household
/// having been quiet long enough".
const MEMORY_EXTRACTION_POLL_SECS: u64 = 60;

/// How long after boot the engine first considers a pass.
///
/// Same reasoning as the index sweep's delay, and the same number: a
/// household's first turn after an upgrade must not be slow because the pond
/// chose that moment to start reading its own history.
const MEMORY_EXTRACTION_DELAY_SECS: u64 = 60;

/// Say so, loudly, when this binary cannot reach the accelerator this host has.
///
/// Called at startup for its side effect only. The check is cheap and the case
/// it catches is otherwise invisible: a CPU build on a Jetson compiles, starts,
/// loads the model and answers correctly at roughly a thirtieth of the speed,
/// with no error anywhere. See `pond_core::models::domain::acceleration`.
fn report_acceleration() {
    use pond_core::models::domain::acceleration::{classify, host_is_accelerated, warning};

    // `cuda` is a feature of `pond-adapters-local-inference`, so a `cfg!` here
    // would always be false. Without the feature there is no accelerated build
    // to have, which is itself the answer.
    #[cfg(feature = "local-inference")]
    let cuda_build = pond_adapters_local_inference::CUDA_ENABLED;
    #[cfg(not(feature = "local-inference"))]
    let cuda_build = false;

    // A device profile answers for the host when one is active, so a Mac can
    // reach this table's other cells. Inert unless POND_DEVICE_PROFILE is set,
    // which nothing in production or in deploy.sh sets.
    let profile = pond_core::models::domain::device_profile::active();

    let probed = std::fs::read_to_string("/proc/device-tree/model").ok();
    // The device tree pads with NULs; a trailing NUL would defeat a `contains`
    // on some readers and costs nothing to strip.
    let probed = probed.as_deref().map(|m| m.trim_end_matches('\0').trim());

    let (model, tegra_release) = match profile {
        Some(p) => (p.device_tree_model.as_deref(), p.has_tegra_release),
        None => (
            probed,
            std::path::Path::new("/etc/nv_tegra_release").exists(),
        ),
    };
    let accelerated = host_is_accelerated(model, tegra_release);
    // An emulated CUDA build is a claim about the binary, not the host, so it is
    // OR-ed rather than substituted: a real CUDA build must never be talked out
    // of reporting itself by a profile.
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

    // NOTE: `ensure_onnx_runtime()` used to be called here. It moved below the
    // `set_network_mode` install, because it can DOWNLOAD, and a download
    // cannot be gated by a setting that has not been read yet. See PAI-2 P6a.
    //
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

    // PAI-2 P5: the egress gate reads a process-global, so it must be installed
    // before any adapter exists to make an outbound call. `PUT /settings`
    // re-installs it, so a change takes effect without a restart.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(&settings.network_mode),
    );

    // Ensure the ONNX Runtime shared library is available — check system
    // paths first, then auto-download from GitHub Releases if needed.
    // Must run before any ONNX-dependent init (face recognition, embeddings);
    // those all happen further down, so nothing between here and the old site
    // (`apply_face_recognition_defaults` sets env vars, `Database::init`, the
    // HF-cache migration, the system-dep warning) touches ONNX.
    //
    // It must run AFTER `set_network_mode`, not before: it downloads ~100 MB
    // from github.com, and at the old site the process-global was still at its
    // `Open` default, so a stored `network_mode = "offline"` did not apply to
    // the single largest outbound transfer `serve` makes. PAI-2 P6a.
    ensure_onnx_runtime();

    // Override agent_backend from DB settings (UI can change it without CLI restart).
    // CLI flag takes precedence only when explicitly set to something other than "goose".
    let agent_backend = if agent_backend == "goose" && !settings.agent_backend.is_empty() {
        &settings.agent_backend
    } else {
        agent_backend
    };

    // Q2-05: pond-agent is quarantined — experimental backend not ready for production.
    // Force goose even if the setting was written as "pond", and PERSIST the
    // correction: a stored "pond" row bricks the desktop Settings page —
    // the UI saves the full settings object, so every PUT echoes the stored
    // value back and trips the 422 quarantine guard regardless of what the
    // user actually edited. Healing the row at startup keeps the guard's job
    // to its intent (rejecting a genuine switch TO "pond").
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

    // STT — whisper ggml model download (the in-process backend reads the
    // same `.bin` files the legacy subprocess used).
    // Apply the microphone privacy setting before anything can open a device.
    pond_core::models::domain::mic_gate::set_mic_enabled(settings.mic_enabled);

    // Single shared microphone owner (see `pond_audio`). This process only
    // ever transcribes already-recorded audio via the HTTP `/transcribe`
    // route below — it never opens the device — but `WhisperRsInput::new`
    // still requires a handle so there is exactly one code path for every
    // caller, live capture or not.
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

    // Transcription is in-process; this URL only reaches an external
    // whisper.cpp if the user has pointed a setting at one deliberately.
    const DEFAULT_WHISPER_URL: &str = "http://127.0.0.1:9000";
    let whisper_url = if settings.voice_whisper_url.is_empty() {
        DEFAULT_WHISPER_URL.to_string()
    } else {
        settings.voice_whisper_url.clone()
    };

    // In-process whisper for the HTTP transcribe route — avoids the external
    // whisper.cpp subprocess. Built here so AppState can hold the closure without
    // depending on pond-adapters-whisper.
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

    // espeak-ng-data, unconditionally.
    //
    // This used to sit inside a `if piper_is_primary` block. Kokoro phonemizes
    // through the same espeak-ng, so gating the data on Piper being the engine
    // would leave the new engine unable to turn text into phonemes at all —
    // the removal of Piper would have taken the phonemizer with it.
    model_download::ensure_espeak_ng_data(&data_dir).await;

    // Where espeak-rs looks for its phoneme tables. The env var keeps its
    // historical `PIPER_` name because that literal is what the espeak-rs crate
    // reads — it names the reader, not the engine that used to own it.
    let espeak_data = {
        let p = model_download::piper_espeak_data_path(&data_dir);
        if p.exists() {
            Some(p)
        } else {
            None
        }
    };

    // Kept only for the status report's `piper_http_port` field, which is now
    // always absent. The legacy subprocess and its HTTP wrapper are gone.
    let piper_http_port: Option<u16> = None;

    // Shared with the TTS control below, so a voice or tier fetch appears in
    // the same progress feed as every other download on the Models page.
    let download_tracker: std::sync::Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, pond_api::DownloadEntry>>,
    > = std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));

    // ── Kokoro: the TTS engine ──
    //
    // Constructed BEFORE anything is spoken but WITHOUT loading the weights —
    // `KokoroOutput::new` reads the vocab and opens the audio device, and the
    // ~92 MB session is loaded on the first utterance and can be dropped again.
    // A pond that never speaks never pays for the model.
    //
    // There is no second engine any more. A failure here is text-only output,
    // said out loud in the log rather than left as silence.
    // Fetch the engine before constructing it. espeak data is already ensured
    // above for Piper, and Kokoro uses the same phonemizer.
    //
    // Resolved before the fetch, not after: a tier that cannot produce audio on
    // this host should not be downloaded either. Onboarding writes the tier
    // straight to settings, so a stored value that is silent here is reachable
    // and has to be handled every start, not only when someone opens the picker.
    // Adopt the host's tier BEFORE resolving one, or the pond downloads and
    // runs the wrong engine for a whole session and only picks the right one up
    // on the next start. This used to live in `ensure_tts_is_set_up`, which
    // `serve` never calls — it is the `setup` subcommand's — so on a board that
    // had been through setup once, nothing ever revisited the tier.
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
                // Still bounded so ONNX Runtime's pool does not take the whole
                // machine from the language model — but derived rather than
                // pinned, because the old pin of 2 could not hit real time on
                // the Jetson at any tier. See `default_intra_threads`.
                intra_threads: Some(pond_adapters_kokoro::default_intra_threads()),
                espeak_data: espeak_data.clone(),
            };
            match pond_adapters_kokoro::KokoroOutput::new(cfg) {
                Ok(out) => {
                    // Voice and pace are hot — neither touches the session.
                    let voice = settings.voice_tts_voice.trim();
                    if !voice.is_empty() && out.set_voice(voice).await.is_err() {
                        // Heal the setting rather than diverging from it.
                        //
                        // An install from before the engine swap holds a Piper
                        // filename here. The adapter falls back to its default
                        // and speaks fine, but the stored value never changes —
                        // so the Voice screen keeps showing a voice that is not
                        // the one talking, and every restart repeats this
                        // warning. Writing back what is actually in use makes
                        // the picker honest and makes this a one-time event.
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
                    // No second engine to fall back to — say so plainly, and
                    // let the `tts: None` path below make it text-only.
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

    // The other half of the engine: reconfiguring it while it runs. Held apart
    // from `tts` because the chat loop is only ever asked to speak, and has no
    // business knowing that voices have files behind them.
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
    // PAI-2 P3: the deterministic redactor. Rule-based, no model in the loop.
    // Built here so it is in scope for both chokepoints below.
    let redactor: Arc<dyn pond_core::security::ports::redactor::Redactor> =
        Arc::new(pond_infra::rule_redactor::RuleRedactor::new());
    // Built BEFORE the memory and context repositories on purpose: both take
    // the vector index and the id of the model whose vectors they mirror, and
    // that id comes from this provider. It reads only `settings` and
    // `data_dir`, both resolved far above, so moving it up is safe.
    // ── Embedding provider (gguf / fastembed / none) ─────────────────────────
    // Initialized before the agent backend so it can be wired into the memory
    // MCP server for semantic search on recall/save.
    //
    // `"gguf"` is the on-device path: fastembed's ONNX Runtime does not
    // initialise on the Jetson Orin (version-incompatible, times out), so a pond
    // that shipped `"fastembed"` there fell back to keyword matching and PAI-3's
    // semantic memory never ran. The GGUF provider reuses the llama.cpp this pond
    // already runs and loads its model LAZILY (first embed, after Goose has
    // claimed the backend) — see `pond_inference::embedding` for why.
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
                // `active_embedding_model` is shared with the fastembed path, so a
                // stale fastembed name (or a typo) must NOT silently disable
                // embeddings — fall back to the gguf default rather than to None.
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
                    // Deliberately NOT awaited. This is a ~146 MB fetch, and awaiting it
                    // here means the server does not bind its port until it finishes --
                    // measured on a Mac: /health refused the connection for the whole
                    // download. On a Jetson behind a slow link that is minutes of a pond
                    // that looks dead, and with no timeout a hung mirror never starts at
                    // all. The provider below loads LAZILY, so it tolerates the file
                    // arriving later; the first embed before it lands reports a clear
                    // error and retrieval falls back to keyword until then.
                    //
                    // Gated by network_mode, but NOT at the chokepoint the obvious
                    // reading suggests: this is a huggingface.co URL, so
                    // `download_file` dispatches to `download_via_hf_cache` BEFORE
                    // reaching its own `egress::begin`. The gate that actually covers
                    // this fetch lives in `pond_hf_cache` (`egress::begin` on both the
                    // HEAD and the GET). Naming the wrong chokepoint here would make a
                    // regression in the real one invisible.
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

    // The shared personal-context index (phase A) and the identity of the model
    // whose vectors go into it. `None` when embeddings are off, in which case
    // the adapters below store exactly as they always did.
    let vector_index: Arc<dyn pond_core::context::vector_index::VectorIndex> = Arc::new(
        pond_infra::sqlite_vector_index::SqliteVectorIndex::new(db.vectors.clone()),
    );
    // A member's first turn must not queue behind the pond indexing itself.
    let index_maintenance_cancel = tokio_util::sync::CancellationToken::new();
    // Somebody asked for a reindex. Held here rather than inside the sweep so
    // the route can reach it: clearing the index without a way to refill it on
    // demand leaves a member staring at an empty panel until the next scheduled
    // pass, which is the shape of "the button did nothing".
    // Filled in when the sweep actually spawns, which is also exactly when
    // there is anything to wake. It used to be a `Notify` created here
    // unconditionally and handed to `AppState` behind `index_sweep_running`,
    // which said the same thing twice; now the handle's existence IS the fact.
    //
    // ONE doorbell, two ringers: the Reindex button clears the index and rings
    // it, and the lane's own "run now" rings it directly. A second `Notify`
    // would have given the sweep two ways to be woken and the lane's button
    // would have reached neither.
    let mut index_reindex_handle: Option<Arc<tokio::sync::Notify>> = None;
    let vector_model_id = embedding_provider.as_ref().map(|p| p.model_id());

    // Chokepoint 1: every memory write, whatever wrote it. Wrapping the one
    // construction covers extraction, the giap-memory MCP tool, POST /memories
    // and consolidation -- and a writer nobody has added yet.
    let memory_repo: Arc<
        dyn pond_core::user_data::ports::memory_repository::MemoryRepository + Send + Sync,
    > = Arc::new(
        pond_core::user_data::services::redacting_memory_repository::RedactingMemoryRepository::new(
            // INSIDE the redactor deliberately: this decorator drops the vector
            // when it finds a secret, and the index must mirror what the store
            // actually keeps, not what the caller handed in.
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

    // Private mesh (#132) — PeerDirectory/CreditLedger/UsageTally are plain
    // SQLite, no extra dependency, so unlike mesh_transport (below, behind
    // the `mesh` feature + settings.mesh_enabled) they're always available.
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

    // The speculative-decoding drafter, provisioned the way the TTS engine is:
    // a helper model nobody asked for and nobody should have to think about.
    // Measured on the Orin, it takes a real turn from 31 to 49 tok/s.
    //
    // Before ANY provider is built, because both consumers read the registry
    // and neither re-reads it: `apply_jetson_settings` sizes the context window
    // against the models that will be resident and sets `draft_model` from the
    // registry, and it runs when the local adapter is constructed a few lines
    // below. Registering after that point costs a restart to converge.
    //
    // Failure is silent by design -- decode is simply not accelerated. The
    // notification further down is the last resort, and it is down there
    // because the queue to put it on does not exist yet.
    let drafter_wanted = model_download::drafter_for(&settings.chat_model).is_some();
    let drafter_ready = if drafter_wanted {
        let present = model_download::ensure_mtp_drafter(&data_dir, &settings.chat_model)
            .await
            .is_some();
        // The registry row is what the engine resolves a drafter by name
        // through, so a downloaded file with no row is invisible.
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
    // Each role (Chat / Think / Task) may use a different provider + model.
    // Token budget and temperature are baked in at startup.
    //
    // Async helper so we can await LocalInferenceLlmAdapter::new() for the
    // "local" (in-process GGUF) provider without blocking the Tokio runtime.
    // TODO(cloud-fallback): when `settings.cloud_fallback_enabled` is ON, wrap
    // the selected local provider so a failed local inference spills over to a
    // cloud model (failure-only, never on success). OFF by default (privacy-first)
    // — the toggle is persisted but no spill path is wired yet.
    async fn build_provider(
        provider: &str,
        model: &str,
        llamafile_url: &str,
        data_dir: Option<&std::path::Path>,
        max_tokens: u32,
        temperature: f32,
    ) -> Arc<dyn LlmProvider> {
        match provider {
            // The mesh stack (mesh_transport/mesh_provider) doesn't exist
            // yet at this point in startup — it's built later in
            // `run_server`, and `AppState.mesh_provider`'s lock is what
            // `PUT /settings` hot-reloads into once it does (see
            // `AppState::mesh_rebuild`'s own docs). A Pond that restarts
            // with `chat_provider` already persisted as "mesh" therefore
            // seeds its INITIAL provider with `UnavailableProvider`, not the
            // real mesh provider — same as `build_one`'s "mesh" arm falls
            // back to when mesh isn't available yet, and for the same
            // stated reason: silently using llamafile instead would leave a
            // user who picked mesh with no way to tell their choice didn't
            // take effect. It self-corrects the next time anything saves
            // `chat_provider`/`chat_model` through `PUT /settings`, which
            // re-derives this from the (by-then-live) mesh provider.
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

            // mistral.rs serves the same OpenAI-compatible surface LlamafileProvider
            // already speaks, so it needs a URL rather than a new adapter.
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

    // AppState still carries an `inference_pool` slot (pond-api's InferencePool
    // port), but the only implementation ever built for it, TokioInferencePool,
    // had zero callers of `submit` — nothing on the serving path ever queued a
    // task through it, so it did no work beyond printing a concurrency figure
    // at startup. Removed with that print; leaving the field `None` costs
    // nothing until a real consumer needs the port.
    let inference_pool: Option<Arc<dyn pond_core::models::ports::inference_pool::InferencePool>> =
        None;

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

    let db = Arc::new(db);

    // Spawn background TTL pruning task (runs every 6 hours). Reads the user's
    // retention settings each cycle (per-category + sensitivity-aware, #117).
    {
        let logs = db.logs.clone();
        let system = db.system.clone();
        let settings_repo = settings_repo.clone();
        tokio::spawn(async move {
            pond_infra::pruning::run_pruning(logs, system, settings_repo).await;
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

    // ── Memory consolidation (inactivity-based) ──────────────────────────
    //
    // Three shared pieces of state:
    //   - last_user_activity: reset by every route via AppState::note_user_activity
    //   - consolidation_cancel: abort mid-run when the user comes back
    //   - consolidation_event_tx: broadcast channel for SSE + background logs
    let last_user_activity = Arc::new(tokio::sync::RwLock::new(std::time::Instant::now()));
    let consolidation_cancel: Arc<
        tokio::sync::RwLock<Option<tokio_util::sync::CancellationToken>>,
    > = Arc::new(tokio::sync::RwLock::new(None));
    let (consolidation_event_tx, _) = tokio::sync::broadcast::channel::<
        pond_core::user_data::ports::memory_consolidator::ConsolidationEvent,
    >(64);

    // The single inference slot every background job takes turns on. Each job
    // keeps its own poll cadence and body; what it no longer keeps is a private
    // answer to "may I run now?", which could only ever account for the jobs its
    // author happened to know about. See `inference_lane_runner`.
    //
    // Seeded from `lane_job_runs` so a restart does not hand every job a clean
    // slate. `since_last_run: None` means "never ran", which both outranks
    // every real wait and skips the interval floor -- correct for a job that
    // has genuinely never run, and badly wrong for one that ran four minutes
    // ago in the process before this one. See `0059_lane_job_runs.sql`.
    let lane_runs: Arc<dyn pond_core::user_data::ports::lane_run_log::LaneRunLog> = Arc::new(
        pond_infra::sqlite_lane_run_log::SqliteLaneRunLog::new(db.system.clone()),
    );
    let lane_history = match lane_runs.load().await {
        Ok(rows) => {
            if !rows.is_empty() {
                tracing::info!(jobs = rows.len(), "lane clock restored from disk");
            }
            rows.into_iter().collect()
        }
        // Degrade to the empty clock every release before this one booted with,
        // rather than refusing to start over a log.
        Err(e) => {
            tracing::warn!(error = %e, "lane clock could not be read; starting with none");
            std::collections::HashMap::new()
        }
    };
    let (lane_run_tx, mut lane_run_rx) = tokio::sync::mpsc::unbounded_channel();
    let inference_lane =
        crate::inference_lane_runner::InferenceLane::restored(lane_history, Some(lane_run_tx));

    // One writer, draining what the slot guard's `Drop` posted. Separate from
    // the loops so a slow disk cannot hold the lane, and unbounded so `Drop`
    // never blocks: the queue's depth is bounded by how often a job can finish,
    // which is at most once per lane slot.
    {
        let lane_runs = lane_runs.clone();
        tokio::spawn(async move {
            while let Some((job, at)) = lane_run_rx.recv().await {
                if let Err(e) = lane_runs.record(job, at).await {
                    // The in-memory clock already advanced, so this process
                    // still schedules correctly; only a restart loses the
                    // stamp. Worth a line, not worth a retry loop.
                    tracing::warn!(
                        job = job.as_str(),
                        error = %e,
                        "could not write a lane run to disk"
                    );
                }
            }
        });
    }

    // Build the ConsolidationRunner closure that pond-api will call from the
    // POST /api/v1/memory/consolidate endpoint. Captures repo, provider, and
    // the broadcast channel so pond-api never imports the consolidator crate.
    //
    // Built UNCONDITIONALLY: the enable toggle is a *runtime* decision, read
    // fresh from the settings DB by the caller and by the loop below. Gating
    // construction on a startup snapshot meant flipping the switch in Settings
    // did nothing until the next restart.
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
                    // Mode + batch size come from the CURRENT settings, so a
                    // manual run honours whatever the user last chose.
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
    //
    // Contract, enforced by `pond_core::user_data::services::consolidation_schedule`:
    // at most one run per `memory_consolidation_interval_hours`, and only after
    // INACTIVITY_THRESHOLD_SECS of quiet **following real user activity in this
    // process lifetime**. A freshly booted server nobody has spoken to never
    // consolidates, however long it idles.
    //
    // Activity is a two-source signal, because the terminal voice loop is a
    // separate OS process that never touches this AppState:
    //   1. in-process `last_user_activity` (HTTP routes), and
    //   2. the newest `sessions.updated_at` in pond_system.db, which the voice
    //      child bumps through ChatService on every turn it persists.
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
        // Claimed at spawn, not per tick: `claim` is what records that a loop
        // for this job exists in this process, which is what the status route
        // reports as `present` and what stops the button ringing a doorbell
        // nobody is behind.
        let inact_wake = inference_lane.claim(LaneJob::Consolidation);

        // Baselines for the "never on startup" guard. Captured before the
        // server binds, so no request can have been served yet.
        let started_at = std::time::Instant::now();
        let started_at_utc = chrono::Utc::now();

        tokio::spawn(async move {
            const POLL_SECS: u64 = 60;
            let idle_threshold = std::time::Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS);

            loop {
                let tick = crate::inference_lane_runner::wait_for_tick(
                    std::time::Duration::from_secs(POLL_SECS),
                    &inact_wake,
                )
                .await;

                let settings = match inact_settings_repo.get().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("consolidation scheduler: settings read failed: {e}");
                        continue;
                    }
                };

                // Out-of-process activity (voice child, GOTG on another
                // process, anything else writing turns).
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

                // The lane owns the gate now: it applies the same
                // activity/interval rules this block used to apply alone, but
                // decides against EVERY registered job rather than this one, and
                // hands back the slot itself so nothing else can be mid-run.
                // Never exempt on a scheduled tick: a pond nobody has talked
                // to has nothing to consolidate. A hand-asked one is exempt,
                // because the asking is the activity.
                let cadence = crate::inference_lane_runner::Cadence::new(
                    sched::interval_floor_from_hours(settings.memory_consolidation_interval_hours),
                    idle_threshold,
                    false,
                );

                let Some(slot) = inact_lane
                    .acquire(
                        LaneJob::Consolidation,
                        settings.memory_consolidation_enabled,
                        cadence,
                        tick.waives(),
                        saw_activity_since_start,
                        idle_for,
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

                // Watcher: abort the moment activity resumes from EITHER source.
                // Routes cancel this token directly; the DB poll is what lets an
                // out-of-process voice turn interrupt a run.
                let watcher_activity = inact_activity.clone();
                let watcher_storage = inact_storage.clone();
                let watcher_cancel = cancel.clone();
                let watcher_baseline_in_process = in_process_at;
                let watcher_baseline_db = db_activity;
                let watcher = tokio::spawn(async move {
                    // The in-process clock is a lock read, so poll it fast. The
                    // DB check is a query, so sample it every Nth tick instead
                    // of hammering SQLite for the whole length of a run.
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
                                // A transient read failure reads as None, which
                                // must not be mistaken for activity.
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

                // The interval floor (not the activity clock) is what prevents a
                // re-fire. Rewriting last_user_activity here would have faked
                // user activity and confused the summary loop that shares it.
                //
                // Deliberate: an attempt consumes the interval budget even when
                // it was cancelled or skipped for too few memories. The
                // alternative — retry after the next 15-minute idle window —
                // reintroduces exactly the repeated-expensive-attempt churn
                // this phase set out to remove. Consolidation is a best-effort
                // background chore, so on a contended device it is better to
                // miss a pass than to keep trying.
                //
                // Dropping the guard is what spends the budget and releases the
                // slot; every path out of this iteration does it, including the
                // early returns above.
                drop(slot);
            }
        });
        tracing::info!(
            "memory consolidation scheduler active — runs after {} min idle, at most once per interval (enable toggle is live)",
            sched::INACTIVITY_THRESHOLD_SECS / 60
        );
    }

    // ── Idle rolling-summary refresh (hybrid compaction, soft half) ──────
    // Same contract as memory consolidation: inactivity-based, interruptible,
    // never at startup. One loop serves every session in pond_system.db —
    // including voice-child sessions, which share the DB. The refreshed
    // summary reaches the model via the deterministic turn trimmer's
    // <conversation-summary> splice.
    if settings.hybrid_compaction_enabled {
        use pond_core::user_data::services::consolidation_schedule as sched;
        use pond_core::user_data::services::inference_lane::LaneJob;

        // Conversations summarised per pass. This sweep used to be unbounded --
        // every session touched since boot, one model call each, in a single
        // tick. On a pond with a busy afternoon behind it that is an arbitrary
        // number of decodes holding the machine, and the next pass is only
        // thirty seconds away. Titling took the same bound for the same reason.
        const MAX_PER_PASS: usize = 5;

        let sum_storage = session_storage.clone();
        let sum_provider = llm_provider.clone();
        let sum_activity = last_user_activity.clone();
        let sum_settings_repo = settings_repo.clone();
        let sum_lane = inference_lane.clone();
        let idle_secs = settings.summary_idle_secs.max(30) as u64;
        let sum_wake = inference_lane.claim(LaneJob::SummaryRefresh);

        // Baselines for the "never on startup" guard, captured before the
        // server binds so no request can have been served yet.
        let started_at_instant = std::time::Instant::now();
        let started_at_utc = chrono::Utc::now();

        tokio::spawn(async move {
            // "Never at startup": only sessions that saw a message AFTER this
            // process started are candidates. Separate from the lane's own
            // activity gate and kept alongside it -- this one is about which
            // sessions are candidates, not about whether the pond is quiet.
            let started_at = started_at_utc;
            loop {
                let tick = crate::inference_lane_runner::wait_for_tick(
                    std::time::Duration::from_secs(30),
                    &sum_wake,
                )
                .await;

                // Re-read every tick so the toggle takes effect without a
                // restart. This used to be read once, at boot, from the
                // settings snapshot that decided whether to spawn the loop at
                // all -- so switching hybrid compaction off left the sweep
                // running until the pond was restarted.
                let settings = match sum_settings_repo.get().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("summary refresh: settings read failed: {e}");
                        continue;
                    }
                };

                // Both activity sources, not just the in-process one. The
                // terminal voice loop runs in a SEPARATE process and can never
                // touch this server's clock, so a pond being talked to by voice
                // looked perfectly idle here -- and this was the one background
                // loop that did not consult the database for it.
                let db_activity = newest_session_activity(sum_storage.as_ref()).await;
                let in_process_at = *sum_activity.read().await;
                let now = chrono::Utc::now();
                let saw_activity_since_start = sched::saw_activity_since_start(
                    started_at_instant,
                    in_process_at,
                    started_at_utc,
                    db_activity,
                );
                let idle_for = sched::combined_idle_for(in_process_at, db_activity, now);

                // On the lane as of this change. It was decoding beside
                // whichever job already held the machine, which is the single
                // thing the lane exists to prevent -- and it is the most
                // frequent poller of the lot, so it was the likeliest to be the
                // second decoder.
                //
                // The interval floor is the poll interval: this job is cheap
                // per session and wants to run whenever the pond is quiet. The
                // bound on its cost is MAX_PER_PASS, not a long floor.
                let cadence = crate::inference_lane_runner::Cadence::new(
                    std::time::Duration::from_secs(30),
                    std::time::Duration::from_secs(idle_secs),
                    false,
                );
                let Some(slot) = sum_lane
                    .acquire(
                        LaneJob::SummaryRefresh,
                        settings.hybrid_compaction_enabled,
                        cadence,
                        tick.waives(),
                        saw_activity_since_start,
                        idle_for,
                    )
                    .await
                else {
                    continue;
                };

                let provider = match sum_provider.read().await.clone() {
                    Some(p) => p,
                    None => continue,
                };
                let sessions = match sum_storage.list_sessions().await {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut refreshed = 0usize;

                for session in sessions {
                    if session.updated_at < started_at {
                        continue;
                    }
                    if refreshed >= MAX_PER_PASS {
                        tracing::debug!("summary refresh: stopping at {MAX_PER_PASS} this pass");
                        break;
                    }
                    // Abort the refresh the moment activity resumes — the
                    // on-device engine is serial and a user turn must never
                    // wait behind a summary pass.
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
                            refreshed += 1;
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

                // An attempt spends the interval budget whether or not it
                // summarised anything, and releases the slot on every path out
                // -- including the two `continue`s above, which return before
                // this line and drop the guard on the way. That is the whole
                // reason the slot is a guard rather than a flag.
                drop(slot);
            }
        });
        tracing::info!(
            "hybrid compaction enabled — rolling-summary refresh after {}s idle",
            settings.summary_idle_secs.max(30)
        );
    }

    // ── Idle conversation re-titling ─────────────────────────────────────
    //
    // A conversation's first title is the first six words of the first thing
    // said in it. That is reliable and unmemorable, and it is what the sidebar
    // shows for the rest of that conversation's life. This pass replaces those
    // with a name worth reading, and revisits one once its conversation has
    // moved substantially past what the name describes.
    //
    // Same contract as memory consolidation, for the same reason — there is
    // one on-device inference slot:
    //   - never at startup: real user activity must have been seen since boot
    //   - only after INACTIVITY_THRESHOLD_SECS of quiet, measured from EITHER
    //     the in-process clock or the newest session row, so a voice turn in
    //     the separate child process counts as somebody being here
    //   - abandoned the instant anyone comes back, having written nothing
    //
    // It additionally stands down while consolidation holds the slot. Both are
    // background chores and neither is worth making the other wait; two
    // concurrent model calls on a six-core Orin is precisely the contention
    // the speculative-ASR work spent a week measuring.
    //
    // Note that the two background title writers deliberately do NOT touch
    // `sessions.updated_at`. That column is one of the two activity sources
    // above, so a job that stamped it would read as a person returning:
    // it would cancel itself partway through its own first pass, and push the
    // idle clock forward every time it ran.
    {
        use pond_core::shared::domain::session_activity::SessionOrigin;
        use pond_core::shared::services::session_title::{RetitleOutcome, SessionTitleService};
        use pond_core::user_data::services::consolidation_schedule as sched;
        use pond_core::user_data::services::inference_lane::LaneJob;

        // How often to consider a pass. The gate, not this, decides whether one
        // actually runs.
        const POLL_SECS: u64 = 5 * 60;
        // Conversations renamed per pass. A pond with hundreds of them should
        // not spend a whole idle window on titles, and the next pass is only
        // five minutes away.
        const MAX_PER_PASS: usize = 5;

        let title_storage = session_storage.clone();
        let title_provider = llm_provider.clone();
        let title_activity = last_user_activity.clone();
        let title_settings_repo = settings_repo.clone();
        let title_lane = inference_lane.clone();
        let title_wake = inference_lane.claim(LaneJob::Titling);

        // Baselines for the "never on startup" guard, captured before the
        // server binds so no request can have been served yet.
        let started_at = std::time::Instant::now();
        let started_at_utc = chrono::Utc::now();

        tokio::spawn(async move {
            let idle_threshold = std::time::Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS);

            loop {
                let tick = crate::inference_lane_runner::wait_for_tick(
                    std::time::Duration::from_secs(POLL_SECS),
                    &title_wake,
                )
                .await;

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

                // The pairwise "stand down while consolidation is mid-run" check
                // that used to live here is gone, and deliberately so: it only
                // ever ran in ONE direction — consolidation never learned to
                // yield to titling — and every job added after it would have
                // needed its own check against every existing job. The lane
                // holds one slot, so exclusion is now a property of asking
                // rather than a list of jobs to remember.
                // The tick IS the floor on a scheduled pass: a pass is bounded
                // and cheap, so there is no reason to space passes further
                // apart than the poll already does. Never exempt either — no
                // turn since boot means no conversation to name.
                let cadence = crate::inference_lane_runner::Cadence::new(
                    std::time::Duration::from_secs(POLL_SECS),
                    idle_threshold,
                    false,
                );

                let Some(slot) = title_lane
                    .acquire(
                        LaneJob::Titling,
                        // Re-read every tick, so the toggle takes effect
                        // without a restart.
                        settings.session_titling_enabled,
                        cadence,
                        tick.waives(),
                        saw_activity_since_start,
                        idle_for,
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

                // One token for the whole pass: activity resuming should end
                // the sweep, not just the conversation being named at the time.
                let cancel = tokio_util::sync::CancellationToken::new();

                let watcher_activity = title_activity.clone();
                let watcher_storage = title_storage.clone();
                let watcher_cancel = cancel.clone();
                let watcher_baseline_in_process = in_process_at;
                let watcher_baseline_db = db_activity;
                let watcher = tokio::spawn(async move {
                    // The in-process clock is a lock read, so poll it fast. The
                    // DB check is a query, so sample it every Nth tick rather
                    // than hammering SQLite for the length of the pass.
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
                                // A transient read failure reads as None, which
                                // must not be mistaken for activity.
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
                    // The pond opens conversations for its own background work
                    // (a cron line firing at 3am mints one). Those are not
                    // conversations anybody browses, so naming them would spend
                    // the inference slot on a row nobody reads.
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

                // An attempt consumes the interval budget whether or not it
                // renamed anything, for the same reason consolidation does:
                // retrying a fruitless pass every tick is the churn the floor
                // exists to prevent. Dropping the guard does both, on every
                // path out — including the two early returns above, which is
                // what stopped this loop starving the lane.
                drop(slot);
            }
        });
        tracing::info!(
            "conversation re-titling active — runs after {} min idle, at most {} per pass (enable toggle is live)",
            sched::INACTIVITY_THRESHOLD_SECS / 60,
            MAX_PER_PASS,
        );
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
        // Build the provider when weather is on and we have *either* explicit
        // coordinates *or* a location name. Onboarding only stores a name (the
        // coordinates default to 0), so requiring coordinates here left every
        // onboarded install with weather permanently "not configured"; the
        // adapter geocodes the name on demand.
        // Asked, not read. `Location::weather_target` is the one place that
        // decides whether this pond knows enough to ask about the weather, and
        // it is the same answer voice mode gets below — these were two copies
        // of the same six lines, and both of them missed the time-zone
        // fallback that `location::resolve` has always applied.
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

    // Private mesh (#132 Milestone 2) — real libp2p MeshTransport, gated on
    // settings.mesh_enabled. Wired into AppState below (Milestone 6) so the
    // /api/v1/mesh/* routes can use it.
    let mesh_transport =
        build_mesh_transport(&settings, &settings_repo, peer_directory.clone()).await;

    // Private mesh (#132 Milestone 5) — Lightning settlement rail. Built
    // before build_mesh_provider so the mesh responder can answer inbound
    // InvoiceRequests immediately once the transport is up, rather than
    // racing a later wire-in.
    let payment_rail = build_payment_rail(&settings, &settings_repo, &data_dir).await;

    // Private mesh (#132 Milestones 3-6) — the LlmProvider a trusted peer can
    // borrow, plus capability-query and invoice-request handles into the same
    // MeshInferenceService singleton. All three are `None` when
    // mesh_transport is `None` (mesh disabled or built without the `mesh`
    // feature).
    let (mesh_provider, peer_capability_query, invoice_requester) = build_mesh_provider(
        &mesh_transport,
        peer_directory.clone(),
        credit_ledger.clone(),
        usage_tally.clone(),
        settings_repo.clone(),
        llm_provider.clone(),
        payment_rail.clone(),
    );
    // Settles at MESH_SETTLEMENT_MILLISATS_PER_TOKEN, the one dev-decided
    // rate every Pond uses (not a per-install setting — see that constant's
    // own docs on why). NOTE this closes over whatever `invoice_requester`
    // was built above — if mesh gets hot-enabled later via `mesh_rebuild`
    // below, this job does NOT pick up the fresh one. Settlement stays
    // restart-only for now; only mesh borrowing/lending itself is made
    // hot-reloadable here.
    spawn_settlement_job(
        peer_directory.clone(),
        usage_tally.clone(),
        payment_rail.clone(),
        invoice_requester,
    );

    // Wrapped in locks (not fixed values) so enabling mesh from
    // PUT /api/v1/settings takes effect immediately instead of requiring a
    // restart — see `mesh_rebuild` and AppState::mesh_rebuild's own docs.
    // GooseAdapter's own mesh_provider field shares this exact lock (see
    // `.with_mesh_provider` below), so a chat turn picks up a freshly-built
    // provider the moment this fires, with no separate wiring needed.
    let mesh_transport = Arc::new(tokio::sync::RwLock::new(mesh_transport));
    let mesh_provider = Arc::new(tokio::sync::RwLock::new(mesh_provider));
    let peer_capability_query = Arc::new(tokio::sync::RwLock::new(peer_capability_query));

    // Runtime mesh enable, no restart: `update_settings` calls this after
    // saving whenever the patch touches `mesh_enabled` and the new value is
    // true. Re-reads settings itself and is always safe to call — it
    // no-ops when the stack is already built (MeshInferenceService is a
    // singleton; see its own docs on why a second one would break
    // mesh_transport's single `recv()` consumer), when mesh is still
    // disabled, or when this binary lacks the `mesh` feature.
    //
    // Deliberately does not tear anything down on disable: mesh_enabled has
    // only ever gated construction here, never the behaviour of an
    // already-built stack (same as build_mesh_transport/build_mesh_provider
    // always worked), so this keeps that contract rather than inventing a
    // new one.
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
                    return; // already built
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

    // ── Embedding backfill ───────────────────────────────────────────────────
    // Extraction stored `embedding: None` before Phase A, and `search_similar`
    // ignores unembedded rows entirely — so without this pass the semantic
    // injection path would see only the handful of rows written by the
    // `save_memory` MCP tool. Batched with a pause between batches: on a Jetson
    // this competes with inference for CPU.
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
            // Then repair rows embedded by a DIFFERENT model. The backfill above
            // cannot see them (it selects `embedding IS NULL` and a stale vector is
            // not null), and semantic search deliberately EXCLUDES them because a
            // vector of another width is not comparable -- so without this pass a
            // pond that changed `embedding_provider` would look fully embedded and
            // silently retrieve worse forever.
            //
            // After the backfill rather than before it: a row with no vector at all
            // is invisible to search, while a stale one is merely excluded, so the
            // never-embedded rows are the more urgent repair.
            relevance::run_dimension_repair(
                backfill_repo.as_ref(),
                provider.as_ref(),
                relevance::BACKFILL_BATCH_SIZE,
                relevance::BACKFILL_BATCH_PAUSE_MS,
            )
            .await;
        });
    }

    // ── Personal-context index maintenance (phases B + D) ────────────────────
    // One pass, composed rather than three ad-hoc spawns: adopt existing vectors
    // (free, pure SQL), embed the summaries that have none (the only step that
    // costs inference), prune orphans, then REPORT what is still wrong.
    //
    // Order matters and is asserted in `run_index_maintenance`: pruning before
    // adopting would delete rows adoption is about to legitimately re-create.
    //
    // Deferred by `index_maintenance_delay_secs` rather than run at boot: a
    // household's first turn after an upgrade must not be slow because the pond
    // chose that moment to index itself. It is cancellable for the same reason.
    // Whether the sweep below exists at all is now carried by
    // `index_reindex_handle` rather than by a separate boolean: the handle is
    // taken inside the block that spawns the sweep, so it cannot disagree with
    // whether the sweep exists. The boolean it replaces was derived from the
    // same two options as this `if let` and could only ever have drifted from
    // it by somebody editing one of the two.
    if let (Some(provider), Some(_)) = (embedding_provider.clone(), vector_model_id.clone()) {
        use pond_core::user_data::services::inference_lane::LaneJob;

        let index = vector_index.clone();
        let storage = session_storage.clone();
        // Step 2c's repairer. The startup backfill above runs once; this is what
        // keeps an unembedded row from surviving until the next restart.
        let sweep_memories = memory_repo.clone();
        let cancel = index_maintenance_cancel.clone();
        let sweep_lane = inference_lane.clone();
        let reindex = inference_lane.claim(LaneJob::IndexMaintenance);
        // The Reindex button is a person, so it rings the HAND bell.
        index_reindex_handle = Some(reindex.hand_bell());
        let sweep_activity = last_user_activity.clone();
        let sweep_storage = session_storage.clone();

        let started_at = std::time::Instant::now();
        let started_at_utc = chrono::Utc::now();

        tokio::spawn(async move {
            use pond_core::context::index_maintenance::{plan_sweep, run_index_maintenance};
            // Same alias the other three schedule blocks in this file use. The
            // sweep reads the shared inactivity threshold so it waits on the
            // same definition of "idle" as consolidation, rather than a second
            // one that could drift.
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
                // Woken either by the clock or by somebody asking. Which one it
                // was changes the gate below, so it is remembered rather than
                // collapsed into "something happened".
                let woke = tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(poll) => crate::inference_lane_runner::Tick::Poll,
                    rang = reindex.rang() => rang,
                };
                let asked = woke.waives();

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

                // A requested pass is not background work, so it does not wait
                // for quiet. Somebody pressed Reindex and is watching an empty
                // panel; the idle gate exists to stop chores stealing the slot
                // from a person, and here the person IS the reason to run.
                //
                // Exclusion is untouched by this: the lane holds one slot and
                // that is what serialises jobs. The floor drops too, or a manual
                // pass would be refused for the sole reason that the scheduled
                // one had just happened.
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
                        // No toggle of its own: an index nobody asked to stop
                        // maintaining is an index quietly going stale, which is
                        // the failure this whole surface exists to end. Whether
                        // there is anything to embed is already answered by the
                        // embedding provider being present at all.
                        true,
                        // The standing cadence. `floor` and `idle_threshold`
                        // are already zeroed above when `asked`, which is this
                        // job's own older waiver and stays -- `plan_sweep` also
                        // uses it to decide an exhaustive first pass. The
                        // exemption is likewise the sweep's own, from
                        // `plan_sweep`, not the lane's hand waiver.
                        crate::inference_lane_runner::Cadence::new(
                            floor,
                            idle_threshold,
                            tick.exempt_from_activity_gate,
                        ),
                        // A person asking is itself the activity this guard
                        // wants to have seen; so is the first pass after boot,
                        // on a pond that would otherwise refuse forever.
                        woke.waives(),
                        saw_activity_since_start,
                        idle_for,
                    )
                    .await
                else {
                    continue;
                };

                // An exempt pass is exhaustive, so it must be interruptible --
                // the alternative is a pond that boots, finds a mailbox to
                // embed, and cannot be told to stop. A CHILD token so that
                // giving the machine back does not also cancel the sweep task
                // for the life of the process.
                let pass = cancel.child_token();
                // The baseline is the moment this pass was admitted. The
                // watcher cancels only on activity NEWER than it — somebody
                // actually came back — never on activity that merely happened
                // recently. The previous predicate (`elapsed() < chore_idle`)
                // judged recency, and for a requested pass that inverted the
                // gate's own decision: the gate waives idleness because the
                // person pressing Reindex IS the reason to run, and then the
                // watcher saw that same person's turn, still under fifteen
                // minutes old, and killed the pass at its first tick — after
                // the route had already CLEARED the index. Measured: press
                // Reindex within 15 minutes of any turn and the pass died at
                // ~15s with requested=true interrupted=true still_missing=1073.
                // The same predicate also made the first-post-boot pass a
                // near-miss: boot initialises the activity clock, and the pass
                // fires at 16 minutes against a 15-minute threshold — one
                // slow poll from cancelling itself forever.
                let baseline = *sweep_activity.read().await;
                let watcher = tokio::spawn({
                    let pass = pass.clone();
                    let activity = sweep_activity.clone();
                    async move {
                        // Only the in-process timestamp: it is written the
                        // moment a turn starts, whereas the database one lags
                        // by however long that turn takes to persist. This is
                        // the signal that says "somebody is here NOW".
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
                    sweep_memories.as_ref(),
                    provider.as_ref(),
                    &pass,
                    tick.budget,
                )
                .await;
                watcher.abort();

                // The exemption is spent only by a pass that finished. One cut
                // short by a member coming back has not indexed the backlog,
                // and treating it as done would leave the pond in exactly the
                // state the exemption exists to prevent.
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
                        memories = report.memories_indexed,
                        still_missing = report.still_missing,
                        "personal-context index pass finished"
                    );
                }
            }
        });
    }

    // ── Batch memory extraction ──────────────────────────────────────────────
    //
    // This is now the ONLY thing that writes an extracted memory. It reads one
    // window of one conversation per lane slot, in the pond's idle time, and
    // there is no longer a per-turn path above it: every surface that persists
    // a turn -- including `/chat` and the voice loop, which never extracted at
    // all -- reaches memory through this walk.
    //
    // The whole block is gated on an embedder being present. Without one there
    // is no cosine, so there is no dedup: every candidate would look new, and
    // the pond would fill its own store with restatements of things it already
    // knows. Reading nothing is the better failure, and it is the one that says
    // so out loud through `blocked_on`.
    let extraction_status = Arc::new(tokio::sync::RwLock::new(
        pond_core::user_data::services::memory_extraction::ExtractionEngineStatus {
            mode: pond_core::user_data::services::memory_extraction::ExtractionMode::parse(
                &settings.memory_extraction_mode,
            )
            .as_str()
            .to_string(),
            ..Default::default()
        },
    ));
    if let Some(provider) = embedding_provider.clone() {
        use pond_core::user_data::services::consolidation_schedule as sched;
        use pond_core::user_data::services::inference_lane::LaneJob;
        use pond_core::user_data::services::memory_extraction::{
            BatchExtractionConfig, BatchExtractionService, HouseholdRoster,
        };
        use pond_core::user_data::services::reminder_proposal;

        /// How many pending reminders one tick will consider.
        ///
        /// Bounded for the same reason the pass itself is: a first walk over a
        /// year of history can file a great many, and the daily cap means only a
        /// handful of them could become proposals anyway.
        const REMINDER_PROMOTION_LIMIT: usize = 50;

        // Where a dated utterance goes once the date rule has refused it as a
        // memory. Wired here rather than left to a later phase because the
        // engine is the only producer: without it the refusal throws the date
        // away, which is the one outcome the rule was justified on not having.
        let reminder_repo: Arc<
            dyn pond_core::user_data::ports::reminder_repository::ReminderRepository + Send + Sync,
        > = Arc::new(pond_infra::sqlite_reminder::SqliteReminderRepository::new(
            db.system.clone(),
        ));

        // The same store the engine writes through, kept for the promotion run
        // below. One store, so a reminder written by the pass is one the
        // promotion can see in the same tick.
        let promotion_reminders = reminder_repo.clone();
        let promotion_proposals = Arc::new(
            pond_infra::sqlite_proposal::SqliteProposalRepository::new(db.system.clone()),
        );

        let service = Arc::new(
            BatchExtractionService::new()
                .with_embedding_provider(provider)
                .with_reminder_repository(reminder_repo),
        );
        let extraction_storage = session_storage.clone();
        let extraction_repo = memory_repo.clone();
        let extraction_settings = settings_repo.clone();
        let extraction_activity = last_user_activity.clone();
        let extraction_lane = inference_lane.clone();
        let extraction_wake = inference_lane.claim(LaneJob::MemoryExtraction);
        let extraction_provider = llm_provider.clone();
        let extraction_profiles = profile_repo.clone();
        let status = extraction_status.clone();

        // Baselines for the "never on startup" guard, captured before the
        // server binds so no request can have been served yet.
        let started_at = std::time::Instant::now();
        let started_at_utc = chrono::Utc::now();

        tokio::spawn(async move {
            use crate::conversation_extractor::LlmConversationExtractor;

            let poll = std::time::Duration::from_secs(MEMORY_EXTRACTION_POLL_SECS);
            tokio::time::sleep(std::time::Duration::from_secs(MEMORY_EXTRACTION_DELAY_SECS)).await;

            let extractor = LlmConversationExtractor::new(extraction_provider);

            loop {
                let tick =
                    crate::inference_lane_runner::wait_for_tick(poll, &extraction_wake).await;

                let settings = match extraction_settings.get().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("[batch-extraction] settings read failed: {e}");
                        continue;
                    }
                };
                // Who lives here, read fresh each pass. It decides whether an
                // unattributed conversation may be mined under the pond-wide
                // name or has to be left alone, so a stale roster is the
                // difference between remembering somebody's habits and filing
                // them under the wrong person.
                //
                // A failed read is treated as SEVERAL members. That is the
                // narrowing direction: it makes every unattributed window
                // unnameable, so the pass reads nothing rather than attributing
                // everything to one name on the strength of a query that did
                // not answer.
                let roster = match extraction_profiles.list().await {
                    Ok(profiles) => HouseholdRoster::new(
                        profiles
                            .into_iter()
                            .map(|p| (p.id, p.display_name))
                            .collect(),
                    ),
                    Err(e) => {
                        tracing::debug!("[batch-extraction] profile list failed: {e}");
                        HouseholdRoster::new(vec![
                            ("unreadable-a".to_string(), String::new()),
                            ("unreadable-b".to_string(), String::new()),
                        ])
                    }
                };
                let config = BatchExtractionConfig::from_settings(&settings).with_household(roster);

                let db_activity = newest_session_activity(extraction_storage.as_ref()).await;
                let in_process_at = *extraction_activity.read().await;
                let now = chrono::Utc::now();
                let saw_activity_since_start = sched::saw_activity_since_start(
                    started_at,
                    in_process_at,
                    started_at_utc,
                    db_activity,
                );
                let idle_for = sched::combined_idle_for(in_process_at, db_activity, now);

                // The poll is the floor on a scheduled tick, unless the
                // operator has asked for a longer one. Never exempt either: no
                // turn since boot means no conversation anybody is waiting to
                // have remembered, and this is the most expensive job in the
                // lane to spend on a guess. A hand-asked tick drops both --
                // this is the job most likely to have been waiting longest, and
                // the one a household pressing the button is usually after.
                let cadence = crate::inference_lane_runner::Cadence::new(
                    poll.max(std::time::Duration::from_secs(
                        settings.memory_extraction_interval_secs as u64,
                    )),
                    std::time::Duration::from_secs(settings.memory_extraction_idle_secs as u64),
                    false,
                );

                let Some(slot) = extraction_lane
                    .acquire(
                        LaneJob::MemoryExtraction,
                        // Health, never emptiness. The rejected predicate --
                        // "stand down while any memory row lacks a vector" --
                        // is a latch that cannot re-open inside a process:
                        // three ordinary paths mint unembedded rows and only a
                        // one-shot startup backfill fills them, so the design's
                        // own restate path would have disabled the engine
                        // permanently.
                        settings.memory_extraction_enabled && service.embedder_is_usable(),
                        // The poll is the floor, unless the operator has asked
                        // for a longer one. The key now has exactly one reader
                        // and one meaning -- how rarely a pass may take the
                        // slot -- and taking the larger of the two keeps it
                        // one-directional: it can make passes rarer than the
                        // tick and never more frequent.
                        cadence,
                        tick.waives(),
                        saw_activity_since_start,
                        idle_for,
                    )
                    .await
                else {
                    continue;
                };

                // One token for the whole pass, checked before each window: a
                // pass that loses the household mid-window loses at most that
                // one window's inference rather than three windows' worth of
                // work nobody will use.
                let cancel = tokio_util::sync::CancellationToken::new();
                let watcher_cancel = cancel.clone();
                let watcher_activity = extraction_activity.clone();
                let baseline = in_process_at;
                let watcher = tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        if watcher_cancel.is_cancelled() {
                            break;
                        }
                        // The in-process clock only: it is written the moment a
                        // turn starts, whereas the database one lags by however
                        // long that turn takes to persist. This is the signal
                        // that says somebody is here NOW.
                        if *watcher_activity.read().await > baseline {
                            tracing::debug!(
                                "user activity resumed — ending the memory-extraction pass"
                            );
                            watcher_cancel.cancel();
                            break;
                        }
                    }
                });

                let report = service
                    .run_pass(
                        extraction_storage.as_ref(),
                        extraction_repo.as_ref(),
                        &extractor,
                        &config,
                        &cancel,
                    )
                    .await;
                watcher.abort();

                status.write().await.record(config.mode, &report);

                // What a stored reminder can become, attempted after the pass
                // rather than inside it. The table is the durable half and it
                // has already been written by this point; this is the best-effort
                // half, and running it separately is what keeps a proposal
                // failure from ever being a reason a date was not kept.
                //
                // On a pond with no profile rows -- every pond today -- this
                // promotes nothing and says so per reminder, because
                // `ProposalAudience` cannot address `Household`. That is the
                // expected answer here, not a fault, and the reminder stays
                // pending and readable either way.
                match reminder_proposal::promote_pending_reminders(
                    promotion_reminders.as_ref(),
                    promotion_proposals.as_ref(),
                    REMINDER_PROMOTION_LIMIT,
                    chrono::Utc::now(),
                )
                .await
                {
                    Ok(promotion) if promotion.considered > 0 => {
                        // Counts only. The reminder's own sentence is the
                        // household's private words and never rises above DEBUG,
                        // the same rule the pass line above follows.
                        tracing::info!(
                            target: "giap::trace",
                            kind = "reminder_promotion",
                            considered = promotion.considered,
                            proposed = promotion.proposed,
                            // Expected on a pond with nobody on file, and the
                            // reason the table is the deliverable.
                            unaddressable = promotion.unaddressable,
                            capped = promotion.capped,
                            // The only one of the four that is a fault.
                            failed = promotion.failed,
                            "pending reminders considered for the proposal queue"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        // The dates are still in the table. Said at WARN anyway:
                        // a promotion that never runs is a queue the household
                        // never sees fill.
                        tracing::warn!(
                            "[reminders] could not consider pending reminders for proposals: {e}"
                        );
                    }
                }

                // Anything that SPENT the slot is logged, not only anything
                // that succeeded. A pass whose every call came back unparseable
                // examines no window and is not blocked -- the model is
                // answering, just not in the schema -- and gating this line on
                // success would make the most expensive failure mode the
                // quietest one.
                if report.model_calls > 0 || report.blocked_on.is_some() {
                    // Counts and bands only. The notes themselves go to DEBUG
                    // inside the service and never to INFO: INFO is what the
                    // on-disk log under <data_dir>/logs keeps, and a would-be
                    // memory written there is the household's private sentence
                    // in a second place with none of the store's scoping,
                    // retention or redaction.
                    tracing::info!(
                        target: "giap::trace",
                        kind = "memory_extraction_pass",
                        mode = config.mode.as_str(),
                        windows = report.windows_examined,
                        // What the pass actually spent on the inference slot.
                        // Logged beside `windows` because the two differ
                        // exactly when something is wrong, and the gap is the
                        // number worth watching.
                        model_calls = report.model_calls,
                        deadline_reached = report.deadline_reached,
                        skipped = report.windows_skipped,
                        oversized = report.windows_oversized,
                        already_mined = report.windows_already_mined,
                        provider_failures = report.provider_failures,
                        unattributed_sessions = report.sessions_unnameable,
                        written = report.memories_written,
                        dropped = report.memories_dropped,
                        refused = report.memories_refused,
                        // What the date rule cost, and what it cost that
                        // nothing else recovered. `dates_lost` above zero is a
                        // model ignoring the reminders half of the prompt.
                        dated = report.memories_dated,
                        dates_lost = report.memories_dates_lost,
                        demoted = report.memories_demoted,
                        reminders_captured = report.reminders_captured,
                        // What the pass KEPT, beside what it read. The two
                        // differ when the store refused a write, and that gap
                        // is a date the pond no longer has -- invisible from
                        // every other number on this line.
                        reminders_written = report.reminders_written,
                        reminders_lost = report.reminders_lost,
                        offered = report.memories_offered,
                        reminders = report.reminders_offered,
                        rejected = report.rejected_kinds,
                        parse_failures = report.parse_failures,
                        gave_up = report.gave_up,
                        resets = report.cursor_resets,
                        band_same = report.bands.same,
                        band_related = report.bands.related,
                        band_new = report.bands.fresh,
                        band_unscored = report.bands.unscored,
                        blocked_on = report.blocked_on.as_deref().unwrap_or(""),
                        "memory extraction pass finished"
                    );
                }

                // Dropping the guard records the run and releases the slot, on
                // every path out. That is the whole reason it is a guard: a job
                // that returns early without recording a run keeps
                // `since_last_run: None`, which the lane treats as infinitely
                // starved, so it would win every tick forever and lock every
                // other job out.
                drop(slot);
            }
        });
        tracing::info!(
            "batch memory extraction active (mode={}) — reads one window per pass after {} min \
             of household quiet",
            settings.memory_extraction_mode,
            settings.memory_extraction_idle_secs / 60,
        );
    } else {
        // Said in the status as well as the log. A field nobody reads is not a
        // surface, and from the outside a pond whose embedder never loaded is
        // indistinguishable from one with nothing left to extract.
        extraction_status.write().await.blocked_on = Some("no_embedder".to_string());
        tracing::info!(
            "batch memory extraction inactive — no embedding provider, so nothing could be \
             deduplicated and the pond would store a restatement of everything it already \
             knows. NOTHING else extracts memories now, so this pond is not learning."
        );
    }

    // ── Agent backend ────────────────────────────────────────────────────────────
    // pond_agent_active is always false while the backend is quarantined (Q2-05).
    // agent_backend has already been normalised to "goose" above.
    #[cfg(feature = "pond-agent")]
    let pond_agent_active = agent_backend == "pond"; // stays false: quarantine override above
    #[cfg(not(feature = "pond-agent"))]
    let pond_agent_active = false;

    // The mistral.rs backend is a checkpoint, not a quarantine: unlike "pond" it
    // is reachable, because nothing rewrites the value and no guard rejects it.
    // What keeps it out of production is the cargo feature, which is off by
    // default and which no device build turns on.
    #[cfg(feature = "mistralrs-agent")]
    let mistralrs_active = agent_backend == pond_adapters_mistralrs::BACKEND_NAME;
    #[cfg(not(feature = "mistralrs-agent"))]
    let mistralrs_active = false;

    // Event bus (#91/#109), created here because the Matter runtime below
    // publishes sensor updates onto it. Its durable log bridge is wired further
    // down, once the logs DB handle is in scope.
    let event_bus: Arc<dyn pond_core::shared::ports::event_bus::EventBus> =
        Arc::new(InProcessEventBus::new());

    // Device actuation backend (#195): Matter when it is switched on and a
    // controller is reachable, else the logging stub.
    //
    // Which of the two is live is the runtime's decision and can change at any
    // moment, because the controller can come and go at runtime rather than being a
    // boot-time constant. `device_control` is therefore a facade — one `Arc`
    // that the agent, the MCP server, and the tool wiring hold for the life of
    // the process while the backend behind it is swapped underneath.
    //
    // The runtime also owns the bridge (fabric node sync plus sensor attribute
    // updates onto the EventBus) and the controller process, so both come and
    // go with the toggle rather than with the process.
    type MatterRuntimeHandle =
        Option<Arc<dyn pond_core::user_data::ports::matter_runtime::MatterRuntimePort>>;
    type DeviceControl = Arc<dyn pond_core::user_data::ports::device_control::DeviceControlPort>;

    // The concrete runtime is kept alongside the trait object because
    // `attach_notifications` is an adapter concern, not part of `MatterRuntimePort`:
    // putting it on the port would make every mock and stub in the workspace
    // answer for a method that only one implementation has any use for.
    #[cfg(feature = "goose-agent")]
    let (matter_concrete, device_control): (
        Option<Arc<pond_adapters_matter::MatterRuntime>>,
        DeviceControl,
    ) = {
        // The bridge holds only a bus handle, so persistence is attached to the
        // handle (#90): the decorator records each BusEvent::Sensor before
        // forwarding it, giving Matter readings the same persist-before-publish
        // ordering `record_sensor` gets by writing inline (#91). Without it
        // nothing writes adapter-sourced readings, and `giap-sensors` answers
        // every question about a real Matter device with "none recorded" — a
        // device that is visibly in the device list and cannot be asked
        // anything, which reads as broken rather than as unimplemented.
        //
        // `AppState` keeps the PLAIN bus: `record_sensor` already persists
        // inline, and giving it the decorator too would double-write every
        // reading that arrives over the REST route.
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
        // NOT converged here: `apply` waits until the notification sender exists
        // further down, so a first-run controller install can tell the user it
        // has started. See the `matter.attach_notifications` call below.
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

    // Install the sensor MCP server's handles — `spawn_sensor_server` only fires at
    // chat time, so anywhere before the server serves is early enough.
    //
    // Down here, beside its siblings' call sites rather than with them, because it
    // needs `device_control`, which does not exist until the Matter runtime above is
    // built. That handle is what lets `get_sensor_reading` ask a reachable device what
    // it reads NOW instead of serving the newest row in the log — and the newest row
    // can be hours old, because the controller publishes only CHANGES, so a steady
    // sensor is written once and never again.
    pond_mcp_server::init_sensor_deps(
        sensor_storage.clone(),
        device_registry.clone(),
        device_control.clone(),
    );

    let (agent, extension_manager, _tool_caller, tool_registry) = if mistralrs_active {
        // A turn served without goose: pond-core's system prompt, the same MCP
        // dispatcher PondAgent uses, and an HTTP client. No extension manager
        // and no tool-calling specialist, because neither exists off the goose
        // path — the registry below is the empty default for the same reason.
        let default_registry: Arc<
            dyn pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort,
        > = Arc::new(pond_core::mcp::services::tool_registry::InMemoryToolRegistry::new());

        #[cfg(feature = "mistralrs-agent")]
        let agent: Arc<dyn Agent> = {
            let settings = settings_repo.get().await.unwrap_or_default();
            let base_url = pond_adapters_mistralrs::base_url_from_env();
            // The window mistral.rs was actually started with is not on its API,
            // so GIAP's own resolution is the only source. Wrong here means a
            // history budget the server will refuse, not a slow turn.
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

    // Secret storage — $DATA_DIR/secrets.json, encrypted with XChaCha20-Poly1305
    // under $DATA_DIR/secrets/master.key (0600, 0700 directory, overridable with
    // POND_SECRET_KEY_FILE). Initialized before MCP auto-connect so startup can
    // resolve OAuth tokens.
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
                // A locked store is not the same failure as a broken one, and
                // the difference decides what the operator should do next. The
                // repository stays None either way: with no repository the
                // secret routes answer 503 and nothing in this process can
                // overwrite the ciphertext, which is still sitting there
                // recoverable if the key turns up.
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

    // Hand the SAME instance to the MCP tool servers. One instance per process
    // is a requirement, not tidiness: `FileSecretRepository` caches
    // secrets.json in memory and rewrites it whole on `set`, so a second
    // instance would serve a stale cache and clobber this one's writes.
    // The servers that consume these handles only spawn at chat time, so
    // installing here (after the agent backend was built) is in time.
    if let Some(repo) = &secret_repo {
        pond_mcp_server::init_secret_deps(repo.clone());

        // PAI-2 P2: move any API key an existing pond already had in its
        // settings table. The fields are gone from `Settings`, so an unmigrated
        // row is simply ignored by `apply_key` — to the user that looks like
        // their key vanished.
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

    // Marketplace — curated registry of installable extensions.
    // Initialized before MCP auto-connect so startup can look up required_secrets.
    // The asset root anchors entries that launch a bundled script, so they no
    // longer depend on which directory pond-server happened to be started from.
    let asset_root = asset_root::resolve();
    tracing::info!(asset_root = %asset_root.display(), "resolved extension asset root");
    let marketplace: Arc<dyn pond_core::mcp::ports::extension_marketplace::ExtensionMarketplace> =
        Arc::new(
            pond_core::mcp::services::marketplace::BundledMarketplace::with_asset_root(&asset_root),
        );

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

                        // Start with the persisted env, then overwrite every
                        // secret from the secret repository.
                        //
                        // The repository is authoritative for credentials; the
                        // env stored on the row is a snapshot taken when the
                        // extension was installed. Preferring the snapshot
                        // handed the child an access token that had since been
                        // rotated, on every restart, for as long as the row
                        // survived — so overwrite rather than fill the gaps.
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

                        // The internal token is a fresh UUID per process run, so
                        // the persisted copy is always from a dead process. Left
                        // alone, the child authenticates to /oauth/refresh with
                        // it, gets a 401, and can never recover from an expired
                        // access token — the extension simply stops working an
                        // hour after every restart.
                        //
                        // GIAP_SERVER_URL is deliberately left as persisted:
                        // auto-connect runs before the listener binds, so the
                        // port is not known here. Install and the OAuth callback
                        // both run after binding and write the correct value.
                        env.insert(
                            pond_api::oauth_callback::INTERNAL_TOKEN_ENV_KEY.to_string(),
                            pond_api::oauth_callback::internal_extension_token().to_string(),
                        );

                        // A config persisted before extension paths were anchored
                        // still carries a cwd-relative arg, which only resolves
                        // when the server is launched from the repo root. Re-anchor
                        // it here so an existing install heals on restart instead
                        // of needing a manual remove-and-reinstall.
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
    let operational_log: Option<
        Arc<dyn pond_core::security::ports::event_log::OperationalLogRepository>,
    > = Some(Arc::new(SqliteOperationalLog::new(db.logs.clone())));

    // Privacy/security boundary hook — audits into the unified event log (#108)
    // as `Auth` events, so a policy decision is correlatable with the rest of a
    // session.
    //
    // This sink is a SEPARATE Arc from the shared `event_log` below, so it gets
    // its own chokepoint-2 wrapper. Leaving it raw would have been a bypass by
    // the one component whose entire job is the audit trail: P1 classifies
    // these rows `Sensitive` precisely because they carry a `token:<client_id>`
    // principal label and a remote address.
    let security_policy: Option<Arc<dyn pond_core::security::ports::policy::SecurityPolicy>> =
        Some(Arc::new(
            pond_infra::sqlite_security_policy::SqliteSecurityPolicy::new(Arc::new(
                pond_core::security::services::redacting_event_log::RedactingEventLog::new(
                    Arc::new(SqliteEventLog::new(db.logs.clone())),
                    redactor.clone(),
                ),
            )),
        ));

    // PAI-2 P1, second production call site: the draft MCP server is a
    // process-wide singleton with no principal of its own. What it does get,
    // per tool call, is the engine session id in the MCP request `_meta`. This
    // authority is what turns that id into a speaker, reads
    // `security_policy_mode` fresh, and records the decision. Installed here
    // rather than in register_giap_extensions because it needs three
    // repositories that registration does not carry, and after `security_policy`
    // so draft decisions land in the same audit trail as identity assertions.
    // The server is not spawned until the first turn, so this is in time.
    // Start routing WARN+ tracing events into the SQLite event log.
    // _file_guard must live until run_server returns so the background file
    // writer keeps flushing log output to disk.
    let _file_guard = drain_handle.drain_into(operational_log.clone());

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

    // Persist the ACTUAL bound port so the standalone `pond pairing` CLI can
    // build a pairing URL with the real port (bind_with_fallback may have picked
    // a fallback, or the operator passed --port). Best-effort; ignored on error.
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

    // Durable event log (#91/#109). The bus (created above, with the Matter
    // runtime) is shared with AppState for publishing on ingest; a background
    // bridge subscribes to it and appends every bus event (sensor/camera/device)
    // into the unified `events` table, so events written in normal operation are
    // queryable from pond_logs.db.
    // One shared event store: the bus→log bridge writes to it, and the activity
    // query API (#114) reads from it via AppState.
    // Chokepoint 2: attributes are redacted on the way in, and a finding raises
    // the event's sensitivity -- which excludes it from the audit MCP reads and
    // shortens its retention. Both narrow. This is also the binding
    // `set_egress_sink` reads a hundred lines below, so every outbound-call
    // record goes through the redactor without egress knowing.
    let event_log: Arc<dyn pond_core::security::ports::event_log::EventLog> = Arc::new(
        pond_core::security::services::redacting_event_log::RedactingEventLog::new(
            Arc::new(SqliteEventLog::new(db.logs.clone())),
            redactor.clone(),
        ),
    );
    // Push-token store (#95) — built here, before `db` is moved into AppState.
    let push_token_repo: Arc<dyn pond_core::user_data::ports::push_token::PushTokenRepository> =
        Arc::new(pond_infra::sqlite_push_token::SqlitePushTokenRepository::new(db.system.clone()));

    // Push-notification path (#99): in-process fan-out to connected
    // `/notifications/stream` clients + an offline queue + a (stub) FCM/APNs
    // relay backed by #95's push tokens.
    let (notification_tx, _) =
        tokio::sync::broadcast::channel::<pond_core::mcp::ports::notification::Notification>(256);
    let notification_queue: Arc<
        dyn pond_core::mcp::ports::notification_queue::NotificationQueueRepository,
    > = Arc::new(
        pond_infra::sqlite_notification_queue::SqliteNotificationQueue::new(db.system.clone()),
    );

    // Last resort: the pond will go on running slower than it could and nothing
    // else would ever say so.
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
    // Real FCM relay when a service-account key is present (Path B: direct
    // FCM v1, data-only wake pings — no Expo hop, no content through Google);
    // otherwise the logging stub. Key location:
    // `<data_dir>/secrets/fcm-service-account.json`, override POND_FCM_KEY_PATH.
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
    // PAI-1 P9's delivery half, and the first production caller the rung has had.
    //
    // Until this line `send_to_profile` answered `AttributionUnavailable` for
    // every member on every pond: the code was correct, tested, and unreachable.
    // That is the shape PAI-1 P5 shipped in once and PAI-6 P1's clamp shipped in
    // again, and it is invisible to `dead_code` because every symbol involved is
    // `pub` in a library crate.
    let device_attribution: Arc<
        dyn pond_core::user_data::ports::device_attribution::DeviceAttribution,
    > = Arc::new(
        pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(db.system.clone()),
    );

    // The concrete type is kept alongside the trait object on purpose.
    // `send_to_profile` is NOT on `NotificationSender` and must not be: the port
    // is `send` (one target) and `broadcast` (the household), and a third method
    // meaning "resolve a member to their devices" would put PAI-1's attribution
    // chain behind a trait every mock and stub in the workspace would have to
    // answer for. PAI-7 invariant 4 -- addressed to a profile, never broadcast --
    // is carried by `TargetedDelivery`, which has no variant meaning "everybody",
    // so the refusal cannot be reached by picking the wrong enum arm.
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
    // Let the `send_notification` MCP tool reach connected phones too (#99).
    pond_mcp_server::init_notification_sender(notification_sender.clone());

    // Matter can now tell the user things, so converge it (#195). Deferred to
    // here rather than left beside the runtime's construction because the first
    // enable on a fresh install downloads and installs a controller, which
    // legitimately takes minutes: without a sender attached first, the one
    // notification explaining that wait would be sent into nothing.
    //
    // Still returns immediately — serving must not wait on the install, and the
    // Devices tab shows the progress.
    #[cfg(feature = "goose-agent")]
    if let Some(matter) = &matter_concrete {
        matter
            .attach_notifications(notification_sender.clone())
            .await;
        matter.apply(pond_core::user_data::ports::matter_runtime::MatterConfig {
            url: settings.matter_ws_url.trim().to_string(),
            ble: settings.matter_ble_enabled,
        });

        // Bounded, so the Matter lines belong to the startup log rather than
        // arriving after the "listening" banner as though something had
        // restarted. Free when Matter is off, a second or two when its
        // controller is already installed, and abandoned rather than waited out
        // on a first run — which is the only case that takes minutes, and the
        // one the Devices tab is already reporting progress for.
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

    // Bridge schedule completion/failure events to push notifications (#99), so a
    // reminder/scheduled task surfaces on the phone, not just the dashboard.
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

    // ── PAI-7 P4: the proactive reviewer ─────────────────────────────────
    //
    // The loop `proactive_review.rs` was written for and has been waiting on.
    // Everything it decides lives in pond-core: when a review may start, who it
    // is for, what it may be told, what its answer becomes. What is here is a
    // clock, four store reads, and the delivery.
    //
    // Two structural facts decide the shape:
    //
    // 1. **A review is its own parent turn.** `GooseOrchestrator::spawn` refuses
    //    any spec whose parent session has no live entry in the
    //    `TurnAuthorityRegistry`, which a background loop fails by construction.
    //    So the loop publishes `review_authority` for the length of the run.
    //    That is not a way around the check -- the registry entry carries the
    //    cancellation token, so publishing is what makes invariant 3's
    //    interruption cascade to the child at all.
    // 2. **The orchestrator is read back out of the same `OnceLock` the
    //    `delegate` tool uses.** A second `GooseOrchestrator` would compile and
    //    then answer `None` to every authority lookup. There is exactly one, and
    //    when Goose init failed there is none -- which means no review.
    {
        let review_settings = settings_repo.clone();
        let review_storage = session_storage.clone();
        let review_activity = last_user_activity.clone();
        let review_sender = targeted_notification_sender.clone();
        let review_proposals: Arc<dyn pond_core::user_data::ports::proposal::ProposalRepository> =
            Arc::new(pond_infra::sqlite_proposal::SqliteProposalRepository::new(
                db.system.clone(),
            ));

        // The observed-event ring, filled by a bus subscriber and drained by the
        // reviewer. `brief_events` collapses repeats, so this only has to be big
        // enough that a burst between two reviews does not lose the one event
        // that mattered; it is sized off pond-core's own cap rather than a
        // number here, so the two cannot drift.
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
                    // `reviewable` refuses the clock tick and the session
                    // lifecycle. Doing that HERE rather than at review time is
                    // what stops an idle overnight pond filling the ring with
                    // its own heartbeat and evicting the evening's camera event.
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
            inference_lane.clone(),
            profile_repo.clone(),
        ));
    }

    // ── Composing suggestions out of the household's own memories ────────
    //
    // The other tier of `GET /api/v1/suggestions`. `suggestion.rs` picks among
    // seven fixed prompt strings and attaches a measured count; this composes a
    // question from one of the household's own notes. Queued rather than
    // generated on demand, because Home is glanced at from across a room and a
    // model call on this board is measured in seconds.
    {
        use pond_core::user_data::domain::profile::ProfileScope;
        use pond_core::user_data::services::consolidation_schedule as sched;
        use pond_core::user_data::services::inference_lane::LaneJob;

        /// How often a pass is considered. The gate decides whether one runs.
        const POLL_SECS: u64 = 10 * 60;
        /// How deep into the store a pass looks for notes nobody has been asked
        /// about. Larger than `MEMORIES_PER_PASS` because the live queue is
        /// subtracted first, and on a pond with a full queue the first N would
        /// otherwise all be ones already used.
        const MEMORY_SCAN: usize = 200;

        let gen_settings = settings_repo.clone();
        let gen_memories = memory_repo.clone();
        let gen_provider = llm_provider.clone();
        let gen_activity = last_user_activity.clone();
        let gen_storage = session_storage.clone();
        let gen_lane = inference_lane.clone();
        let gen_profiles = profile_repo.clone();
        let gen_wake = inference_lane.claim(LaneJob::SuggestionGeneration);
        let gen_queue: Arc<
            dyn pond_core::user_data::ports::suggestion_queue::SuggestionQueueRepository,
        > = Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        );

        // Baselines for the "never on startup" guard, captured before the
        // server binds so no request can have been served yet.
        let started_at = std::time::Instant::now();
        let started_at_utc = chrono::Utc::now();

        tokio::spawn(async move {
            let idle_threshold = std::time::Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS);

            loop {
                let tick = crate::inference_lane_runner::wait_for_tick(
                    std::time::Duration::from_secs(POLL_SECS),
                    &gen_wake,
                )
                .await;

                let settings = match gen_settings.get().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("[suggestions] settings read failed: {e}");
                        continue;
                    }
                };

                let db_activity = newest_session_activity(gen_storage.as_ref()).await;
                let in_process_at = *gen_activity.read().await;
                let now = chrono::Utc::now();
                let saw_activity_since_start = sched::saw_activity_since_start(
                    started_at,
                    in_process_at,
                    started_at_utc,
                    db_activity,
                );
                let idle_for = sched::combined_idle_for(in_process_at, db_activity, now);

                // Never exempt on a scheduled tick: a pond nobody has talked to
                // has no new notes to compose from. A hand-asked one is exempt,
                // because somebody pressing Run now IS the activity -- and this
                // is the job they are most likely to be pressing it for.
                let cadence = crate::inference_lane_runner::Cadence::new(
                    std::time::Duration::from_secs(POLL_SECS),
                    idle_threshold,
                    false,
                );

                let Some(slot) = gen_lane
                    .acquire(
                        LaneJob::SuggestionGeneration,
                        settings.suggestion_generation_enabled,
                        cadence,
                        tick.waives(),
                        saw_activity_since_start,
                        idle_for,
                    )
                    .await
                else {
                    continue;
                };

                // Re-read per tick, so adding a member takes effect on the
                // next pass rather than at the next restart. `user_name` is
                // included because on a pond with no profile rows it is the
                // only name the household has -- and that is exactly the pond
                // where every note is unattributed, so it is the name a model
                // reaches for when it starts addressing somebody.
                let mut subjects: Vec<String> = gen_profiles
                    .list()
                    .await
                    .map(|p| p.into_iter().map(|p| p.display_name).collect())
                    .unwrap_or_default();
                subjects.push(settings.user_name.clone());

                let outcome = compose_suggestions(
                    gen_memories.as_ref(),
                    gen_queue.as_ref(),
                    gen_provider.clone(),
                    &subjects,
                    MEMORY_SCAN,
                    now,
                )
                .await;

                // Said on EVERY pass that did anything at all, including one
                // that composed nothing -- which is the pass a reader most
                // needs explained. The titling loop's empty match arm is the
                // shape this is avoiding: a correct no-op and a silent bail
                // were indistinguishable there for 554 passes.
                match outcome {
                    Ok(report) => {
                        if report.considered > 0 {
                            tracing::info!(
                                target: "giap::trace",
                                kind = "suggestions_composed",
                                considered = report.considered,
                                queued = report.queued,
                                refused = report.refused,
                                unparseable = report.unparseable,
                                "composed suggestions from the household's memories"
                            );
                        } else {
                            tracing::debug!(
                                "[suggestions] nothing to compose from -- every recent memory \
                                 already carries a question"
                            );
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "suggestion generation failed"),
                }

                // Dropping the guard records the run and releases the slot on
                // every path out, including the early returns inside the call
                // above.
                drop(slot);
            }
        });
        tracing::info!(
            "suggestion composing active -- turns the household's own memories into questions, \
             after {} min idle",
            sched::INACTIVITY_THRESHOLD_SECS / 60,
        );
        let _ = ProfileScope::Household;
    }

    // ── PAI-8 P1: the personal-context corpus gets a producer ────────────
    //
    // Everything in `pond-core/src/context/` landed on 2026-08-11 with nothing
    // that could produce a `RawItem`, so `context_items` was empty on every
    // pond by construction and `context_pipeline_is_not_wired_yet.rs` asserted
    // it. This block is what that test was written to fail on.
    //
    // Three things decide the shape.
    //
    // 1. **The redactor is not optional.** `IngestPipeline::new` takes one by
    //    value rather than as an `Option`, so there is no "no redactor wired"
    //    fallback to lose invariant 3 through. This is also PAI-2 P3's third
    //    chokepoint finally getting a call site -- the one the ledger recorded
    //    as circularly blocked on PAI-8.
    // 2. **The toggle is answered per event, from the settings the loop last
    //    read**, not captured at boot. `BusIngest::absorb` reads it before it
    //    reads any store, so on a default pond the whole cost of this feature
    //    is a bool per bus event and no query.
    // 3. **The bus is subscribed once more rather than sharing the reviewer's
    //    ring.** They want different things: the reviewer wants a bounded
    //    window of recent household facts to reason over, this wants every
    //    event exactly once so nothing is silently dropped by a ring that
    //    wrapped.
    // Hoisted out of the block below so the router can reach it: the sweep and
    // the "check now" route must be the SAME syncer, or the button and the
    // timer become two implementations of one word.
    let mut account_syncer: Option<Arc<dyn pond_core::context::ports::AccountSync>> = None;
    {
        let context_repo: Arc<dyn pond_core::context::ports::ContextRepository> = Arc::new(
            pond_infra::sqlite_context::SqliteContextRepository::new(
                db.system.clone(),
                redactor.clone(),
            )
            // A ContextItem cannot exist un-redacted (one constructor, and it
            // takes the redactor), so mirroring its vector is safe by
            // construction -- no decorator ordering to get wrong here.
            .with_vector_index(vector_index.clone(), vector_model_id.clone()),
        );
        let pipeline = Arc::new(
            pond_core::context::ingest::IngestPipeline::new(context_repo.clone(), redactor.clone())
                .with_embedder(embedding_provider.clone()),
        );

        // PAI-8 P2's read surface. Installed unconditionally, like the other
        // `init_*_deps`: registration is what the toggle gates, so with the
        // extension unregistered these handles are simply unused.
        // Unified retrieval (phase C) behind the `recall` tool. Absent when no
        // embedder is wired, in which case `recall` answers nothing rather than
        // quietly degrading to context-only results.
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

        // PAI-8's first connector, on a timer.
        //
        // Deferred like the index sweep and for the same reason: a household's
        // first turn after a restart must not wait while the pond talks to a
        // calendar server. The interval is deliberately unhurried -- a calendar
        // changes a few times a week, the ctag check makes an unchanged sync
        // nearly free, and anything faster is load on somebody else's server
        // for no new information.
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
                        // Silent when nothing is connected, which is every pond
                        // until somebody connects something. A half-hourly line
                        // saying "nothing" is how a log stops being read.
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
                // A settings read that fails skips the event rather than
                // falling back to `Settings::default()`. The default is off, so
                // both mean "do not ingest" today -- but a default is a value
                // somebody can change, and a gate that launders an unreadable
                // store through it would start storing the day that moved.
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

    // Egress tracker (#113): record every outbound HTTP call made by built-in
    // MCP tools into the same event store, so network egress is queryable via
    // `GET /api/v1/activity?category=network`.
    pond_mcp_server::set_egress_sink(event_log.clone());

    // Sensor/event-triggered rules engine (#92): fire SensorTrigger schedules
    // when a matching sensor/camera/device event arrives on the bus. Fires go
    // through the scheduler's own run_now path (run records + result events).
    if let Some(sched) = scheduler.clone() {
        tokio::spawn(pond_infra_scheduler::run_rules_engine(
            event_bus.subscribe(),
            sched,
        ));
    }

    // The other half of the event spine (PAI-7 P1 and P2): the pond's own
    // clock, whether somebody is at it, and which household member that is.
    // Spawned here, beside the two consumers, so the whole spine reads in one
    // place. Both hold the bus weakly — see `run_time_ticker` — and both
    // publish only; the rules engine skips these events because they have no
    // device-shaped view, and the bridge above records them in the event log
    // like any other bus traffic.
    tokio::spawn(run_time_ticker(Arc::downgrade(&event_bus)));
    tokio::spawn(run_session_activity_observer(
        Arc::downgrade(&event_bus),
        session_storage.clone(),
        profile_repo.clone(),
        last_user_activity.clone(),
    ));

    // Vision pipeline (#130): camera frames → on-device motion detection →
    // camera_events + EventBus, so #92 rules and the activity feed react to
    // what the camera sees. Opt-in (`vision_enabled` + a camera URL) because
    // it needs a camera and ffmpeg on the device.
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
            // Save the triggering frame per event (bounded per-camera
            // retention) so the dashboard can show WHAT moved (#175 follow-up).
            snapshots: Some(pond_adapters_vision::SnapshotConfig::new(
                data_dir.join("snapshots"),
            )),
            ..Default::default()
        };

        // Optional ONNX classifier: upgrades "motion" into person/pet/package
        // on `vision-onnx` builds. An empty `vision_classifier_model` means
        // "the default YOLOX-Nano", auto-downloaded on first run exactly like
        // whisper/piper/face models; an explicit value points at an
        // operator-managed file (no auto-download). Any failure degrades to
        // unlabelled motion — never blocks the pipeline.
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

    // DB-backed handshake/pairing (#93). Construct before `db` is moved into
    // AppState, then issue a fresh pairing code the operator reads off the CLI
    // to pair a GOTG device.
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
        // Before `db`, and that is not cosmetic: struct-literal fields evaluate
        // in source order and `db` is moved by the next line.
        suggestion_queue: Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
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
        // The SAME handle the memory and context repositories write through and
        // the maintenance sweep repairs, cloned rather than constructed again.
        // A second construction here would be a second place to keep in step:
        // the day this handle is wrapped in a decorator -- as the memory repo
        // already is, twice -- the health route would be reporting on something
        // the writers had stopped using, and would say so in a number that
        // looked entirely plausible.
        vector_index: Some(vector_index.clone()),
        // Some only when the sweep above actually spawned. Handing the route a
        // notify with nothing listening would have it answer `refilling: true`
        // on a pond where nothing is going to refill, which is a lie that reads
        // as success -- the caller waits for a rebuild that never happens.
        index_reindex: index_reindex_handle,
        lane: Some(inference_lane.clone()),
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
        // Long-lived per-device notification streams get their own, larger pool
        // so connected phones never starve interactive chat SSE (#99 audit).
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(32)),
        answer_reviewer: answer_reviewer_for_http,
        extraction_status: Some(extraction_status),
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

                    // PAI-2 P6a: gate before the token leaves the machine. This
                    // is a background loop, so a refusal skips THIS provider on
                    // THIS tick and the loop keeps running -- killing the worker
                    // would mean tightening network_mode once permanently
                    // disabled refresh, even after the user loosened it again.
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

    // Advertise _pond._tcp.local. so phones on the LAN can discover this hub.
    // The handle is kept alive for the duration of the server; dropping it
    // deregisters the service gracefully.
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

    // The web UI is normally embedded into this binary (single executable). We
    // only fall back to `static_dir` when the binary was built without the UI,
    // so a missing dir is only worth warning about in that case.
    if !pond_api::web_ui_embedded() && !static_dir.exists() {
        tracing::warn!(
            "No web UI embedded and static dir {:?} not found — the dashboard will \
             not be served. Build the UI (`cd pond-desktop && npm run build`) before \
             building the server to embed it, or pass an existing --static-dir.",
            static_dir
        );
    }

    // Build router
    // Precompile the static prompt prefix before the first message arrives:
    // the model load and the multi-thousand-token preamble prefill move to
    // boot, and turn 1 hits the engine's ReusePrefix path. Progress is
    // mirrored into `state.warmup` for GET /api/v1/warmup (the UI's boot
    // banner); the settings handler re-runs this on a provider/model change.
    pond_api::spawn_prefix_prewarm(state.clone(), false);

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

    // --native: spawn the desktop app after the server is ready (macOS only).
    if native {
        spawn_desktop_app(api_port).await;
    }

    // Serve until a shutdown signal. We race the server against the signal
    // rather than using graceful shutdown so long-lived SSE streams (chat,
    // notifications) can't hold shutdown open indefinitely.
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

    // Stop the matter-server GIAP started, if any. kill_on_drop does not fire on
    // the signal path (the process exits without unwinding), so the runtime is
    // asked to kill it here — otherwise the controller orphans and survives the
    // Pond, including under `systemctl stop`.
    if let Some(matter) = &matter_runtime {
        matter.shutdown().await;
    }

    Ok(())
}

/// Resolves when the process is asked to stop: Ctrl-C on any platform, plus
/// SIGTERM on Unix (what `systemctl stop` and `docker stop` send). Used to end
/// serving so shutdown cleanup — notably killing the matter-server child — runs
/// instead of the process being torn down mid-flight.
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
            // If we can't install the handler, never resolve on this arm.
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
            // Indent each line to match the banner style.
            for line in image.lines() {
                println!("  {line}");
            }
        }
        Err(e) => tracing::warn!("QR code generation failed: {e}"),
    }
}

/// Locate and spawn the pond-desktop app.
///
/// macOS only: the desktop shell is an Electron app and is not packaged for
/// Linux. On the Jetson the UI is the dashboard this very server already
/// serves over HTTP, so there is nothing to spawn and nothing missing.
///
/// Search order:
///   1. `$GIAP_DESKTOP_BIN`                                    — explicit override
///   2. `/Applications/Goose In A Pond.app/...`                — installed
///   3. `pond-desktop/release/mac-*/Goose In A Pond.app/...`   — local package
///   4. `pond-desktop/node_modules/.bin/electron`              — dev, unpackaged
///
/// Note there is no debug/release pair to confuse any more. The Tauri version
/// probed a debug build FIRST, so a stale `cargo build` artifact silently took
/// precedence over the release one -- a trap documented in four places, and
/// one that cannot occur here.
///
/// The child process is detached (not joined) so the server keeps running.
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

    // Unpackaged dev: run Electron against the repo checkout. It needs the
    // app directory as its argument, which a packaged bundle does not.
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
        // The shell reads this and MUST NOT spawn its own server; it attaches
        // to ours instead. Without it there are two pond-servers fighting for
        // one port, which presents as a blank window.
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

    // Spawning is not the same as appearing, and this is the gap that made
    // --native look broken: the app takes a single-instance lock, so if one is
    // already running the process we just started quits within a few hundred
    // milliseconds, in silence. Reporting "started" and returning left a server
    // with no window on it and a log that claimed success.
    //
    // try_wait also reaps the child, which is what stopped it becoming a zombie
    // under the server for as long as the server ran.
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

/// The desktop shell is macOS-only, so `--native` has nothing to launch here.
/// This is not a degraded mode: on Linux -- which in practice means the Jetson
/// -- the UI is the dashboard this server already serves.
#[cfg(not(target_os = "macos"))]
async fn spawn_desktop_app(server_port: u16) {
    tracing::warn!(
        "--native: the desktop shell is macOS-only. This server's dashboard is already \
         available at http://127.0.0.1:{server_port} and on the LAN."
    );
}

/// Write one `WorkflowEvent` to stdout as a single NDJSON line (JSON + `\n`,
/// flushed immediately). This is the single framing helper for the
/// `--json-events` contract; both the streaming event sink and the
/// `ready`/`error`/`exit` lifecycle emissions in `run_chat` route through it so
/// they can never drift into two inconsistently-framed families on the same
/// pipe. Broken-pipe/partial-write errors are ignored: a dead shell means the
/// child is being torn down anyway.
fn write_ndjson_line(ev: &pond_core::shared::domain::agent::WorkflowEvent) {
    use std::io::Write as _;
    if let Some(line) = ev.to_ndjson() {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

/// Build the speech detector the capture loops should use, or `None` to keep
/// the energy gate.
///
/// Diagnostics go to stderr rather than through `out!`: that macro is a no-op
/// under `--json-events`, which is the only mode the desktop shell uses, so a
/// warning printed with it would reach nobody in the case that matters most.
///
/// The composition root owns this choice because `pond-adapters-whisper` is in
/// CI's fast-crate set and must stay buildable without an ONNX Runtime; it
/// knows the trait and nothing else.
///
/// Every failure degrades to `None` rather than propagating — a missing model,
/// a dead download, an ONNX Runtime that will not load. A pond that cannot load
/// its VAD should be a pond with a worse VAD, not a deaf one, and the energy
/// gate it falls back to is the one that shipped for a year.
async fn build_speech_detector(
    vad_backend: &str,
    data_dir: &std::path::Path,
) -> Option<Box<dyn pond_voice::dsp::SpeechDetector + Send>> {
    if vad_backend.eq_ignore_ascii_case("rms") {
        // The escape hatch, for a board whose ONNX Runtime is broken.
        return None;
    }
    if !vad_backend.eq_ignore_ascii_case("silero") {
        // Validation rejects anything outside `VAD_BACKENDS` at the API, but
        // `apply_key` stores whatever is in the row verbatim, so a hand-edited
        // database can still land here. Say so rather than silently choosing.
        eprintln!("  Listen   unknown vad_backend \"{vad_backend}\" — using the energy gate.");
        return None;
    }

    // This DOES download, in front of `ready`, which is the opposite of what
    // the Kokoro engine does a few hundred lines below — and deliberately.
    // Kokoro's weights are 92 MB and `serve` has already fetched them, so the
    // voice child can refuse and report a diagnostic. These are 2 MB, and
    // `chat --voice` has to work as a standalone command with no server ever
    // having run: refusing here would mean the detector is only ever on for
    // people who happened to start the desktop first. The whisper model on the
    // same path is 142 MB and fetches here too, so on the run where this is
    // slow it is not what is making it slow.
    let path = model_download::ensure_silero_model(data_dir).await?;

    // Bounded, because a broken ONNX Runtime does not fail — it HANGS.
    // `load-dynamic` with no dylib to open blocks forever inside ort's init,
    // and an unbounded wait here is a permanently silent startup with nothing
    // in the log. Kokoro's engine load is guarded the same way, for the same
    // reason.
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
    // In `--json-events` mode, stdout carries NOTHING but NDJSON lines. All the
    // human-facing banners/prompts below route through `out!`, which no-ops when
    // json_events is set. Diagnostics still reach stderr via `eout!`.
    macro_rules! out {
        ($($arg:tt)*) => {
            if !json_events {
                println!($($arg)*);
            }
        };
    }

    // Error/diagnostic output. Always writes to stderr, so it survives
    // `--json-events` mode (which reserves stdout exclusively for NDJSON).
    macro_rules! eout {
        ($($arg:tt)*) => {
            eprintln!($($arg)*);
        };
    }

    out!(
        "\n  Goose in a Pond {} — voice\n",
        env!("CARGO_PKG_VERSION")
    );

    // Before anything opens an audio device or loads a model: one voice
    // session per device. Two sessions fight over the microphone and the
    // speaker, and the symptoms never name that as the cause — they look like
    // two assistants answering in different voices, or like a stream
    // configuration the device has suddenly stopped supporting.
    //
    // Held for the rest of this function; released when the process exits,
    // however it exits.
    let _voice_lock = match voice_lock::VoiceLock::acquire() {
        Ok(lock) => lock,
        Err(e) => {
            // Returned, not printed: `main` already renders the error to
            // stderr, and printing it here as well showed the user the same
            // paragraph twice. The NDJSON events are additive — the desktop
            // shell reads those, never stderr.
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
    // NOTE: `ensure_onnx_runtime()` used to be called here, before the database
    // even existed. It moved below the settings load for the reason given at
    // its new site. PAI-2 P6a.
    let db = Database::init(&data_dir).await?;

    // And the sensor store. `run_server` installs all three; this path
    // installed only two, so every voice session logged
    // "spawn_sensor_server called before init_sensor_deps" and then failed to
    // load giap-sensors — the extension was simply missing from voice, with
    // an error on the console saying so.
    pond_mcp_server::init_sensor_deps(
        Arc::new(SqliteSensorStorage::new(db.logs.clone())),
        Arc::new(SqliteDeviceRegistry::new(db.system.clone())),
        // No Matter runtime on this path, so no device to read live. The port's
        // default `state` bails, which is exactly the case the stored fallback
        // exists for -- and the reply says the reading is stored and how old it is
        // rather than passing it off as current.
        Arc::new(pond_infra::logging_device_control::LoggingDeviceControl::new()),
    );

    // And the personal-context read handles — the fourth verse of the same
    // song (audit #115/#157, vision #130, sensors above): `serve` installed
    // them and this path did not, so the first voice session that loaded
    // giap-context panicked with "init_context_deps() not called"
    // (2026-08-27) — and, before the spawn fns learned to degrade, took every
    // other builtin server down with it. No vector index or embedder here:
    // like the serve path without an embedding provider, `recall` answers
    // nothing rather than quietly degrading to context-only results.
    pond_mcp_server::context::init_context_deps(
        Arc::new(pond_infra::sqlite_context::SqliteContextRepository::new(
            db.system.clone(),
            Arc::new(pond_infra::rule_redactor::RuleRedactor::new()),
        )),
        None,
        None,
    );

    // Load settings and model registry early — drives provider, model, TTS, and wake word.
    // Falls back to Settings::default() when the DB has no rows yet (first run).
    let settings_repo_chat = SqliteSettingsRepository::new(db.system.clone());
    let settings = settings_repo_chat.get().await.unwrap_or_default();

    // PAI-2 P6a: install the egress gate on THIS path too. `set_network_mode`
    // had exactly one call site, inside `run_server`, and the mode is a
    // process-global that defaults to `Open` — so `network_mode = "offline"`
    // was a silent no-op for the whole of `pond chat`, which is also the
    // terminal voice loop. Every outbound call this process makes, gated or
    // not, was evaluated against a setting nobody had read.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(&settings.network_mode),
    );

    // Must run before any ONNX-dependent init (Piper TTS); without it
    // ORT_DYLIB_PATH is never set and Piper::new() hangs indefinitely. It runs
    // after the mode install above because it can download ~100 MB from
    // github.com, and a download cannot be gated by a setting read afterwards.
    ensure_onnx_runtime();

    // CLI args override settings; settings provide the defaults from the chat role.
    let settings_provider = settings.chat_provider.clone();
    let effective_provider: &str = provider.unwrap_or(&settings_provider);

    let settings_model = settings.chat_model.clone();
    let effective_model: &str = model.unwrap_or(&settings_model);

    // Apply the microphone privacy setting before anything can open a device.
    pond_core::models::domain::mic_gate::set_mic_enabled(settings.mic_enabled);

    // Single shared microphone owner (see `pond_audio`). Before this, the
    // wake-word detector, the VAD follow-up capture, and the "record until
    // silence" one-breath capture each opened their own `cpal` stream — live
    // simultaneously by design (the wake listener races the reply for
    // barge-in), which could race the very next turn's capture for the same
    // device. Every capture path in this session goes through this one
    // handle instead, so opens/closes are serialized on one owner thread.
    //
    // Sized to the wake-word detector's own history requirement —
    // `window_ms.max(lookback_ms) + post_trigger_ms` from
    // `KeywordDetectorConfig::default()` (~13.4s) — with headroom; see the
    // sizing rule on `pond_audio::spawn`.
    let (mic_handle, _mic_owner_join) = pond_audio::spawn(
        Box::new(pond_audio::CpalCapture::new()),
        pond_audio::CAPTURE_RATE_HZ,
        15_000,
        settings.mic_enabled,
    );

    // One resolution for both voice models, accepting every on-disk shape the
    // settings fields have carried. See `voice_models` for why there are three.
    let voice_models = voice_models::resolve_voice_models(
        &settings,
        &SqliteModelRepository::new(db.system.clone()),
        &data_dir,
    )
    .await;

    // Setup problems worth telling the UI about, held until after `ready`.
    // The NDJSON contract guarantees `ready` is the FIRST line and the desktop
    // keys session setup on it, so a diagnostic emitted during wiring would
    // both break the contract and arrive before there is a session to attach
    // it to. Flushed immediately after `ready` below.
    let deferred_diagnostics: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());

    // Resolve the TTS engine from the CLI flag, else from what actually
    // resolved. The old test was `active_tts_model.starts_with("piper")`, which
    // is never true for a catalog name like `en-lessac-medium` — so every
    // install fell through to text-only and said nothing about it.
    let effective_tts_owned: String;
    let effective_tts: &str = match tts {
        Some(t) => t,
        None => {
            // Kokoro is the engine. Nothing else is.
            //
            // This used to fall through to `active_tts_model`, which since the
            // engine swap holds a Kokoro VOICE name ("af_heart"). That matched
            // no arm below, so voice mode selected the catch-all and went
            // text-only — the session looked healthy over NDJSON and simply
            // never made a sound.
            effective_tts_owned = "kokoro".to_string();
            &effective_tts_owned
        }
    };

    // Resolve the whisper ggml model path for the in-process backend. Only
    // under --voice: outside it there is no microphone and no reason to make a
    // text session wait on a 142 MB download.
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

    // Auto-start llamafile only when the provider is explicitly "llamafile".
    // Other providers (ollama, local, gguf, openai, etc.) manage their own process or need no process.
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

    // `--session-id` replaces the historical hardcoded "default-session" so the
    // desktop shell can pass a per-session uuid; the UI then reads history via
    // GET /api/v1/sessions/{id}/messages. Defaults to "default-session" when
    // absent (backward compatible). ensure_session_title still derives a
    // readable sidebar title from the first user message on this session.
    let session_id = session_id_arg.unwrap_or("default-session").to_string();

    // ── Build repos for GooseAdapter (before db.system is consumed) ───────────────
    let settings_repo_arc: Arc<
        dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync,
    > = Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    // Chokepoint 1 again. This entry point hands `memory_repo` to
    // `build_goose_backend`, which registers `giap-memory` -- so a CLI or voice
    // session writes memories exactly like the server does, and leaving the
    // repo raw here would be a hole in the chokepoint that nothing warns about.
    //
    // The index side of that same chokepoint, with the model id deliberately
    // `None`. `db.vectors` exists on every entry point, but the embedder does
    // not: this path passes `None` for `embedding_provider` to
    // `build_goose_backend` below, so nothing here can say which model produced
    // a vector. `None` is the honest answer rather than a guess --
    // `SqliteMemoryRepository::mirror` reads it as "leave any attributed entry
    // alone and let the sweep own it", which is exactly right for a process that
    // cannot attribute. What the handle still buys on an embedder-less path is
    // `delete`: the `giap-memory` tool this backend registers can forget a
    // memory, and without the index the row went and its vector stayed behind as
    // an orphan until someone ran `prune_orphans`.
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

    // Reseed built-in prompt templates at startup with the latest Jinja2 general-purpose content.
    // Uses upsert (not insert_if_absent) so existing installs get the updated templates.
    // User-created templates (is_system = false) are never touched.
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

    // Wire weather so giap__get_current_weather MCP tool is available in voice mode.
    // Same gate as the primary wiring above: coordinates OR a location name (the
    // adapter geocodes the name), so an onboarded name-only config still works.
    // The same question the HTTP wiring asks above, through the same function.
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
    // When goose-agent is compiled in, GooseAdapter is used for all inference —
    // it selects provider/model internally via the settings DB.  CLI --provider
    // and --model flags are persisted to the DB first so GooseAdapter picks them up.
    //
    // Without the feature we fall back to MockAgent and wire a direct LlmProvider
    // via with_provider() later (identical to the old behaviour).
    #[cfg(feature = "goose-agent")]
    let agent: Arc<dyn Agent> = {
        // Persist CLI overrides so GooseAdapter reads the right provider + model.
        // `--provider mock` is a test/dev-only shortcut that routes to MockAgent and
        // never consults the DB provider, so we must NOT write "mock" into the user's
        // real settings — a later `serve` would read it back and break live chat.
        if (provider.is_some() || model.is_some()) && effective_provider != "mock" {
            let mut s = settings.clone();
            s.chat_provider = effective_provider.to_string();
            s.chat_model = effective_model.to_string();
            settings_repo_arc.update(&s).await.ok();
        }
        // `--provider mock` routes to the MockAgent backend so the loop runs
        // deterministically offline (no llamafile/network/models) — used by the
        // json-events spawn-binary contract test. Any other provider uses the
        // live GooseAdapter, which selects its own provider/model via the DB.
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
            Arc::new(tokio::sync::RwLock::new(None)), // mesh_provider — CLI chat doesn't build the mesh stack (server-only for now)
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
                out!(
                    "  Prompt:   custom ({})",
                    prompt_dir.join("system.md").display()
                );
                let name = pond_core::prompts::sanitize_field(&settings.assistant_name, 50);
                let user = pond_core::prompts::sanitize_field(&settings.user_name, 50);
                let persona =
                    pond_core::prompts::sanitize_field(&settings.assistant_personality, 200);
                let tz = pond_core::prompts::sanitize_field(&settings.timezone, 50);
                // Through the resolver, like `prompts.rs` already does. This
                // copy read the raw field, so the two prompt paths described
                // the same pond differently: one knew the time zone implied a
                // city and the other said nothing at all.
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

    let warm_agent = agent.clone();
    let mut chat_service = ChatService::new(agent, session_id.clone(), storage)
        .with_system_prompt(system_prompt)
        .with_thinking_tone(settings.voice_thinking_tone_enabled);
    if let Some(model_name) = model {
        chat_service = chat_service.with_model_name(model_name);
    }
    // Durable per-turn telemetry (TTFT, tok/s, context) for voice turns —
    // same turn_metrics table the REST path writes.
    match SqliteTelemetry::new(db.logs.clone()).await {
        Ok(telemetry) => chat_service = chat_service.with_telemetry(Arc::new(telemetry)),
        Err(e) => tracing::warn!("voice telemetry disabled (init failed): {e}"),
    }

    // ── NDJSON event sink (--json-events) ──────────────────────────────────────
    // Writes one serialized WorkflowEvent per line to stdout with immediate
    // flush. In this mode the run_loop's human-facing prints are suppressed
    // (stdout_diagnostics=false) so stdout carries NOTHING but JSON lines. The
    // The desktop shell parses these lines to drive its voice UI.
    if json_events {
        let sink: pond_core::shared::services::chat::WorkflowEventSink =
            Arc::new(|event: &pond_core::shared::domain::agent::WorkflowEvent| {
                write_ndjson_line(event);
            });
        chat_service = chat_service
            .with_event_sink(sink)
            .with_stdout_diagnostics(false);
    }

    // Live mic-level reporting for the voice-mode UI orb (wait + recording
    // states). Only meaningful under --json-events — the desktop shell is the
    // only consumer of this NDJSON contract.
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
    // When GooseAdapter is active it selects the provider internally via the DB.
    // This block runs only in builds without the goose-agent feature.
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
            // "llamafile" and any unrecognised value — use the llamafile process started above.
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
    // Build a shared in-process Whisper backend once per session. It powers
    // both the `VoiceInput` adapter and the wake-word detector — no separate
    // KWS subprocess needed any more.
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
                    // Whisper was explicitly requested but failed to load. Under
                    // --json-events, out! is a no-op, so a bare warning would leave
                    // the desktop shell with a deaf session and zero diagnostics.
                    // Surface it on stderr (eout!) AND as an NDJSON error event so
                    // the UI can tell voice input is unavailable before we degrade
                    // to stdin (which the shell holds open and never writes to).
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
                // whisper requested but no usable model path (download failed or
                // model not in catalog — already warned above via out!). Same
                // deaf-session hazard under --json-events: surface it.
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

    // Swap in the configured detector, if there is one and it loads. Done after
    // construction rather than passed to `new` because both the VoiceInput
    // adapter and the wake-word detector share this one instance, and the
    // choice is a setting rather than a property of the model file.
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
        // The detector captures audio from before it fired, so the wake word
        // is inside the command clip. Hand the transcriber the detector's own
        // resolved trigger list so it strips exactly what matched.
        backend.set_wake_words(detector.triggers());
        chat_service = chat_service.with_wake_word_detector(detector);
    };

    // ── Wire barge-in energy ──
    // `MicEnergy` reads live RMS off the same shared mic owner every other
    // capture path here uses. Without this, `ChatService::new`'s default
    // `NoEnergy` is inert and barge-in-by-speaking silently never fires — the
    // wake word was the only way to interrupt a reply.
    chat_service =
        chat_service.with_speech_energy(Arc::new(pond_audio::MicEnergy::new(&mic_handle)));

    // ── Wire TTS output ──
    // The text/print fallback (no piper): in --json-events mode this MUST NOT
    // write to stdout (the assistant text is already streamed as NDJSON `token`
    // events), so use SilentOutput. Otherwise PrintOutput echoes to stdout.
    let text_fallback = || -> Arc<dyn VoiceOutput> {
        if json_events {
            Arc::new(SilentOutput)
        } else {
            Arc::new(PrintOutput)
        }
    };
    // Same as text_fallback, but first surfaces WHY voice output is unavailable as
    // a non-fatal NDJSON error event. Under --json-events the SilentOutput fallback
    // makes a broken-TTS session protocol-indistinguishable from a working one
    // (state:speak + tokens stream while zero audio plays); this lets the UI tell
    // the user the response is text-only and why. Non-json runs get PrintOutput and
    // still see the answer, so only the event differs.
    let tts_unavailable = |reason: &str| -> Arc<dyn VoiceOutput> {
        if json_events {
            deferred_diagnostics.borrow_mut().push(format!(
                "voice output unavailable: {reason}; response is text-only"
            ));
        }
        text_fallback()
    };
    let voice_out: Arc<dyn VoiceOutput> = match effective_tts {
        // Kokoro — the same engine `serve` builds, wired here because voice
        // mode runs in THIS process, not through the HTTP server. Wiring one
        // and not the other is why voice mode stayed on Piper (and then on
        // nothing) while the Settings preview spoke correctly.
        "kokoro" => {
            // Deliberately does NOT download. `serve` ensures the engine; this
            // child only uses it.
            //
            // Fetching here would put a 92 MB download in front of `ready`,
            // and the desktop keys the whole voice session on `ready` being the
            // first line out. A missing engine is reported as a diagnostic the
            // UI can show, which is a far better failure than a session that
            // appears to hang at startup.
            let kdir = model_download::kokoro_dir(&data_dir);
            let espeak_data_dir = {
                let p = model_download::piper_espeak_data_path(&data_dir);
                p.exists().then_some(p)
            };
            let cfg = pond_adapters_kokoro::KokoroConfig {
                // Same resolution `serve` does — the voice child reads the same
                // settings row and must not load a tier that is silent here.
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
                            // The default voice is always fetched, so this
                            // degrades to a different voice, never to silence.
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
            // Reached whenever no piper voice resolved. The old code treated
            // this as a deliberate text-only choice and stayed quiet — but the
            // desktop never chooses, it just spawns the child, so this arm was
            // the whole "voice mode is silent and says nothing" symptom.
            // Report it; silence must never be indistinguishable from success.
            out!("  Speak    off — replies are printed");
            // Name the setting that is actually wrong. `other` is
            // `active_tts_model`, but the voice is chosen by `voice_tts_voice`,
            // so reporting on `other` alone told a user who had set a voice
            // that they had not configured one.
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

    // The console is deliberately near-silent from here on (tracing is pinned
    // to WARN for it), so point at the file that is not — every detail of the
    // session lands there and it is the first thing to ask for when something
    // goes wrong.
    out!(
        "  Log      {}",
        data_dir.join("logs").join("pond.log").display()
    );

    // ── Prefix warm-up + spoken readiness ─────────────────────────────────────
    // The voice child used to pay model load + preamble prefill on the FIRST
    // utterance, with the user already mid-sentence. Move that cost to session
    // start, say so aloud while it runs, and greet by name when the pond is
    // ready — the greeting doubles as the audible "you can speak now" signal.
    // Under --json-events every spoken line goes through `voice_out`, which is
    // SilentOutput when no TTS engine is up, so stdout stays pure NDJSON.
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
    // All models are loaded and every adapter is wired; announce readiness
    // before entering the wait loop. run_loop emits the terminal `exit` event
    // itself (stdin_eof on EOF, dismissed on hard exit); on an unexpected loop
    // error we emit `exit` with reason "error" below.
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

/// Newest `sessions.updated_at` across every session in `pond_system.db`.
///
/// This is the second of the consolidation scheduler's two activity sources.
/// The terminal voice loop runs in a **separate OS process** (`pond-server chat
/// --json-events`, spawned by the desktop shell) and so can never touch this
/// process's `AppState.last_user_activity` — but it does persist every turn
/// through `ChatService`, which bumps `sessions.updated_at` in the shared
/// system DB. Watching that column is therefore what lets a voice interaction
/// both hold consolidation off and interrupt a run already in flight.
///
/// Returns `None` when there are no sessions or the read fails; callers treat
/// that as "no observable out-of-process activity".
///
/// **Sessions the pond minted for itself do not count** (`human_activity`,
/// PAI-7 P1). Every `AgentPrompt` schedule fire creates a `sched-*` row and
/// bumps its `updated_at`, so without the filter a cron line running at 3am is
/// indistinguishable here from the user returning — which both opens the
/// never-on-startup gate on a pond nobody has touched and holds consolidation
/// off as if somebody were typing.
async fn newest_session_activity(
    storage: &dyn pond_core::user_data::ports::session_storage::SessionStorage,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let sessions = storage.list_sessions().await.ok()?;
    pond_core::shared::domain::session_activity::human_activity(&sessions).newest_activity
}

// ── PAI-7 P1: clock and session-activity publishers on the event bus ────────
//
// Both publish and nothing consumes: the rules engine now skips these events
// (they have no `trigger_view`) and the bus-to-event-log bridge records them.
// Deciding anything about them is PAI-7 P4's, and that separation is the point
// of the phase — an event nobody can trust is worse than no event.

/// Publish one [`BusEvent::Time`] per local hour boundary.
///
/// **Cadence is computed from the wall clock each time**, not by ticking a
/// fixed hour-long interval, so the tick lands on the hour rather than an hour
/// after whenever the pond happened to boot. A machine that slept through
/// three hours publishes one tick when it wakes rather than three: the tick is
/// a heartbeat, and a backlog of "it is now 2am" is noise.
///
/// **The bus is held weakly.** This is the one task on the spine with no input
/// stream of its own — the rules engine and the event-log bridge both end when
/// the bus's senders drop. A strong `Arc` here would keep the bus, and so
/// their streams, alive for exactly as long as this loop, which is what turns
/// a background timer into something that outlives the shutdown it should have
/// ended with.
async fn run_time_ticker(bus: std::sync::Weak<dyn pond_core::shared::ports::event_bus::EventBus>) {
    use chrono::Timelike;
    use pond_core::shared::domain::time_tick::{secs_to_next_hour_from, TimeBoundary, TimeTick};
    use pond_core::shared::ports::event_bus::BusEvent;

    loop {
        // The clock reading goes in whole. Splitting it into a minute and a
        // second here is what let the two swap unnoticed.
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

/// Publish [`BusEvent::Session`] transitions — a conversation started, the
/// pond went quiet, the user came back — and [`BusEvent::Presence`]
/// transitions, which say *which household member* that was.
///
/// Reuses the consolidation scheduler's activity model wholesale:
/// `saw_activity_since_start` for the never-at-startup guard,
/// `combined_idle_for` so an out-of-process voice turn counts, and
/// `INACTIVITY_THRESHOLD_SECS` as the threshold — for presence too, so the
/// bus cannot say a member is still here after it has already said the pond
/// went idle. One pond, one definition of "the user has gone quiet".
///
/// **One task, one read, two observers** (PAI-7 P2). A second polling loop
/// would read the same table on its own schedule and the two could disagree
/// about which conversations exist — which is the shape of defect
/// `human_activity` was written as one function to prevent.
///
/// **What this loop decides, honestly.** The seed, the machine-session filter,
/// the never-at-startup gate, the idle arithmetic, the presence baseline, the
/// freshness window, which rows are worth an identity read, what an unreadable
/// profile list means and what an unreadable identity means all live in
/// pond-core, where a mutation to any of them fails a test.
///
/// This paragraph used to open "this loop decides nothing", and that was false
/// in both halves — four of those decisions were expressed right here, and the
/// freshness window was one of them: `presence_window` was a field this
/// function filled in, so I could set it to twenty-four hours and `cargo check`
/// plus the entire `pond-core` suite stayed green. `PresenceInputs`'s fields
/// are private now and `for_poll` is the only way in, so this loop cannot name
/// a window at all.
///
/// **What genuinely remains here is unguarded, and it is not nothing.**
/// `POLL_SECS`, the two store reads, the `tracing` lines — and `idle_threshold`
/// for [`PollClock`], which is still bound at this call site from
/// `INACTIVITY_THRESHOLD_SECS`. That is the same gap on P1's session-lifecycle
/// side; it is left as it is because `PollClock`'s two `Instant` fields sit
/// next to each other, and a positional constructor to close it would trade a
/// visible constant for two arguments that swap silently. Nothing tests this
/// function: `crates/pond-server/tests` does not name it, and `ci.yml` runs
/// `cargo check` for this crate and never `cargo test`.
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

    // Baselines for the never-at-startup guard, captured before the first
    // poll so the activity clock's boot value can never pass for activity.
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

        // One read serves all three halves: which conversations exist, the
        // newest `updated_at` (the out-of-process activity source), and which
        // rows carry an attribution to look up.
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

        // ── Presence (PAI-7 P2) ──────────────────────────────────────────
        //
        // Which rows are worth a `get_session_identity` is pond-core's,
        // because it is derived from the observer's own refusals rather than
        // invented here: `attribution_candidates` drops the pond's own
        // conversations, the ones nobody has spoken in inside the window, and
        // the ones whose `sessions.profile_id` is NULL. It is read-avoidance
        // and not a gate — the observer re-applies origin and freshness to
        // whatever it is handed, through the same predicate — and it matters
        // because `list_sessions` has no LIMIT and no time bound, so this used
        // to be one point query per session the pond had EVER attributed,
        // every sixty seconds, forever, on a Jetson.
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

        // What an unreadable row means, and what an unreadable profile list
        // means, are both pond-core's answers. This loop supplies the two
        // `Result`s and logs them; it does not get to say what they imply.
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

/// PAI-7 P4: think about what has happened, in idle time, and propose.
///
/// `proactive_review.rs` decides everything of consequence and this is the
/// loop it was written for. What is decided *here* — and therefore what is
/// unguarded, because `ci.yml` runs `cargo check` for this crate and never
/// `cargo test` — is `POLL_SECS`, the store reads, and the failure direction
/// of each of them. Those directions are the part worth reading:
///
/// - **A settings or session read that fails skips the tick.** Nothing is
///   assumed about a pond that cannot be read.
/// - **A proposal count that fails reads as the cap.** An unreadable count
///   means no review, never an unlimited one.
/// - **A decision read that fails skips the tick**, and this one is the least
///   obvious. An empty ledger suppresses nothing, so treating the failure as
///   "no decisions" would re-propose exactly the things a member has already
///   said no to — the widening direction, reached by a plausible default.
/// - **No orchestrator means no review.** When Goose init fell back to the mock
///   agent there is nothing in the `OnceLock`, and the loop returns rather than
///   running with a second one it made itself.
///
/// The ring is DRAINED when a review starts, so a brief says what has happened
/// *since the last review* — which is what the brief claims when it is empty.
/// A run that then fails loses those events; the alternative is re-reviewing
/// the same evening forever, which is worse and much harder to notice.
/// What one composing pass did.
///
/// `considered` is the count of memories the model was actually shown, and it
/// is what tells a zero-yield pass apart from a pass with nothing to do — the
/// distinction the titling loop's empty match arm threw away for 554 passes.
struct ComposeReport {
    considered: usize,
    queued: usize,
    refused: usize,
    unparseable: bool,
}

/// One pass: pick notes nobody has been asked about, compose questions, queue
/// them.
///
/// Split out of the loop so the ordering below is readable in one screen, and
/// because every early return here is a path that still has to spend the lane's
/// interval budget — which it does, because the caller holds the slot guard
/// across this call.
async fn compose_suggestions(
    memories: &dyn pond_core::user_data::ports::memory_repository::MemoryRepository,
    queue: &dyn pond_core::user_data::ports::suggestion_queue::SuggestionQueueRepository,
    provider: Arc<
        tokio::sync::RwLock<Option<Arc<dyn pond_core::models::ports::provider::LlmProvider>>>,
    >,
    // The names the household goes by. A question that opens by addressing one
    // of them is the pond talking TO the household, and this card's contract is
    // that the question shown IS the message sent when it is tapped.
    subjects: &[String],
    scan: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<ComposeReport> {
    use pond_core::models::domain::message::ChatMessage;
    use pond_core::user_data::domain::profile::ProfileScope;
    use pond_core::user_data::services::suggestion_generation as gen;

    let empty = ComposeReport {
        considered: 0,
        queued: 0,
        refused: 0,
        unparseable: false,
    };

    // Subtract what is already queued FIRST. A pass that composed a question
    // the unique index then refused would spend the same inference and yield
    // nothing, and on a pond with a full queue that is every pass.
    let already: std::collections::HashSet<String> =
        queue.live_memory_ids().await?.into_iter().collect();

    // `Household` because this reads the whole store to choose from; the
    // suggestion carries its source memory's own `profile_id`, and the READ
    // path is what scopes it to an audience. Reading at `Owner` here would
    // silently stop composing anything from unattributed notes, which on this
    // pond is all of them.
    let mut pool: Vec<_> = memories
        .search_recent(&ProfileScope::Household, scan)
        .await?
        .into_iter()
        .filter(|m| !already.contains(&m.id))
        // A note too short to be about anything cannot carry a question, and
        // asking a model about it spends a slot to be told so.
        .filter(|m| m.content.trim().chars().count() >= 20)
        .collect();

    // WHICH twelve is most of the quality. `search_recent` hands them over
    // newest first; this puts habits and preferences in front of the trivia the
    // pond itself said in a conversation, and keeps recency inside each rank.
    // See `order_candidates`.
    gen::order_candidates(&mut pool);
    let candidates: Vec<_> = pool.into_iter().take(gen::MEMORIES_PER_PASS).collect();

    if candidates.is_empty() {
        return Ok(empty);
    }

    let Some(provider) = provider.read().await.clone() else {
        // Logged rather than silent, unlike the titling loop's own version of
        // this line, which is invisible at every level and cost an evening to
        // find.
        tracing::debug!("[suggestions] no language model is configured");
        return Ok(empty);
    };

    let user = gen::build_user_prompt(&candidates);
    let reply = provider
        .complete(gen::SYSTEM, vec![ChatMessage::user(user)])
        .await?;
    let outcome = gen::parse_response(&reply.content, &candidates, subjects, now);

    for refusal in &outcome.refused {
        tracing::debug!(
            reason = refusal.as_str(),
            "[suggestions] refused a candidate"
        );
    }

    let queued = queue.queue(&outcome.accepted).await?;
    Ok(ComposeReport {
        considered: candidates.len(),
        queued,
        refused: outcome.refused.len(),
        unparseable: outcome.unparseable,
    })
}

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
    lane: Arc<crate::inference_lane_runner::InferenceLane>,
    profiles: Arc<dyn pond_core::user_data::ports::profile::ProfileRepository>,
) {
    use pond_core::mcp::ports::notification::Notification;
    use pond_core::user_data::services::consolidation_schedule as sched;
    use pond_core::user_data::services::inference_lane::LaneJob;
    use pond_core::user_data::services::proactive_review as review;

    const POLL_SECS: u64 = 60;

    // See the block in `run_server`: this is the SAME orchestrator and the SAME
    // registry the `delegate` tool holds, because a second registry compiles and
    // then refuses every spawn.
    let Some(deps) = pond_mcp_server::installed_orchestrator_deps() else {
        tracing::info!(
            "proactive reviewer: no orchestrator was installed (mock agent?) — not starting"
        );
        return;
    };
    let orchestrator = deps.orchestrator();
    let authorities = deps.authorities();

    // Fallible rather than a lazy unwrap, and checked once at start rather than
    // per tick: the recipe is text through a `deny_unknown_fields` parser, and a
    // typo in it should stop the reviewer loudly instead of logging every minute.
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

    tracing::info!(
        "proactive reviewer active — off unless both `proactive_review_enabled` and \
         `ext_orchestrator_enabled` are set"
    );

    // Claimed HERE, after the two early returns above, because `claim` is what
    // makes the status route say a loop exists — and on a pond with no
    // orchestrator, or a role that does not parse, one does not. Claiming at
    // the call site would have given a household a button whose job had already
    // returned.
    //
    // The reviewer is on the lane, but its gate runs in two halves and the
    // order matters. `should_review` goes first because its three extra
    // refusals -- the orchestrator toggle, a run in flight, the daily cap on
    // interrupting a household -- are about whether there is anything worth
    // doing at all, and a job should not take the only inference slot in order
    // to discover it has nothing to do with it. `acquire` goes second, for the
    // machine itself and for the tie-break against the other six jobs.
    let wake = lane.claim(LaneJob::ProactiveReview);

    loop {
        let tick = crate::inference_lane_runner::wait_for_tick(
            std::time::Duration::from_secs(POLL_SECS),
            &wake,
        )
        .await;

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

        // Invariant 7's tidying pass. Correctness does not depend on it — every
        // read filters expiry in SQL — but the terminal status is what the
        // feedback ledger reads, so an unswept row teaches nothing.
        if let Err(e) = proposals.expire_due(now).await {
            tracing::debug!(error = %e, "proactive reviewer: expiry sweep failed");
        }

        // Invariant 4. `None` here is the whole household and an unidentified
        // speaker both answering "not addressable", and the answer is no review.
        //
        // It is TRACED rather than skipped in silence, and that is a repair
        // rather than a nicety: on a pond where nobody has been identified —
        // which is every pond until PAI-1's identification routes get a caller —
        // this is where the reviewer stops, on every tick, and the first probe
        // of this loop produced no output at all. A feature that is switched on
        // and says nothing is indistinguishable from one that is broken, and
        // this programme has spent whole phases on that distinction.
        // The roster, re-read per tick so adding a second member takes effect on
        // the next review rather than at the next restart -- and it MATTERS
        // which way that lands: a second member is what closes the sole-member
        // fallthrough, so a stale roster of one would keep addressing reviews
        // to the first member after somebody else moved in.
        //
        // A failed read is treated as NO members, which makes the fallthrough
        // unavailable and the tick a no-op. That is the narrowing direction: the
        // alternative is addressing a proposal to whoever a failed query last
        // returned.
        let members: Vec<String> = profiles
            .list()
            .await
            .map(|p| p.into_iter().map(|p| p.id).collect())
            .unwrap_or_default();

        // Deliberately NOT a `continue`. Every refusal between here and
        // `acquire` has to reach the lane as `enabled: false` rather than as an
        // early return, because a registered job that stops asking leaves a
        // stale registration behind -- and a stale registration keeps winning
        // tie-breaks it will not act on. "Nobody has been identified" is a
        // state a pond can sit in for months, so this is the one most likely to
        // do it.
        let audience = review::audience_for_review(&sessions, now, &members);
        if audience.is_none() {
            tracing::trace!(
                sessions = sessions.len(),
                "proactive reviewer: nobody to address — no member has been identified inside \
                 the audience window, so there is no review to run"
            );
        }

        let db_activity =
            pond_core::shared::domain::session_activity::human_activity(&sessions).newest_activity;
        let in_process_at = *last_user_activity.read().await;

        // A rolling 24 hours, not a calendar day: the cap is about how often
        // somebody is interrupted, and midnight is not a fact about that.
        let proposals_today = match &audience {
            Some(a) => proposals
                .count_made_since(a.profile_id(), now - chrono::Duration::days(1))
                .await
                .unwrap_or(review::MAX_PROPOSALS_PER_DAY),
            // No audience means no review either way, and the cap is per
            // member, so there is nothing to count. `wants` below is already
            // false; this value never reaches a decision.
            None => 0,
        };

        // The reviewer's own half of the gate: is a review WORTH running?
        // Neither of these is waivable, and that is why they are separate from
        // the timing rules the lane applies. A hand-asked tick drops only the
        // gates about whether now is a polite moment; the daily cap especially
        // must survive it, or the one limit a household has on being
        // interrupted becomes a suggestion.
        //
        // This runs BEFORE `acquire` because a job must not take the only
        // inference slot in order to discover it has nothing to do with it.
        let refusal = review::reviewer_refusal(
            // The loop awaits its own run, so two can never overlap here. The
            // input exists for a caller that does not have that property.
            false,
            proposals_today,
        );
        if let Some(reason) = refusal {
            tracing::trace!(
                reason = reason.as_str(),
                "proactive reviewer: skipping tick"
            );
        }

        // One `acquire`, reached on every tick. That is the invariant the other
        // six jobs keep by construction and the reason none of the refusals
        // above is an early return: `acquire` is what refreshes this job's
        // registration, and a job whose registration goes stale keeps the
        // longest apparent wait on the lane, wins every tie-break it is offered,
        // and -- since a losing tick now nudges the winner -- gets woken again
        // and again to decline. Registering as disabled is how a job says "not
        // me" without leaving that hole.
        //
        // `enabled` therefore carries four facts: the household asked for
        // proactive review; there is `delegate` machinery to run a child at all
        // (`ext_orchestrator_enabled` ships off, so folding it in here is what
        // keeps PAI-6's posture structural); somebody has been identified to
        // address; and the reviewer's own cap has room.
        //
        // The floor borrows `memory_consolidation_interval_hours` because that
        // is what this loop has always used -- worth revisiting, but not in the
        // change that moves it onto the lane.
        let cadence = crate::inference_lane_runner::Cadence::new(
            sched::interval_floor_from_hours(settings.memory_consolidation_interval_hours),
            idle_threshold,
            false,
        );
        let Some(slot) = lane
            .acquire(
                LaneJob::ProactiveReview,
                settings.proactive_review_enabled
                    && settings.ext_orchestrator_enabled
                    && audience.is_some()
                    && refusal.is_none(),
                cadence,
                tick.waives(),
                sched::saw_activity_since_start(
                    started_at,
                    in_process_at,
                    started_at_utc,
                    db_activity,
                ),
                sched::combined_idle_for(in_process_at, db_activity, now),
            )
            .await
        else {
            continue;
        };

        // Unreachable when the lane granted the slot -- `enabled` above is
        // false without an audience -- but written as a refusal rather than an
        // unwrap, because a panic in a background loop takes the pond with it.
        let Some(audience) = audience else {
            continue;
        };

        // PAI-7 P7. A failed read skips the tick rather than proceeding with an
        // empty ledger — see this function's docs for why that direction matters.
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

        // The review becomes its own parent turn for the length of the run. The
        // lease revokes on drop, so nothing can delegate from a review that has
        // ended — the same property a user's finished turn has.
        let cancel = tokio_util::sync::CancellationToken::new();
        let lease = authorities.publish(
            &session_id,
            review::review_authority(&audience, &session_id),
            cancel.clone(),
        );

        // Invariant 3's second half. On the Orin this is correctness rather than
        // politeness: the review is holding the only GPU the household's next
        // turn needs.
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
                        // A transient read failure reads as `None`, which must
                        // never be mistaken for activity.
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
        // An attempt spends the interval budget whether or not it produced
        // anything, for the reason the consolidation scheduler records: retrying
        // after the next idle window reintroduces the repeated-expensive-attempt
        // churn the gate exists to remove.
        //
        // Dropping the slot is what spends it now, and what writes the stamp to
        // `lane_job_runs`. The explicit drop is for the same reason every other
        // job has one: the remainder of this iteration persists proposals and
        // sends notifications, none of which needs the model, and holding the
        // only inference slot through that would block every other job for no
        // reason.
        drop(slot);

        let run = match run {
            Ok(run) => run,
            Err(e) => {
                tracing::warn!(error = %e, "proactive reviewer: the run did not start");
                continue;
            }
        };

        // `interpret_answer` reads `result_for_parent()`, so a cancelled or
        // turn-exhausted run yields nothing rather than partial opinions.
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

        // A run that produced words and yielded NOTHING is the failure this
        // feature is most likely to have, and at DEBUG it is invisible.
        //
        // Measured on the Orin 2026-08-12: the loop fired correctly, resolved
        // its audience, spawned a child that answered in 32 s, and every impulse
        // was refused because a 2B model wrote a `type` field that
        // `ReviewerImpulse` did not have. Zero proposals, one DEBUG line each,
        // and a GPU spent per interval for nothing. On a household pond nobody
        // would ever see the reason.
        //
        // That specific cause is FIXED -- `ReviewerImpulse` no longer carries
        // `deny_unknown_fields`, because nothing in it is a capability and the
        // attribute was refusing whole suggestions over a key that could not
        // reach a decision. This warning stays regardless: it was never about
        // that one schema, it is about the class. A reviewer that runs, costs a
        // model turn and yields nothing is a defect whatever the reason, and
        // DEBUG is where this one hid for two recorded runs.
        //
        // WARN, not INFO: refusing every impulse is a defect somewhere -- in the
        // prompt, in the schema, or in the model -- and it is never the intended
        // steady state. A run that legitimately has nothing to say returns an
        // empty array and lands in neither branch.
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
            // PAI-7 P5's first production caller, and PAI-1 P9's rung earning
            // its keep. `send_to_profile` resolves the member's paired devices
            // and reaches NOBODY when there are none — it cannot fall back to a
            // broadcast, because `TargetedDelivery` has no variant meaning "the
            // household".
            //
            // `action_required` rather than `alert`: a proposal wants a decision,
            // and P6's speech gate ships allowing `alert` only, so switching the
            // pond's voice on does not also make it read out its suggestions.
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

/// Run one consolidation pass in the configured mode, apply the accepted
/// actions, and broadcast progress events.
///
/// Shared by the inactivity scheduler and the manual
/// `POST /api/v1/memory/consolidate` endpoint (via `ConsolidationRunner`).
///
/// `mode` picks the cost/thoroughness tradeoff:
/// - `"single"` — one LLM call. The default, and the sane choice on a 3B
///   on-device model.
/// - anything else (`"adversarial"`) — the three-stage Proposer -> Adversary ->
///   Judge pipeline, cancellable between stages.
///
/// `batch_size` bounds how many memories reach the prompt, so a growing store
/// cannot blow the context window. Whatever does not fit is logged, not
/// silently dropped.
///
/// Both modes funnel their accepted actions through pond-core's
/// `apply_actions`, so the correction-safety guards live in exactly one place
/// and cannot drift between modes.
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

    // Bound the prompt. Oldest-first, because duplicates cluster in time and a
    // contiguous window is the ordering most likely to contain both halves of a
    // duplicate pair (see `select_batch`).
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

    // Bridge: mpsc -> broadcast so the consolidator writes to mpsc and the
    // SSE stream (if any) reads from broadcast.
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
            // Collected, not lazy: a borrowing iterator held across the
            // `apply_actions` await defeats the compiler's higher-ranked
            // lifetime inference for the whole spawned future.
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
                // Persist audit trail
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
// 1.24.2 is here because fastembed takes `ort`'s default features, which include
// `api-24`. The previous pin (1.22.0) does not expose that API level, and the
// resulting failure was first seen on the Jetson and written up as a Jetson
// problem; it was not. With this pin the Orin embeds normally.
//
// The cost, checked against GitHub Releases on 2026-08-16 rather than assumed:
//
//            osx-arm64   osx-x86_64   linux-x64   linux-aarch64
//   1.23.2      yes          yes          yes           yes
//   1.24.0      yes          NO           yes           NO
//   1.24.2      yes          NO           yes           yes
//
// So 1.23.2 was the last release carrying all four, and this pin gives up the
// Intel Mac: `ort_platform_tags()` builds `onnxruntime-osx-x86_64-1.24.2.tgz`,
// which 404s, and `ensure_onnx_runtime` treats that as non-fatal — the server
// starts with face recognition and embeddings silently absent. An Intel Mac
// needs a system ONNX Runtime (`brew install onnxruntime`) or an explicit
// `ORT_DYLIB_PATH`; both are checked before the download is attempted.
//
// Do NOT bump to 1.24.0: it drops linux-aarch64, which is the Jetson.
//
// Re-run the check before changing this — availability has moved in both
// directions across three releases, so it is not a property to reason about:
//   for a in osx-arm64 osx-x86_64 linux-x64 linux-aarch64; do
//     curl -sIL -o /dev/null -w "$a %{http_code}\n" \
//       "https://github.com/microsoft/onnxruntime/releases/download/vX/onnxruntime-$a-X.tgz"
//   done
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

    // stderr, not stdout: `chat --json-events` gives stdout to the NDJSON
    // contract, and a download banner there corrupts the stream the desktop
    // parses. model_download sends its progress to stderr for the same reason.
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
    // PAI-2 P6a. This is a ~100 MB fetch from github.com and it was invisible
    // to `egress_guard.rs`, which finds senders by looking for `reqwest`. A
    // subprocess is a sender too. It is the only one in the tree
    // (`Command::new("curl")` matches here and nowhere else), so the hole was
    // one call, but it was the largest single outbound transfer the pond makes.
    //
    // `check_egress` and not `EgressCall::begin`: this fn is synchronous, and
    // `EgressCall::finish` -> `record_egress` reaches `tokio::spawn`, which
    // panics with no runtime on the thread. All three callers happen to be
    // inside an async fn today, but nothing in the signature says so, and
    // panicking inside a privacy check is the failure this module's own
    // `append_event` doc warns about. A refusal is still recorded -- that path
    // is runtime-safe -- so the audit trail keeps the half that matters.
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

    // A self-contained mic owner for this one-shot calibration run — it never
    // runs concurrently with the wake-word detector, so it does not share a
    // handle with `run_chat`/`run_server`.
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

// ── Private mesh (#132 Milestone 2) ───────────────────────────────────────────

/// Build the real `MeshTransport` when `settings.mesh_enabled` and this
/// binary was compiled with the `mesh` feature. The mesh identity keypair is
/// generated once and persisted via the raw settings key-value store (not a
/// `Settings` field — it's an internal secret) so peers stay pinned to the
/// same `PeerId` across restarts.
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

    // An OS-assigned ephemeral port (`tcp/0`) meant a fresh, unpredictable
    // port on every restart — any peer holding an older invite/address then
    // gets a real "connection refused" the moment this Pond restarts, with
    // no way to tell that's what happened. Derived from the identity secret
    // (already persisted, one value per install) rather than stored
    // separately: stable across restarts for free, and naturally different
    // per machine/instance since each generates its own secret.
    let listen_port = 40000 + (u16::from_be_bytes([secret_bytes[0], secret_bytes[1]]) % 10000);
    tracing::info!("mesh listening on a stable, identity-derived port: {listen_port}");

    // Harness/model attestation isn't wired up yet (ties to reproducible
    // builds — explicitly out of scope for this milestone per the issue's
    // risk list); a fixed placeholder lets any two Milestone-2 Ponds pair
    // during development. Replace once harness/model pinning lands.
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

/// Connects to Breez/Spark for mesh-peer Lightning settlement (#132
/// Milestone 5), when `settings.lightning_enabled` and this binary was
/// compiled with the `lightning` feature. Mirrors `build_mesh_transport`'s
/// shape and generate-once-and-persist discipline: the wallet mnemonic is
/// read from (or, the first time, generated and written to) the raw
/// settings key-value store under `lightning_wallet_mnemonic` — same
/// mechanism as `mesh_identity_secret`, not a new secret-storage path.
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

/// Reads through `AppState.llm_provider`'s own hot-swap lock on every call,
/// so the mesh responder (which needs a fixed `Arc<dyn LlmProvider>` at
/// construction — see `pond_adapters_mesh_inference`) automatically serves
/// with whatever provider is *currently* active on this Pond, not a stale
/// snapshot from whenever the mesh service was built.
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

/// Builds the mesh `LlmProvider` (and its capability-query / invoice-request
/// handles) exactly once at startup, only when `mesh_transport` is `Some` —
/// constructing more than one `MeshInferenceService` would spawn a second
/// consumer of the transport's single `recv()` queue (see the crate's own
/// docs). `build_provider` / `build_one`'s `"mesh"` match arms hand back a
/// cheap handle into this same singleton; they never construct a new one.
/// All three return values are handles into the *same* service —
/// `PeerCapabilityQuery` and `InvoiceRequester` are both implemented
/// directly on `MeshInferenceService` alongside `LlmProvider` support via
/// `.provider()`.
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
        // Reset on every chunk received, not an overall stream deadline — so
        // this bounds the GAP between chunks, not the total reply length.
        // 30s was tuned for a fast LAN peer; a lender on modest hardware
        // (observed as low as ~1.7 tokens/sec over a real mesh connection)
        // can leave a longer gap between chunks on a long generation (e.g. a
        // requested short story) without actually being stuck, and 30s
        // turned that into a false "no reply from peer" failure.
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

/// Spawns the periodic Lightning settlement job (#132 Milestone 6): once per
/// interval, pays down each trusted peer's pending usage tally via
/// `SettlementService`, at the fixed `MESH_SETTLEMENT_MILLISATS_PER_TOKEN`
/// rate. Always spawned — no `#[cfg(feature = "mesh")]` split needed, since
/// it only touches `pond-core` port traits, always compiled — safe to,
/// because the loop checks `payment_rail`/`invoice_requester` on every tick
/// and simply does nothing until both are real (i.e. mesh + lightning are
/// enabled on this Pond).
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
    device_control: Arc<
        dyn pond_core::user_data::ports::device_control::DeviceControlPort + Send + Sync,
    >,
    session_storage: Option<Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage>>,
    // Model catalog, so `GooseAdapter` can reach `ModelRecord.context_length`
    // — rung 3 of the context governor. Optional only so a caller with no
    // catalog to hand still compiles; every caller in this file supplies one,
    // because without it an Ollama model's window is guessed from its name.
    model_repo: Option<Arc<dyn ModelRepository>>,
    voice_mode: bool,
    // Wrapped in a lock (not a fixed value) so `PUT /api/v1/settings`
    // enabling mesh at runtime is visible on the very next chat turn — see
    // AppState::mesh_rebuild's own docs. CLI callers with no mesh stack pass
    // a lock that is permanently `None` (equivalent to the old `None` here).
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
    // The specialist is an in-process GGUF engine, so it only exists when the
    // `local-inference` feature is compiled in. In lean builds (e.g. the Jetson
    // single-executable without in-process GGUF) there is no specialist and the
    // main LLM handles all tool calling natively via MCP.
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

    // Register all GIAP MCP servers into Goose's builtin extension registry.
    // Extension toggles (ext_*_enabled) are read from settings to gate registration.
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

    // Build the adapter with all repos injected.
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
            // Session storage powers the deterministic turn trimmer's
            // rolling-summary splice (hybrid compaction).
            let adapter = match session_storage {
                Some(storage) => adapter.with_giap_session_storage(storage),
                None => adapter,
            };
            // Semantic memory injection: without this the per-turn retrieval
            // falls back to the keyword LIKE search.
            let adapter = match embedding_provider {
                Some(provider) => adapter.with_embedding_provider(provider),
                None => adapter,
            };
            // The model catalog, which is the only rung of the context governor
            // that can answer for an Ollama model. Without it the adapter falls
            // back to a substring match on the model's name.
            let adapter = match model_repo {
                Some(repo) => adapter.with_model_repo(repo),
                None => adapter,
            };
            // Private mesh (#132): lets chat_provider="mesh" route through a
            // trusted peer's compute. GooseAdapter reads this lock live on
            // every turn, so it stays empty until the mesh transport is
            // actually running — the "mesh" arm then warns and keeps
            // whatever provider was already active rather than failing the
            // turn — and starts serving the moment mesh_rebuild fills it,
            // with no adapter rebuild required.
            let adapter = adapter.with_mesh_provider(mesh_provider);
            if voice_mode {
                adapter.set_voice_mode(true);
            }
            let ext_mgr: Arc<dyn ExtensionManagerPort> = adapter.extension_manager();
            tracing::info!("Goose agent active — GIAP MCP extension registered");
            let adapter = Arc::new(adapter);
            // Phase D2 escape hatch: giap-toolkit's tools reach back into the
            // adapter that owns the per-session tool selection. Installed here
            // rather than in register_giap_extensions because the adapter is the
            // implementor and did not exist at registration time.
            pond_mcp_server::init_toolkit_deps(Some(adapter.clone()
                as Arc<
                    dyn pond_core::mcp::ports::tools::tool_selection_control::ToolSelectionControl,
                >));
            // PAI-6 P5: the `delegate` tool's handles. Installed here for the
            // same ordering reason as the toolkit's, and with one detail that
            // decides whether the feature works at all: the registry passed
            // here must be `adapter.turn_authorities()`, the SAME map the
            // adapter publishes each turn's authority into. A freshly
            // constructed registry compiles, and then answers `None` to every
            // lookup — which the tool correctly reads as "no live turn" and
            // refuses, so the failure would look like a working guard rather
            // than like broken wiring.
            //
            // Installed unconditionally: the toggle
            // gates REGISTRATION (in `register_giap_extensions`), so with it off
            // no server is ever spawned and these handles are simply unused.
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

/// Seed the persistent model catalog from upstream sources (static list + local Ollama).
///
/// Upserts all returned records (preserving `is_custom` rows) and sets `downloaded`
/// by checking the filesystem.  A failure to fetch is non-fatal — the server starts
/// with whatever models are already in the DB.
/// Give the speech engine a voice on first run, so the pond can talk out of
/// the box.
///
/// Everything else about TTS bootstraps itself — `ensure_kokoro_engine` fetches
/// the tokenizer, the weights and the default voice, and the 54 voice rows come
/// from a static list that seeds even with no network. The one thing that did
/// not was the *assignment*: a fresh install had a working engine, a downloaded
/// voice, and `SPEAKING — Nothing assigned` on the Models page, because
/// choosing the voice was left to the household.
///
/// Only ever fills a hole. An existing assignment is never touched, so this
/// cannot overwrite a voice someone picked.
async fn ensure_tts_is_set_up(
    repo: &dyn ModelRepository,
    settings_repo: &dyn pond_core::user_data::ports::settings::SettingsRepository,
) {
    let assignments = repo.list_assignments().await.unwrap_or_default();
    if assignments.iter().any(|a| a.role == "tts") {
        return;
    }

    // Prefer whatever the household already has in settings — an upgrade from
    // before roles existed carries a voice there — and fall back to Kokoro's
    // own reference voice.
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

/// Give this machine the speech tier it can actually keep up with.
///
/// Deliberately NOT part of `ensure_tts_is_set_up`, which is where it used to
/// live and where it did nothing. That function returns early when a TTS
/// assignment already exists — a question about the *voice* — and the tier
/// decision sat after the return, so every pond that had ever assigned a voice
/// skipped it. That is every upgraded pond, including the Jetson the
/// measurements were taken on: it kept `q8` and synthesised at **RTF 1.335**,
/// slower than playback, while `q4f16` runs the same sentence at 0.780. The
/// code was right and unreachable, which is the worst of both.
///
/// So it is its own step, run on every start, and idempotent by construction —
/// `tier_to_adopt` returns `None` once the stored tier is the host default or
/// anything the household picked. pond-core cannot make this call: it is pure
/// domain and must not sniff the machine, so the composition root does it.
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
                // A `tts_piper` assignment is now always stale, and syncing it
                // is actively harmful. Piper is gone as an engine; what this
                // branch writes into `voice_tts_voice` is a `.onnx` FILENAME,
                // and Kokoro takes a voice NAME, so the value can only ever be
                // one the engine rejects.
                //
                // Measured on the Orin, which still carries
                // `tts|tts_piper/en-lessac-medium` from before the swap: this
                // sync wrote `en_US-lessac-medium.onnx` about four seconds into
                // every boot, the Kokoro bootstrap noticed it was not a Kokoro
                // id and healed it back to `af_heart` about eighty seconds
                // later, and the next boot did it again. The heal's own comment
                // says it "makes this a one-time event" — it could not, because
                // it repaired the setting while this repaired the setting back
                // from an assignment nobody had migrated.
                //
                // Skipped rather than migrated here: this function's job is to
                // mirror assignments into settings, not to decide what the
                // household's voice should be. Leaving the row alone and
                // declining to mirror it lets the Kokoro bootstrap establish
                // the truth once, and it stays.
                if category == "tts_piper" {
                    tracing::info!(
                        model = %model_name,
                        "ignoring a Piper TTS assignment left over from before the \
                         engine swap; Kokoro will choose the voice"
                    );
                    continue;
                }
                // The TTS engine gate elsewhere checks active_tts_model.starts_with("piper")
                // — a bare catalog slug (e.g. "en-lessac-medium") never satisfies that, so
                // it must be stored prefixed for piper voices.
                //
                // Idempotent, because this value round-trips: it is written to
                // `active_tts_model` here and read back into a role assignment
                // elsewhere, so a bare `format!` compounds a prefix once per
                // settings-write/boot cycle — `piper-piper-en-lessac-medium`,
                // then `piper-piper-piper-...`. The gate that reads it only
                // checks `starts_with("piper")`, so nothing fails loudly; the
                // voice filename in the same block just stops matching a real
                // model, and TTS goes quiet for a reason nobody can see.
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

    // PAI-2 P6a follow-up: `pond models download` calls
    // `model_download::download_file` twice and was the FOURTH downloading
    // entry point, not the third. P6a gated the download and installed the mode
    // on serve/chat/setup, but never here — so the gate it added was inert on
    // this path and a stored `network_mode = "offline"` permitted a full model
    // download. That is a privacy control failing OPEN, which is the polarity
    // invariant 3 forbids. The guard's detector now looks for functions that
    // DOWNLOAD rather than functions that call `ensure_onnx_runtime()`, which
    // is what let the omission through.
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
            AgentStreamEvent::TurnLimitReached { max_turns } => {
                // The cap sentence itself already printed as Text. In a REPL the
                // continuation is just the next prompt, so say how to give it.
                eprintln!(
                    "\r\x1b[K\x1b[2m  (turn budget of {max_turns} reached — \
                     send \"continue\" to resume)\x1b[0m"
                );
            }
            // PAI-6 P6. On stderr with the other progress chatter, so piping
            // stdout still yields exactly the assistant's answer. `detail` is a
            // tool name or a GIAP-authored reason and is printed as it arrives;
            // it never carries the child's own text.
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
    // Chokepoint 1 again, for the same reason as the voice path: all three
    // arms below reach `build_goose_backend`, so all three can write a memory.
    //
    // Index handle with no model id, for the reason spelled out at the same
    // wiring in `run_chat`: every arm calls `build_goose_backend` with
    // `embedding_provider: None`, so this process can delete an indexed memory
    // through `giap-memory` but can never attribute a vector it wrote.
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
    // The catalog the context governor's rung 3 reads. The CLI paths get one
    // too: a `pond-server chat` turn budgets its history exactly the way a
    // dashboard turn does, and giving only the server the real window would put
    // the two back out of agreement — which is the whole defect PAI-3 removes.
    let cli_model_repo: Arc<dyn ModelRepository + Send + Sync> =
        Arc::new(SqliteModelRepository::new(db.system.clone()));

    let settings = settings_repo.get().await.unwrap_or_default();
    // PAI-2 P6b: install the egress gate on THIS entry point too. `pond agent`
    // has read the settings row since it was written, and ignored the one field
    // on it that says whether the pond is allowed to talk to anybody. All three
    // arms below reach `build_goose_backend`, which wires the LLM provider, the
    // weather adapter and the whole MCP tool surface -- everything that phones
    // out on a turn. Without this the process-global stays at its `Open`
    // default and every gate those paths inherit evaluates against a mode
    // nobody chose. P6a fixed the same defect for `run_chat` and `run_setup`
    // and did not reach here.
    pond_core::shared::services::egress::set_network_mode(
        pond_core::shared::services::egress::NetworkMode::parse(&settings.network_mode),
    );
    // Use the configured LLM server URL (llamafile default). GooseAdapter uses this to
    // route requests when chat_provider = "llamafile"; for ollama/local it uses its own logic.
    let llamafile_url = format!("http://127.0.0.1:{}", ports::llamafile_port());

    // Wire weather from settings so giap__get_current_weather MCP tool is available.
    // The third copy of this decision, and it was the strictest of the three:
    // it required COORDINATES, so a pond that had only ever been given a place
    // name got weather over HTTP and in voice mode, and was refused it here.
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
                Arc::new(tokio::sync::RwLock::new(None)), // mesh_provider — CLI paths don't build the mesh stack (server-only for now)
            )
            .await;

            let request = AgentRequest {
                message,
                session_id: session,
                model_role,
                images: Vec::new(),
                voice_mode: false,
                canvas_mode: false,
                // Local CLI on the device itself. Whoever ran it has shell
                // access to the pond already, so a narrower scope would be
                // theatre rather than a boundary.
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
                Arc::new(tokio::sync::RwLock::new(None)), // mesh_provider — CLI paths don't build the mesh stack (server-only for now)
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
                Arc::new(tokio::sync::RwLock::new(None)), // mesh_provider — CLI paths don't build the mesh stack (server-only for now)
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

            // Deliberately the same lookup the REST reset handler uses. This arm
            // used to carry its own `match`, so the two reset paths could ship
            // different factory text — and did, for the description.
            let Some((content, description)) = builtin_template_content(&name) else {
                eprintln!("'{name}' is not a built-in template. Only balanced | concise | technical | warm can be reset.");
                std::process::exit(1);
            };
            let t = PromptTemplate {
                name: name.clone(),
                content: content.to_string(),
                description: description.to_string(),
                is_system: true,
                // An explicit reset returns the row to factory ownership, which
                // includes adopting the current generation.
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
    // Chokepoint 1: `memories add` is a direct write path into the same store
    // the server writes to, and it takes its content straight from argv --
    // which is where a shell-history copy of a credential comes from.
    //
    // `memories remove` is the delete that most needs the index handle: it is
    // the one command whose entire job is to take a memory out, and unwired it
    // took the row and left the vector sitting in `pond_vectors.db`. No embedder
    // in this process either, so the model id is `None` -- the case `mirror`
    // documents by name, and the reason it returns rather than removing when a
    // fragment arrives carrying a vector it cannot attribute.
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

/// `pond pairing` — re-display (or refresh) the device pairing code + QR by asking
/// the RUNNING server (loopback) to mint/return it. Pairing-code plaintext is
/// process-local (`ISSUED_CODE_CACHE` lives in the server process), so the CLI must
/// NOT mint locally — a locally-minted code lands in a per-process cache the running
/// server can never see, so it can never verify (DEF-6). We instead delegate to the
/// server's loopback-gated `GET/POST /api/v1/handshake/pairing-code` endpoints, which
/// execute in the process that owns the cache.
async fn run_pairing(refresh: bool) -> Result<()> {
    let data_dir = default_data_dir();

    // The running server persisted its actually-bound port here (see run_server).
    // Fall back to the default only so the URL is still meaningful; a missing file
    // almost certainly means the server isn't running, which the HTTP call surfaces.
    let port = std::fs::read_to_string(data_dir.join(".runtime_api_port"))
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(ports::API_SERVER);

    let base = format!("http://127.0.0.1:{port}/api/v1/handshake/pairing-code");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    // GET returns the current unconsumed code; POST mints a fresh one. Both are
    // loopback-only and execute inside the server process (cache is populated there).
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

    // The session-activity seed's guards moved to pond-core with the code
    // (`shared/domain/session_activity.rs`), because CI only `cargo check`s
    // this crate — a test that lives here never runs in CI at all.
}
