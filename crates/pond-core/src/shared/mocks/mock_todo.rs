//! In-memory [`TodoRepository`] and [`HookEventQueue`] with `sqlite_todo`'s semantics: every read
//! is the owner's, hooks go with their to-do, and closed or fully asked to-dos are never due.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::shared::domain::todo::{
    HookEvent, HookSpec, Place, StateFilter, Todo, TodoKind, TodoState, MAX_ASKS,
};
use crate::shared::ports::todo::{HookEventQueue, TodoRepository};

#[derive(Default)]
pub struct MockTodoRepository {
    todos: Mutex<Vec<Todo>>,
    places: Mutex<Vec<Place>>,
    events: Mutex<Vec<(HookEvent, Option<DateTime<Utc>>)>>,
}

impl MockTodoRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event raised, with when it was handled.
    pub fn events(&self) -> Vec<(HookEvent, Option<DateTime<Utc>>)> {
        self.events.lock().unwrap().clone()
    }
}

/// A to-do asked as often as Goose asks is no longer due for a question.
fn asking_over(todo: &Todo) -> bool {
    todo.kind == TodoKind::Todo && todo.ask && todo.asked >= MAX_ASKS
}

#[async_trait]
impl TodoRepository for MockTodoRepository {
    async fn create(&self, todo: &Todo) -> Result<()> {
        self.todos.lock().unwrap().push(todo.clone());
        Ok(())
    }

    async fn get(&self, owner: &str, id: &str) -> Result<Option<Todo>> {
        Ok(self
            .todos
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.owner == owner && t.id == id)
            .cloned())
    }

    async fn list(&self, owner: &str, filter: StateFilter) -> Result<Vec<Todo>> {
        Ok(self
            .todos
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.owner == owner && filter.admits(t.state))
            .cloned()
            .collect())
    }

    async fn update(&self, todo: &Todo, replace_hooks: bool) -> Result<()> {
        let mut todos = self.todos.lock().unwrap();
        if let Some(kept) = todos.iter_mut().find(|t| t.id == todo.id) {
            let hooks = std::mem::take(&mut kept.hooks);
            *kept = todo.clone();
            if !replace_hooks {
                kept.hooks = hooks;
            }
        }
        Ok(())
    }

    async fn delete(&self, owner: &str, id: &str) -> Result<bool> {
        let mut todos = self.todos.lock().unwrap();
        let before = todos.len();
        todos.retain(|t| !(t.owner == owner && t.id == id));
        Ok(todos.len() < before)
    }

    async fn due_clock_hooks(&self, now: DateTime<Utc>) -> Result<Vec<Todo>> {
        Ok(self
            .todos
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.state == TodoState::Open)
            .filter(|t| {
                t.hooks.iter().any(|h| {
                    h.spec.listens_for().is_none() && h.next_fire_at.is_some_and(|at| at <= now)
                })
            })
            .cloned()
            .collect())
    }

    async fn due_dates(&self, now: DateTime<Utc>) -> Result<Vec<Todo>> {
        Ok(self
            .todos
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.due_without_hooks(now) && !asking_over(t))
            .cloned()
            .collect())
    }

    async fn follow_ups_due(&self, now: DateTime<Utc>) -> Result<Vec<Todo>> {
        Ok(self
            .todos
            .lock()
            .unwrap()
            .iter()
            .filter(|t| {
                t.state == TodoState::Open
                    && t.snoozed_until.is_some_and(|at| at <= now)
                    && !asking_over(t)
            })
            .cloned()
            .collect())
    }

    async fn listening(&self) -> Result<Vec<Todo>> {
        Ok(self
            .todos
            .lock()
            .unwrap()
            .iter()
            .filter(|t| {
                t.state == TodoState::Open && t.hooks.iter().any(|h| h.spec.listens_for().is_some())
            })
            .cloned()
            .collect())
    }

    async fn hook_fired(
        &self,
        hook_id: &str,
        fired_at: DateTime<Utc>,
        next_fire_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        for todo in self.todos.lock().unwrap().iter_mut() {
            for hook in todo.hooks.iter_mut().filter(|h| h.id == hook_id) {
                hook.last_fired_at = Some(fired_at);
                hook.next_fire_at = next_fire_at;
            }
        }
        Ok(())
    }

    async fn pushed(&self, todo_id: &str, at: DateTime<Utc>, asked: u32) -> Result<()> {
        if let Some(todo) = self
            .todos
            .lock()
            .unwrap()
            .iter_mut()
            .find(|t| t.id == todo_id)
        {
            todo.last_asked_at = Some(at);
            todo.asked = asked;
            todo.snoozed_until = None;
        }
        Ok(())
    }

    async fn places(&self, owner: &str) -> Result<Vec<Place>> {
        Ok(self
            .places
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.owner == owner)
            .cloned()
            .collect())
    }

    async fn add_place(&self, place: &Place) -> Result<()> {
        self.places.lock().unwrap().push(place.clone());
        Ok(())
    }

    async fn delete_place(&self, owner: &str, id: &str) -> Result<bool> {
        let mut places = self.places.lock().unwrap();
        let before = places.len();
        places.retain(|p| !(p.owner == owner && p.id == id));
        let removed = places.len() < before;
        if removed {
            for todo in self
                .todos
                .lock()
                .unwrap()
                .iter_mut()
                .filter(|t| t.owner == owner)
            {
                todo.hooks.retain(
                    |h| !matches!(&h.spec, HookSpec::Place { place_id, .. } if place_id == id),
                );
            }
        }
        Ok(removed)
    }
}

#[async_trait]
impl HookEventQueue for MockTodoRepository {
    async fn raise(&self, event: &HookEvent) -> Result<()> {
        self.events.lock().unwrap().push((event.clone(), None));
        Ok(())
    }

    async fn unhandled(&self, limit: usize) -> Result<Vec<HookEvent>> {
        Ok(self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, handled)| handled.is_none())
            .take(limit)
            .map(|(e, _)| e.clone())
            .collect())
    }

    async fn mark_handled(&self, ids: &[String], at: DateTime<Utc>) -> Result<()> {
        for (event, handled) in self.events.lock().unwrap().iter_mut() {
            if ids.contains(&event.id) {
                *handled = Some(at);
            }
        }
        Ok(())
    }

    async fn prune_handled(&self, before: DateTime<Utc>) -> Result<u64> {
        let mut events = self.events.lock().unwrap();
        let count = events.len();
        events.retain(|(_, handled)| handled.is_none_or(|at| at >= before));
        Ok((count - events.len()) as u64)
    }
}
