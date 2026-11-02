//! Transient tool results in goose's own session store. The model reads such a result whole for
//! the rest of its turn; once the turn is over, however it ended, the stored conversation keeps
//! only the stub the result's `_meta` names (`pond_core::mcp::domain::transient_result`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use goose::conversation::message::{Message, MessageContent};
use goose::conversation::Conversation;
use goose::session::SessionManager;
use pond_core::mcp::domain::transient_result::{
    keep_from_meta, stub_owed, sweep_due, StoredResult, SweepPoint,
};
use rmcp::model::{CallToolResult, ContentBlock};

/// The stub a result asks history to keep in its place; `None` for a result kept whole.
pub(crate) fn keep_of(result: &CallToolResult) -> Option<String> {
    result
        .meta
        .as_ref()
        .and_then(|meta| keep_from_meta(&meta.0))
}

/// `result` as history keeps it: its stub alone, its marker kept so a later sweep sees it done.
fn as_kept(result: &CallToolResult, keep: &str) -> CallToolResult {
    let mut kept = CallToolResult::success(vec![ContentBlock::text(keep.to_string())]);
    kept.is_error = result.is_error;
    kept.meta = result.meta.clone();
    kept
}

/// The stub `content` still owes, by pond-core's rule; `None` for anything else.
fn owed_stub(content: &MessageContent) -> Option<String> {
    let MessageContent::ToolResponse(response) = content else {
        return None;
    };
    let result = response.tool_result.as_ref().ok()?;
    stub_owed(&StoredResult {
        meta: result.meta.as_ref().map(|meta| &meta.0),
        blocks: result
            .content
            .iter()
            .map(|block| block.as_text().map(|t| t.text.as_str()))
            .collect(),
        has_structured_content: result.structured_content.is_some(),
    })
}

/// `messages` with every transient tool response reduced to its stub, and how many were; `None`
/// when none was owed, so an unchanged conversation is never rewritten. Ids, roles and every
/// other part stay as they were: a provider rejects a tool response re-keyed from its call.
pub(crate) fn redact(messages: &[Message]) -> Option<(Vec<Message>, usize)> {
    if !messages
        .iter()
        .any(|m| m.content.iter().any(|c| owed_stub(c).is_some()))
    {
        return None;
    }
    let mut redacted = 0usize;
    let rebuilt = messages
        .iter()
        .map(|message| {
            let mut kept = message.clone();
            for part in kept.content.iter_mut() {
                let Some(keep) = owed_stub(part) else {
                    continue;
                };
                if let MessageContent::ToolResponse(response) = part {
                    let Ok(result) = &response.tool_result else {
                        continue;
                    };
                    let kept_result = as_kept(result, &keep);
                    response.tool_result = Ok(kept_result);
                    redacted += 1;
                }
            }
            kept
        })
        .collect();
    Some((rebuilt, redacted))
}

/// Rewrite `goose_sid`'s stored conversation so no transient result outlives its turn. The caller
/// holds that conversation's rewrite lock. Returns how many results were reduced.
pub(crate) async fn sweep(manager: &SessionManager, goose_sid: &str, when: &'static str) -> usize {
    let Ok(session) = manager.get_session(goose_sid, true).await else {
        return 0;
    };
    let Some(conversation) = session.conversation else {
        return 0;
    };
    let Some((messages, redacted)) = redact(conversation.messages()) else {
        return 0;
    };
    match manager
        .replace_conversation(goose_sid, &Conversation::new_unvalidated(messages))
        .await
    {
        Ok(()) => {
            note_redacted(goose_sid, when, redacted);
            redacted
        }
        Err(e) => {
            tracing::warn!(
                goose_sid = %goose_sid,
                error = %e,
                "a transient tool result could not be reduced to its stub; the next turn retries"
            );
            0
        }
    }
}

pub(crate) fn note_redacted(goose_sid: &str, when: &'static str, redacted: usize) {
    tracing::info!(
        target: "giap::trace",
        kind = "transient_redacted",
        goose_sid = %goose_sid,
        when,
        redacted,
    );
}

/// One conversation's turns, as the rewrites between them see them.
#[derive(Default)]
struct ConversationTurns {
    /// Held by every rewrite of the stored conversation between turns, so none interleaves
    /// another's read and write.
    rewrite: tokio::sync::Mutex<()>,
    /// Turns begun, so a late rewrite can tell a newer turn has the conversation.
    begun: AtomicU64,
}

/// Every conversation's turn bookkeeping, one entry per goose session this process has run.
#[derive(Default)]
pub(crate) struct TransientSweeps {
    conversations: Mutex<HashMap<String, Arc<ConversationTurns>>>,
}

impl TransientSweeps {
    /// Opens a turn on `goose_sid`. Its guard sweeps the conversation when the turn ends.
    pub(crate) fn begin(&self, goose_sid: &str, manager: Arc<SessionManager>) -> TurnSweep {
        let turns = self
            .conversations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(goose_sid.to_string())
            .or_default()
            .clone();
        let generation = turns.begun.fetch_add(1, Ordering::SeqCst) + 1;
        TurnSweep {
            turns,
            generation,
            goose_sid: goose_sid.to_string(),
            manager,
            saw_transient: false,
            finished: false,
        }
    }
}

/// One turn's duty to leave no transient result in the store. [`TurnSweep::finish`] discharges it
/// when the turn ends on its own; dropping it unfinished (a cancel, an early return) hands the
/// sweep to a task.
pub(crate) struct TurnSweep {
    turns: Arc<ConversationTurns>,
    generation: u64,
    goose_sid: String,
    manager: Arc<SessionManager>,
    saw_transient: bool,
    finished: bool,
}

impl TurnSweep {
    /// The conversation's rewrite lock, for the rewrite before the turn's first reply.
    pub(crate) async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.turns.rewrite.lock().await
    }

    pub(crate) fn note_transient(&mut self) {
        self.saw_transient = true;
    }

    /// The turn ended on its own: sweep, when due, before the caller hears it is done.
    pub(crate) async fn finish(&mut self) {
        self.finished = true;
        if !sweep_due(SweepPoint::TurnEnd {
            saw_transient: self.saw_transient,
        }) {
            return;
        }
        let _held = self.turns.rewrite.lock().await;
        sweep(&self.manager, &self.goose_sid, "turn_end").await;
    }
}

impl Drop for TurnSweep {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let turns = self.turns.clone();
        let generation = self.generation;
        let goose_sid = std::mem::take(&mut self.goose_sid);
        let manager = self.manager.clone();
        runtime.spawn(async move {
            let _held = turns.rewrite.lock().await;
            let newer_turn_began = turns.begun.load(Ordering::SeqCst) != generation;
            if !sweep_due(SweepPoint::TurnDropped { newer_turn_began }) {
                return;
            }
            sweep(&manager, &goose_sid, "turn_dropped").await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goose::config::GooseMode;
    use goose::session::session_manager::SessionType;
    use pond_mcp_server::web_search::{result_from, WebHit};

    const PAGE_TEXT: &str = "Nairobi National Park is open every day from 6:00 AM to 6:00 PM.";

    fn search_result() -> CallToolResult {
        let hits = vec![WebHit {
            title: "Nairobi National Park".to_string(),
            url: "https://www.kws.go.ke/nairobi-national-park".to_string(),
            description: "BRAVE SNIPPET about the park.".to_string(),
            age: None,
            extra_snippets: Vec::new(),
        }];
        result_from(
            "What time does Nairobi National Park close?",
            &hits,
            &[Some(vec![PAGE_TEXT.to_string()])],
        )
    }

    fn turn(result: CallToolResult) -> Vec<Message> {
        vec![
            Message::user().with_text("What time does Nairobi National Park close?"),
            Message::assistant().with_tool_request(
                "call-1",
                Ok(rmcp::model::CallToolRequestParams::new(
                    "giap-knowledge__search_web".to_string(),
                )),
            ),
            Message::user().with_tool_response("call-1", Ok(result)),
            Message::assistant().with_text("It closes at 6 pm."),
        ]
    }

    #[test]
    fn a_search_result_carries_its_stub() {
        let keep = keep_of(&search_result()).expect("search_web marks its result transient");
        assert_eq!(
            keep,
            "Web search results are not kept after the turn. Sources: kws.go.ke."
        );
    }

    #[test]
    fn a_transient_result_is_reduced_to_its_stub_and_nothing_else_moves() {
        let messages = turn(search_result());
        let (redacted, count) = redact(&messages).expect("one result was owed its stub");
        assert_eq!(count, 1);
        assert_eq!(redacted.len(), messages.len());
        for (before, after) in messages.iter().zip(&redacted) {
            assert_eq!(before.id, after.id);
            assert_eq!(before.role, after.role);
            assert_eq!(before.created, after.created);
        }
        assert_eq!(redacted[0], messages[0]);
        assert_eq!(redacted[1], messages[1]);
        assert_eq!(redacted[3], messages[3]);
        let MessageContent::ToolResponse(response) = &redacted[2].content[0] else {
            panic!("the tool response stays a tool response");
        };
        assert_eq!(response.id, "call-1", "the pairing with its call is kept");
        let result = response.tool_result.as_ref().unwrap();
        assert_eq!(result.content.len(), 1, "the card goes with the text");
        assert_eq!(
            result.content[0].as_text().unwrap().text,
            "Web search results are not kept after the turn. Sources: kws.go.ke."
        );
        let stored = serde_json::to_string(&redacted).unwrap();
        for gone in [
            PAGE_TEXT,
            "BRAVE SNIPPET",
            "mcp-ui",
            "nairobi-national-park",
        ] {
            assert!(!stored.contains(gone), "{gone:?} survived: {stored}");
        }
    }

    #[test]
    fn a_reduced_conversation_is_left_alone() {
        let (once, _) = redact(&turn(search_result())).unwrap();
        assert!(redact(&once).is_none(), "a second sweep must write nothing");
    }

    #[test]
    fn results_with_no_marker_are_never_touched() {
        let plain = CallToolResult::success(vec![ContentBlock::text("24C and clear")]);
        assert!(redact(&turn(plain)).is_none());
    }

    async fn stored_session(dir: &std::path::Path) -> (Arc<SessionManager>, String) {
        let manager = Arc::new(SessionManager::new(dir.to_path_buf()));
        let session = manager
            .create_session(
                dir.to_path_buf(),
                "transient-test".to_string(),
                SessionType::User,
                GooseMode::Auto,
            )
            .await
            .unwrap();
        for message in turn(search_result()) {
            manager.add_message(&session.id, &message).await.unwrap();
        }
        (manager, session.id)
    }

    async fn stored_text(manager: &SessionManager, id: &str) -> String {
        let session = manager.get_session(id, true).await.unwrap();
        serde_json::to_string(session.conversation.unwrap().messages()).unwrap()
    }

    #[tokio::test]
    async fn a_finished_turn_leaves_only_the_stub_in_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let (manager, id) = stored_session(dir.path()).await;
        assert!(stored_text(&manager, &id).await.contains(PAGE_TEXT));

        let sweeps = TransientSweeps::default();
        let mut turn = sweeps.begin(&id, manager.clone());
        turn.note_transient();
        turn.finish().await;

        let stored = stored_text(&manager, &id).await;
        assert!(!stored.contains(PAGE_TEXT), "{stored}");
        assert!(!stored.contains("mcp-ui"), "{stored}");
        assert!(stored.contains("Sources: kws.go.ke."), "{stored}");
        assert!(stored.contains("It closes at 6 pm."), "{stored}");
    }

    #[tokio::test]
    async fn a_dropped_turn_is_swept_by_a_task() {
        let dir = tempfile::tempdir().unwrap();
        let (manager, id) = stored_session(dir.path()).await;
        let sweeps = TransientSweeps::default();
        drop(sweeps.begin(&id, manager.clone()));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while stored_text(&manager, &id).await.contains(PAGE_TEXT) {
            assert!(
                std::time::Instant::now() < deadline,
                "the dropped turn's sweep never ran"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn a_dropped_turn_leaves_a_newer_turn_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (manager, id) = stored_session(dir.path()).await;
        let sweeps = TransientSweeps::default();
        let newer = {
            let older = sweeps.begin(&id, manager.clone());
            // The newer turn holds the conversation before the older one's task can run.
            let newer = sweeps.begin(&id, manager.clone());
            let held = newer.lock().await;
            drop(older);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            drop(held);
            newer
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            stored_text(&manager, &id).await.contains(PAGE_TEXT),
            "the older turn's task rewrote a conversation a newer turn holds"
        );
        let mut newer = newer;
        newer.note_transient();
        newer.finish().await;
        assert!(!stored_text(&manager, &id).await.contains(PAGE_TEXT));
    }
}
