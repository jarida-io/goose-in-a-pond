//! How much a deep-research helper may read: a share of the model's own window, so a step reads
//! as many sources as its context can hold and then writes, rather than a fixed count of pages.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use pond_core::shared::domain::long_task::LongTaskKind;
use pond_core::shared::services::child_sessions::process_child_sessions;
use pond_core::shared::services::step_bindings::process_step_bindings;
use rmcp::model::RequestMetaObject;

/// The window's share a step's sources may fill; its brief, tools, thinking and writing need the rest.
const SOURCE_SHARE: f64 = 0.45;

/// Web text runs about this many bytes a token on Gemma's tokenizer.
const BYTES_PER_TOKEN: f64 = 3.6;

/// The window assumed before any turn has reported one: the nano's.
const ASSUMED_WINDOW_TOKENS: usize = 32_768;

/// Helpers remembered at once: a task runs one at a time, so this is many tasks' worth.
const KEPT_HELPERS: usize = 64;

static READ: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);

/// The engine session behind this call when it is a deep-research step's helper.
pub(crate) fn research_session(meta: &RequestMetaObject) -> Option<String> {
    let session = crate::session_from_meta(meta)?;
    process_step_bindings()
        .binding_for_helper(&session, &process_child_sessions())
        .filter(|binding| binding.kind == LongTaskKind::Research)
        .map(|_| session)
}

/// Bytes of sources a research step may read in a window of `window_tokens`.
fn room_for(window_tokens: usize) -> usize {
    (window_tokens as f64 * SOURCE_SHARE * BYTES_PER_TOKEN) as usize
}

fn room() -> usize {
    room_for(pond_core::shared::services::context_window::tokens().unwrap_or(ASSUMED_WINDOW_TOKENS))
}

/// Bytes this research helper may still read; `None` for a caller that is not one.
pub(crate) fn left(meta: &RequestMetaObject) -> Option<usize> {
    let session = research_session(meta)?;
    let read = READ
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&session)
        .copied()
        .unwrap_or(0);
    Some(room().saturating_sub(read))
}

/// Counts `bytes` handed to a research helper against its room; anyone else reads freely.
pub(crate) fn spend(meta: &RequestMetaObject, bytes: usize) {
    let Some(session) = research_session(meta) else {
        return;
    };
    let mut read = READ.lock().unwrap_or_else(|e| e.into_inner());
    if read.len() >= KEPT_HELPERS && !read.contains_key(&session) {
        read.clear();
    }
    *read.entry(session).or_insert(0) += bytes;
}

/// Said to a research helper whose room is full, in place of another read.
pub(crate) const ROOM_FULL: &str = "This step has read as much as its window can hold. Read no \
     more: save what you found now with artifact_write, with finish true.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_room_grows_with_the_window() {
        let nano = room_for(32_768);
        assert!((50_000..56_000).contains(&nano), "{nano}");
        let wide = room_for(65_536);
        assert!(wide.abs_diff(nano * 2) <= 1, "{wide} against {nano}");
    }

    #[test]
    fn a_caller_that_is_not_a_research_helper_has_no_room_to_count() {
        let mut meta = RequestMetaObject::new();
        meta.0.insert(
            crate::session_meta::SESSION_ID_META_KEY.to_string(),
            serde_json::json!("not-a-helper"),
        );
        assert_eq!(left(&meta), None);
        spend(&meta, 10_000);
        assert!(READ.lock().unwrap().get("not-a-helper").is_none());
    }
}
