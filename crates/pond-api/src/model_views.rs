//! How a catalogue row is shown over REST.

use std::collections::HashMap;
use std::path::PathBuf;

use pond_core::models::domain::curated;
use pond_core::models::domain::drafter::{drafter_for, drafter_path};
use pond_core::models::domain::engine::{Engine, EngineAvailability};
use pond_core::models::domain::model_layout;
use pond_core::models::domain::model_record::{ModelCategory, ModelRecord, ModelRoleAssignment};
use pond_core::models::domain::model_use::{HelperModel, ModelSection, ModelUse, RoleHolders};
use pond_core::models::domain::recommended::{self, DeviceClass};
use pond_core::models::domain::taxonomy::{self, Acquisition, ModelKind, Provenance};
use pond_core::models::domain::vision_encoder::{encoder_for_model, EncoderSpec, EncoderState};
use pond_core::models::domain::vision_pairing::gguf_file_name;
use pond_core::models::ports::agent::Agent;

use crate::{AppState, CompanionDto, MeasuredDto, ModelStatusEntry, RecommendedDto};

/// `m` as the catalogue shows it; `engines` decides whether this pond can run it, `holders`
/// which jobs it is doing.
pub(crate) fn record_to_dto(
    m: &ModelRecord,
    assignments: &[ModelRoleAssignment],
    holders: &RoleHolders,
    engines: EngineAvailability,
) -> ModelStatusEntry {
    let active = assignments.iter().any(|a| a.model_id == m.id);
    let pick = curated::for_record(m);
    let unavailable = engines.unavailable_for(&m.category);
    let roles = holders.uses_of(&m.id);
    ModelStatusEntry {
        section: ModelSection::of(m, &roles),
        roles,
        category: m.category.as_str().to_string(),
        name: m.name.clone(),
        description: m.description.clone(),
        size_mb: m.size_mb,
        downloaded: m.downloaded,
        active,
        url: m.url.clone(),
        hf_id: m.hf_id.clone(),
        filename: m.filename.clone(),
        ram_estimate_mb: m.ram_estimate_mb,
        recommended_role: m.recommended_role.clone(),
        context_length: m.context_length,
        asr_language: m.asr_language.clone(),
        asr_size: m.asr_size.clone(),
        tts_engine: m.tts_engine.clone(),
        config_filename: m.config_filename.clone(),
        // `list_models` fills these for GGUF rows: the agent's verdict may read a header.
        reads_images: None,
        image_support_bytes: None,
        id: m.id.clone(),
        title: taxonomy::title(m, pick.map(|p| p.title)),
        quantization: m
            .quantization
            .clone()
            .or_else(|| pick.and_then(|p| p.quantization.map(str::to_string))),
        engine: Engine::for_category(&m.category).map(Into::into),
        provenance: Some(Provenance::derive(m, pick.is_some())),
        kind: Some(ModelKind::of(m)),
        acquire: Some(Acquisition::of(m, pick.is_some())),
        recommended: recommendation(&m.id, DeviceClass::current()),
        companions: Vec::new(),
        available: unavailable.is_none(),
        unavailable_reason: unavailable,
    }
}

/// The name the household reads for `m`.
pub(crate) fn title_of(m: &ModelRecord) -> String {
    taxonomy::title(m, curated::for_record(m).map(|p| p.title))
}

/// The suggestion for `model_id`, carrying numbers only measured on `device`'s class.
pub(crate) fn recommendation(model_id: &str, device: DeviceClass) -> Option<RecommendedDto> {
    let r = recommended::recommendation_for(model_id)?;
    Some(RecommendedDto {
        rank: r.rank,
        reason: r.reason.to_string(),
        measured: r.measured_on(device).map(MeasuredDto::from),
    })
}

/// Where a llama.cpp model's file is: its row's file name, else its name as a file.
pub(crate) async fn gguf_path_for(state: &AppState, model: &str) -> Option<PathBuf> {
    let data_dir = state.data_dir.as_ref()?;
    let recorded = match &state.model_repo {
        Some(repo) => repo
            .get_by_id(&ModelRecord::id_for(&ModelCategory::Gguf, model))
            .await
            .ok()
            .flatten()
            .and_then(|r| r.filename),
        None => None,
    };
    let filename = recorded.unwrap_or_else(|| gguf_file_name(model));
    model_layout::path_for(data_dir, &ModelCategory::Gguf, &filename)
}

/// The picture add-on `model` pairs with, reading its header only when no name is listed.
pub(crate) async fn chat_model_encoder(state: &AppState, model: &str) -> Option<EncoderSpec> {
    let gguf = gguf_path_for(state, model).await;
    let model = model.to_string();
    tokio::task::spawn_blocking(move || encoder_for_model(&model, gguf.as_deref()))
        .await
        .ok()
        .flatten()
}

/// What one llama.cpp row says about pictures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GgufVision {
    pub reads_images: bool,
    pub image_support_bytes: Option<u64>,
    pub companion: Option<CompanionDto>,
}

/// Picture facts for `model`: the agent's verdict where it gives one, else the pairing alone. A
/// pure read: listing must never prepare or fetch.
pub(crate) fn gguf_vision(agent: &dyn Agent, model: &str, spec: Option<EncoderSpec>) -> GgufVision {
    let state = agent.vision_state("local", model);
    let reads_images = match &state {
        None | Some(EncoderState::Unknown) => spec.is_some(),
        Some(state) => !state.is_unsupported(),
    };
    GgufVision {
        reads_images,
        image_support_bytes: spec.filter(|_| reads_images).map(|s| s.size_bytes),
        companion: spec.map(|s| CompanionDto {
            kind: "pictures".to_string(),
            label: s.label.to_string(),
            size_bytes: s.size_bytes,
            state: companion_state(state.as_ref()).to_string(),
        }),
    }
}

/// The add-on's state on the wire, from picture support's own state. A file that has arrived
/// reads `verifying` until its hash is checked, then `installed`.
pub(crate) fn companion_state(state: Option<&EncoderState>) -> &'static str {
    match state {
        Some(EncoderState::Ready { .. }) => "installed",
        Some(EncoderState::NotOnThisDevice) => "not_on_this_device",
        Some(EncoderState::Verifying) => "verifying",
        Some(EncoderState::Downloading { .. }) => "downloading",
        _ => "available",
    }
}

/// [`gguf_vision`] for every llama.cpp row, keyed by row id, in one blocking-pool batch: a row
/// may read a header on every page visit.
pub(crate) async fn gguf_vision_batch(
    state: &AppState,
    records: &[ModelRecord],
) -> HashMap<String, GgufVision> {
    let rows: Vec<(String, String, Option<PathBuf>)> = records
        .iter()
        .filter(|m| m.category == ModelCategory::Gguf)
        .map(|m| {
            let file = m
                .filename
                .clone()
                .unwrap_or_else(|| gguf_file_name(&m.name));
            let path = state
                .data_dir
                .as_ref()
                .and_then(|dd| model_layout::path_for(dd, &ModelCategory::Gguf, &file));
            (m.id.clone(), m.name.clone(), path)
        })
        .collect();
    let agent = state.agent.clone();
    tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|(id, name, path)| {
                let spec = encoder_for_model(&name, path.as_deref());
                (id, gguf_vision(agent.as_ref(), &name, spec))
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// MB the in-use in-process chat model holds (with its add-on when attached), from its file.
/// Zero for a model another program runs, or when none is chosen.
pub(crate) async fn in_use_mb(state: &AppState) -> u64 {
    let Ok(settings) = state.settings_repo.get().await else {
        return 0;
    };
    let category = ModelCategory::for_chat_model(&settings.chat_provider, &settings.chat_model);
    let in_process = Engine::for_category(&category).is_some_and(Engine::in_process);
    if !in_process || settings.chat_model.trim().is_empty() {
        return 0;
    }
    let record = match &state.model_repo {
        Some(repo) => repo
            .get_by_id(&ModelRecord::id_for(&category, &settings.chat_model))
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let filename = record
        .as_ref()
        .and_then(|r| r.filename.clone())
        .unwrap_or_else(|| match category {
            ModelCategory::Gguf => gguf_file_name(&settings.chat_model),
            _ => settings.chat_model.clone(),
        });
    let on_disk = state
        .data_dir
        .as_ref()
        .and_then(|dd| model_layout::path_for(dd, &category, &filename))
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len() / 1_048_576);
    let weights = on_disk.or(record.map(|r| r.size_mb)).unwrap_or(0);
    let add_on = match state.agent.vision_state("local", &settings.chat_model) {
        Some(EncoderState::Ready { bytes: Some(b) }) if category == ModelCategory::Gguf => {
            b / 1_048_576
        }
        _ => 0,
    };
    weights + add_on
}

/// [`device_budget::reclaimable_mb`](pond_core::models::domain::device_budget::reclaimable_mb)
/// for this pond right now, against the budget the status was measured with; zero where memory
/// is managed by another program.
pub(crate) async fn reclaimable_mb(
    state: &AppState,
    status: &pond_core::models::ports::model_scheduler::MemoryStatus,
) -> u64 {
    if status.total_mb == 0 {
        return 0;
    }
    pond_core::models::domain::device_budget::reclaimable_mb(
        in_use_mb(state).await,
        status.budget_mb,
        status.available_for_llm_mb,
    )
}

/// Who holds each job among `records`, read from this pond's settings. Unreadable settings
/// hold nothing, said once in the log rather than guessed from the assignment table.
pub(crate) async fn role_holders(
    state: &AppState,
    records: &[ModelRecord],
    assignments: &[ModelRoleAssignment],
) -> RoleHolders {
    match state.settings_repo.get().await {
        Ok(settings) => RoleHolders::resolve(records, assignments, &settings),
        Err(e) => {
            tracing::warn!(
                kind = "model_roles_settings_unreadable",
                error = %e,
                "settings could not be read, so no model is shown doing a job"
            );
            RoleHolders::default()
        }
    }
}

/// Where the Memory model stands now: the host's MemAvailable, what the model running gives back
/// on a switch, and that model's vector id.
pub(crate) struct MemoryFooting {
    available_mb: Option<u64>,
    returns_mb: u64,
    running: Option<String>,
}

impl MemoryFooting {
    pub(crate) fn read(
        state: &AppState,
        status: &pond_core::models::ports::model_scheduler::MemoryStatus,
    ) -> Self {
        use pond_core::models::domain::{device_budget, embedding_models};
        use pond_core::models::services::active_embedder::EmbedderState;

        // Only a loaded model gives anything back; one still loading or failed holds nothing yet.
        let running = match state.embedder.state() {
            EmbedderState::Ready { model_id, .. } => Some(model_id),
            _ => None,
        };
        let returns_mb = running
            .as_deref()
            .and_then(embedding_models::find_by_model_id)
            .map_or(0, |m| {
                device_budget::embedder_returns_mb(m, device_budget::budgeted_device())
            });
        Self {
            available_mb: status.available_mb,
            returns_mb,
            running,
        }
    }

    /// [`device_budget::embedder_room_mb`](pond_core::models::domain::device_budget::embedder_room_mb);
    /// `None` where the host's memory was not read.
    pub(crate) fn room_mb(&self) -> Option<u64> {
        self.available_mb
            .map(|a| pond_core::models::domain::device_budget::embedder_room_mb(a, self.returns_mb))
    }

    /// Drops a Memory row's recommendation where this pond could not load the model now.
    pub(crate) fn weigh(&self, dto: &mut ModelStatusEntry) {
        use pond_core::models::domain::embedding_models;
        if dto.category != ModelCategory::Embedding.as_str() || dto.recommended.is_none() {
            return;
        }
        let Some(model) = embedding_models::find(&dto.name) else {
            return;
        };
        let stands = recommended::memory_recommendation_stands(
            model.ram_estimate_mb(),
            self.available_mb.map(|a| a + self.returns_mb),
            self.running.as_deref() == Some(model.model_id),
        );
        if !stands {
            dto.recommended = None;
        }
    }
}

/// What the pond knows, right now, about the models that work beside the catalogue rows.
#[derive(Debug, Default)]
pub(crate) struct HelperFacts {
    /// The conversation row's id, with the label of its picture encoder when that is installed.
    pub pictures: Option<(String, String)>,
    /// The conversation row's id, with the id of a drafter on disk that the engine attaches.
    pub drafter: Option<(String, &'static str)>,
    /// The camera detector's name, when one is loaded.
    pub camera: Option<String>,
    /// Face recognition's models are loaded.
    pub faces: bool,
}

/// The helpers `facts` describe, in section order.
pub(crate) fn helpers_from(facts: HelperFacts) -> Vec<HelperModel> {
    let mut out = Vec::new();
    if let Some((chat, label)) = facts.pictures {
        out.push(HelperModel::new(
            format!("{chat}#pictures"),
            Some(label),
            ModelUse::Pictures,
            Some(chat),
        ));
    }
    if let Some(name) = facts.camera {
        out.push(HelperModel::new(
            "camera_detector",
            Some(name),
            ModelUse::Camera,
            None,
        ));
    }
    if facts.faces {
        out.push(HelperModel::new(
            "face_recognition",
            None,
            ModelUse::Faces,
            None,
        ));
    }
    if let Some((chat, drafter)) = facts.drafter {
        out.push(HelperModel::new(
            drafter,
            None,
            ModelUse::Drafter,
            Some(chat),
        ));
    }
    out
}

/// The camera detector's name: the bundled YOLOX-Nano unless settings point at another file.
fn camera_detector_name(configured: &str) -> String {
    let configured = configured.trim();
    if configured.is_empty() {
        return "YOLOX-Nano".to_string();
    }
    std::path::Path::new(configured)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(configured)
        .to_string()
}

/// The models in use beside the catalogue rows, for `holders`' conversation model: its picture
/// encoder and drafter, the camera detector and face recognition. Each is listed only when it
/// is loaded or on disk where the engine picks it up.
pub(crate) async fn helper_models(state: &AppState, holders: &RoleHolders) -> Vec<HelperModel> {
    let settings = state.settings_repo.get().await.unwrap_or_default();
    let mut facts = HelperFacts::default();

    // The conversation row is a llama.cpp file only when the row id says `gguf/`.
    let chat = holders
        .chat
        .as_deref()
        .and_then(|id| id.strip_prefix("gguf/").map(|name| (id, name)));
    if let Some((id, name)) = chat {
        if let Some(EncoderState::Ready { .. }) = state.agent.vision_state("local", name) {
            let label = chat_model_encoder(state, name)
                .await
                .map(|spec| spec.label.to_string())
                .unwrap_or_else(|| name.to_string());
            facts.pictures = Some((id.to_string(), label));
        }
        // The engine attaches a drafter on budgeted devices only (goose_agent.rs).
        if pond_core::models::domain::device_budget::budgeted_device() {
            if let (Some(spec), Some(dd)) = (drafter_for(name), state.data_dir.as_ref()) {
                if drafter_path(dd, &spec).exists() {
                    facts.drafter = Some((id.to_string(), spec.id));
                }
            }
        }
    }

    if pond_adapters_vision::LiveCameraRegistry::global()
        .status()
        .classifier_available
    {
        facts.camera = Some(camera_detector_name(&settings.vision_classifier_model));
    }
    facts.faces = state
        .face_recognition
        .as_ref()
        .is_some_and(|slot| slot.current().is_ok());

    let helpers = helpers_from(facts);
    tracing::debug!(
        kind = "model_helpers_listed",
        count = helpers.len(),
        ids = ?helpers.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
        "helper models in use"
    );
    helpers
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::recommended::Rank;

    #[test]
    fn nothing_known_lists_no_helper() {
        assert!(helpers_from(HelperFacts::default()).is_empty());
    }

    #[test]
    fn each_helper_is_listed_under_its_job_and_names_the_row_it_serves() {
        let helpers = helpers_from(HelperFacts {
            pictures: Some(("gguf/gemma-4-E4B-it".into(), "Picture support".into())),
            drafter: Some(("gguf/gemma-4-E4B-it".into(), "mtp-gemma-4-E4B-it")),
            camera: Some("YOLOX-Nano".into()),
            faces: true,
        });
        let listed: Vec<_> = helpers
            .iter()
            .map(|h| {
                (
                    h.id.as_str(),
                    h.uses.as_slice(),
                    h.section,
                    h.serves.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                (
                    "gguf/gemma-4-E4B-it#pictures",
                    &[ModelUse::Pictures][..],
                    ModelSection::Pictures,
                    Some("gguf/gemma-4-E4B-it")
                ),
                (
                    "camera_detector",
                    &[ModelUse::Camera][..],
                    ModelSection::Pictures,
                    None
                ),
                (
                    "face_recognition",
                    &[ModelUse::Faces][..],
                    ModelSection::Pictures,
                    None
                ),
                (
                    "mtp-gemma-4-E4B-it",
                    &[ModelUse::Drafter][..],
                    ModelSection::Other,
                    Some("gguf/gemma-4-E4B-it")
                ),
            ]
        );
        assert_eq!(helpers[0].title.as_deref(), Some("Picture support"));
    }

    #[test]
    fn the_camera_detector_is_named_by_its_file() {
        assert_eq!(camera_detector_name(""), "YOLOX-Nano");
        assert_eq!(camera_detector_name("  "), "YOLOX-Nano");
        assert_eq!(camera_detector_name("custom/yolox_s.onnx"), "yolox_s");
    }

    #[test]
    fn a_row_carries_the_jobs_it_holds_and_its_section() {
        let row = pond_core::models::domain::embedding_models::ALL_MINILM_L6_V2.record();
        let holders = RoleHolders {
            embedding: Some(row.id.clone()),
            ..RoleHolders::default()
        };
        let dto = record_to_dto(
            &row,
            &[],
            &holders,
            EngineAvailability {
                litert_library: true,
            },
        );
        assert_eq!(dto.roles, [ModelUse::Embedding]);
        assert_eq!(dto.section, ModelSection::Memory);
        assert!(!dto.active, "`active` still reads the assignment table");
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["roles"], serde_json::json!(["embedding"]));
        assert_eq!(json["section"], "memory");

        let idle = record_to_dto(
            &row,
            &[],
            &RoleHolders::default(),
            EngineAvailability {
                litert_library: true,
            },
        );
        assert!(idle.roles.is_empty());
        assert_eq!(idle.section, ModelSection::Memory);
    }

    #[test]
    fn a_desktop_reads_the_reason_and_no_orin_number() {
        let id = "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL";
        let desktop = recommendation(id, DeviceClass::Desktop).unwrap();
        assert_eq!(desktop.rank, Rank::Primary);
        assert!(desktop.measured.is_none());
        let orin = recommendation(id, DeviceClass::Orin).unwrap();
        let measured = orin.measured.unwrap();
        assert_eq!(measured.window_tokens, Some(16_384));
        assert_eq!(measured.tokens_per_second_min, Some(15));
        assert_eq!(measured.tokens_per_second_max, Some(16));
        assert!(recommendation("gguf/llama-3.2-3b", DeviceClass::Orin).is_none());
    }

    /// An agent that answers `vision_state` from a fixed table.
    struct TableAgent(Option<EncoderState>);

    #[async_trait::async_trait]
    impl Agent for TableAgent {
        async fn chat(
            &self,
            _request: pond_core::shared::domain::agent::AgentRequest,
        ) -> anyhow::Result<pond_core::shared::domain::agent::AgentResponse> {
            unimplemented!("not exercised")
        }
        async fn chat_stream(
            &self,
            _request: pond_core::shared::domain::agent::AgentRequest,
        ) -> anyhow::Result<
            futures::stream::BoxStream<
                'static,
                anyhow::Result<pond_core::shared::domain::agent::AgentStreamEvent>,
            >,
        > {
            unimplemented!("not exercised")
        }
        fn vision_state(&self, provider: &str, _model: &str) -> Option<EncoderState> {
            assert_eq!(provider, "local", "a GGUF row is asked about as `local`");
            self.0.clone()
        }
    }

    const E2B: &str = "gemma-4-E2B-it-Q4_K_M";

    #[test]
    fn a_gguf_row_reads_images_by_the_agents_verdict_and_by_the_pairing_without_one() {
        let e2b = encoder_for_model(E2B, None);
        let facts = |state| gguf_vision(&TableAgent(state), E2B, e2b);
        let refused = facts(Some(EncoderState::NotOnThisDevice));
        assert!(!refused.reads_images);
        assert_eq!(refused.image_support_bytes, None);
        assert_eq!(
            refused.companion.map(|c| c.state),
            Some("not_on_this_device".to_string())
        );

        let absent = facts(Some(EncoderState::Absent));
        assert!(absent.reads_images);
        assert_eq!(absent.image_support_bytes, Some(986_833_728));
        let companion = absent.companion.unwrap();
        assert_eq!(companion.kind, "pictures");
        assert_eq!(companion.label, "Gemma 4 E2B");
        assert_eq!(companion.state, "available");

        assert!(facts(None).reads_images, "no verdict: the pairing decides");
        let text_only = gguf_vision(
            &TableAgent(Some(EncoderState::Unknown)),
            "Llama-3.2-3B",
            None,
        );
        assert!(!text_only.reads_images);
        assert!(text_only.companion.is_none(), "text only lists no add-on");
    }

    #[test]
    fn the_add_on_state_follows_picture_support() {
        assert_eq!(
            companion_state(Some(&EncoderState::Ready { bytes: Some(1) })),
            "installed"
        );
        assert_eq!(
            companion_state(Some(&EncoderState::Downloading { done: 1, total: 2 })),
            "downloading"
        );
        assert_eq!(
            companion_state(Some(&EncoderState::Verifying)),
            "verifying",
            "arrived and being checked, between the tracker's done and installed"
        );
        assert_eq!(
            companion_state(Some(&EncoderState::NotOnThisDevice)),
            "not_on_this_device"
        );
        for available in [
            None,
            Some(EncoderState::Absent),
            Some(EncoderState::Unknown),
        ] {
            assert_eq!(companion_state(available.as_ref()), "available");
        }
    }
}
