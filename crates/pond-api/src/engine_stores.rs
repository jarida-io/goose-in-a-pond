//! The idea engine's stores as the routes reach them, in one field of `AppState`: a store the
//! engine gains is one line here, not one in every place that builds the state.

use std::sync::Arc;

use pond_core::user_data::ports::dont_list::DontListRepository;
use pond_core::user_data::ports::engine_jobs::EngineJobRepository;
use pond_core::user_data::ports::fact_sheet::FactSheetRepository;
use pond_infra::sqlite_dont_list::SqliteDontListRepository;
use pond_infra::sqlite_engine_jobs::SqliteEngineJobRepository;
use pond_infra::sqlite_fact_sheet::SqliteFactSheetRepository;

#[derive(Clone)]
pub struct EngineStores {
    pub facts: Arc<dyn FactSheetRepository>,
    pub dont: Arc<dyn DontListRepository>,
    pub jobs: Arc<dyn EngineJobRepository>,
}

impl EngineStores {
    /// The SQLite stores over the system database.
    pub fn sqlite(system: sqlx::SqlitePool) -> Self {
        Self {
            facts: Arc::new(SqliteFactSheetRepository::new(system.clone())),
            dont: Arc::new(SqliteDontListRepository::new(system.clone())),
            jobs: Arc::new(SqliteEngineJobRepository::new(system)),
        }
    }
}
