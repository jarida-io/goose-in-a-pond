//! Links a member asked Goose to watch the price of.

use axum::Router;

use std::sync::Arc;

use crate::AppState;

/// Whether this Pond serves this part of My Goose; `/me/hub` reports it.
pub(crate) const SERVED: bool = false;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
}
