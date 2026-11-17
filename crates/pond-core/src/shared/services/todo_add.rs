//! Adding a to-do to a member's list once: what is already open on it under the same key is
//! answered in place of a second.

use anyhow::Result;

use crate::shared::domain::todo::{todo_key, Todo};
use crate::shared::ports::todo::TodoRepository;

/// What adding a to-do came to.
#[derive(Debug, Clone, PartialEq)]
pub enum AddedTodo {
    New(Todo),
    /// The list already held it, so nothing was added; `moved` when it took the new ask's time
    /// or place because none of its own was left to fire.
    Listed {
        todo: Todo,
        moved: bool,
    },
}

impl AddedTodo {
    /// The to-do now on the list.
    pub fn todo(&self) -> &Todo {
        match self {
            Self::New(todo) | Self::Listed { todo, .. } => todo,
        }
    }
}

/// Writes `todo`, unless its owner's open list already holds one under the same key.
pub async fn add_once(repo: &dyn TodoRepository, todo: Todo) -> Result<AddedTodo> {
    let Some(mut kept) = repo
        .open_with_key(&todo.owner, &todo_key(&todo.text))
        .await?
    else {
        repo.create(&todo).await?;
        return Ok(AddedTodo::New(todo));
    };
    let moved = kept.take_moment_from(&todo, todo.created_at);
    if moved {
        repo.update(&kept, true).await?;
    }
    Ok(AddedTodo::Listed { todo: kept, moved })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::todo::{
        new_todo, HookCatalog, StateFilter, TodoSource, TodoState, Tz,
    };
    use crate::shared::mocks::mock_todo::MockTodoRepository;
    use chrono::{DateTime, Utc};
    use serde_json::{json, Value};

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    fn utc(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn asked(owner: &str, body: Value) -> Todo {
        new_todo(
            owner,
            &body,
            &HookCatalog::default(),
            TodoSource::Chat,
            None,
            utc("2026-11-09T07:00:00Z"),
            NAIROBI,
        )
        .unwrap()
    }

    fn reminder_at(at: &str) -> Value {
        json!({
            "kind": "reminder",
            "text": "Call the garage",
            "due_at": at,
            "hooks": [{"kind": "time", "spec": {"at": at}}],
        })
    }

    #[tokio::test]
    async fn what_is_open_on_the_list_is_not_added_twice_whatever_its_kind() {
        let repo = MockTodoRepository::new();
        let first = add_once(&repo, asked("jerry", json!({"text": "Call the garage"})))
            .await
            .unwrap();
        assert!(matches!(first, AddedTodo::New(_)));

        let again = add_once(
            &repo,
            asked(
                "jerry",
                json!({"kind": "reminder", "text": " call the GARAGE! "}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            again,
            AddedTodo::Listed {
                todo: first.todo().clone(),
                moved: false
            }
        );
        assert_eq!(repo.list("jerry", StateFilter::All).await.unwrap().len(), 1);

        let theirs = add_once(&repo, asked("liz", json!({"text": "Call the garage"})))
            .await
            .unwrap();
        assert!(matches!(theirs, AddedTodo::New(_)), "another member's list");
    }

    #[tokio::test]
    async fn a_done_or_dropped_todo_does_not_block_a_new_one() {
        let repo = MockTodoRepository::new();
        for closed in [TodoState::Done, TodoState::Dropped] {
            let added = add_once(&repo, asked("jerry", json!({"text": "Renew the logbook"})))
                .await
                .unwrap();
            assert!(matches!(added, AddedTodo::New(_)), "after {closed:?}");
            let mut todo = added.todo().clone();
            todo.state = closed;
            repo.update(&todo, false).await.unwrap();
        }
        assert_eq!(repo.list("jerry", StateFilter::All).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_listed_reminder_that_has_fired_is_set_again_not_doubled() {
        let repo = MockTodoRepository::new();
        let first = add_once(&repo, asked("jerry", reminder_at("2026-11-09T14:00:00Z")))
            .await
            .unwrap();
        let later = asked("jerry", reminder_at("2026-11-09T14:10:00Z"));

        let waiting = add_once(&repo, later.clone()).await.unwrap();
        assert_eq!(
            waiting,
            AddedTodo::Listed {
                todo: first.todo().clone(),
                moved: false
            },
            "its own time is still to come"
        );

        let fired = utc("2026-11-09T14:00:00Z");
        repo.hook_fired(&first.todo().hooks[0].id, fired, None)
            .await
            .unwrap();
        let set_again = add_once(&repo, later.clone()).await.unwrap();
        assert!(matches!(set_again, AddedTodo::Listed { moved: true, .. }));
        let open = repo.list("jerry", StateFilter::Open).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, first.todo().id);
        assert_eq!(open[0].hooks, later.hooks);
        assert_eq!(open[0].due_at, later.due_at);
    }
}
