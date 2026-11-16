//! The idea engine's stores as the routes reach them, in one field of `AppState`: a store the
//! engine gains is one line here, not one in every place that builds the state.

use std::sync::Arc;

use pond_core::user_data::ports::anchors::AnchorRepository;
use pond_core::user_data::ports::captures::CaptureRepository;
use pond_core::user_data::ports::dont_list::DontListRepository;
use pond_core::user_data::ports::engine_jobs::EngineJobRepository;
use pond_core::user_data::ports::fact_sheet::FactSheetRepository;
use pond_core::user_data::ports::member_agents::MemberAgentRepository;
use pond_core::user_data::ports::questions::QuestionRepository;
use pond_infra::sqlite_anchors::SqliteAnchorRepository;
use pond_infra::sqlite_captures::SqliteCaptureRepository;
use pond_infra::sqlite_dont_list::SqliteDontListRepository;
use pond_infra::sqlite_engine_jobs::SqliteEngineJobRepository;
use pond_infra::sqlite_fact_sheet::SqliteFactSheetRepository;
use pond_infra::sqlite_member_agents::SqliteMemberAgentRepository;
use pond_infra::sqlite_questions::SqliteQuestionRepository;

#[derive(Clone)]
pub struct EngineStores {
    pub facts: Arc<dyn FactSheetRepository>,
    pub dont: Arc<dyn DontListRepository>,
    pub anchors: Arc<dyn AnchorRepository>,
    pub questions: Arc<dyn QuestionRepository>,
    pub captures: Arc<dyn CaptureRepository>,
    pub agents: Arc<dyn MemberAgentRepository>,
    pub jobs: Arc<dyn EngineJobRepository>,
}

impl EngineStores {
    /// The SQLite stores over the system database.
    pub fn sqlite(system: sqlx::SqlitePool) -> Self {
        Self {
            facts: Arc::new(SqliteFactSheetRepository::new(system.clone())),
            dont: Arc::new(SqliteDontListRepository::new(system.clone())),
            anchors: Arc::new(SqliteAnchorRepository::new(system.clone())),
            questions: Arc::new(SqliteQuestionRepository::new(system.clone())),
            captures: Arc::new(SqliteCaptureRepository::new(system.clone())),
            agents: Arc::new(SqliteMemberAgentRepository::new(system.clone())),
            jobs: Arc::new(SqliteEngineJobRepository::new(system)),
        }
    }
}
