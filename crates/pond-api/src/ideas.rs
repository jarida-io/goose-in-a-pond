//! Ideas Goose researched for a member, which Goose or the member can act on.

use axum::Router;

use std::sync::Arc;

use crate::AppState;

/// Whether this Pond serves this part of My Goose; `/me/hub` reports it.
pub(crate) const SERVED: bool = false;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
}
