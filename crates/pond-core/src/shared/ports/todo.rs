//! Driven ports for to-dos: where a member's to-dos, hooks and places are kept, and the queue of
//! events their hooks match.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::shared::domain::todo::{HookEvent, Place, StateFilter, Todo};

#[async_trait]
pub trait TodoRepository: Send + Sync {
    /// Writes a new to-do and its hooks.
    async fn create(&self, todo: &Todo) -> Result<()>;
    /// The owner's to-do with its hooks; `None` when they have none by that id.
    async fn get(&self, owner: &str, id: &str) -> Result<Option<Todo>>;
    /// The owner's to-dos with their hooks: open ones by due time, then the newest.
    async fn list(&self, owner: &str, filter: StateFilter) -> Result<Vec<Todo>>;
    /// Writes the to-do's own fields, and replaces its hooks with `todo.hooks` when
    /// `replace_hooks`.
    async fn update(&self, todo: &Todo, replace_hooks: bool) -> Result<()>;
    /// Deletes the owner's to-do and its hooks; `false` when they have none by that id.
    async fn delete(&self, owner: &str, id: &str) -> Result<bool>;

    /// Open to-dos with a time or repeat hook due at or before `now`, each with every hook.
    async fn due_clock_hooks(&self, now: DateTime<Utc>) -> Result<Vec<Todo>>;
    /// Open to-dos with no hooks whose due time has come.
    async fn due_dates(&self, now: DateTime<Utc>) -> Result<Vec<Todo>>;
    /// Open to-dos whose follow-up after a not-yet has come.
    async fn follow_ups_due(&self, now: DateTime<Utc>) -> Result<Vec<Todo>>;
    /// Open to-dos with a place, event or connector hook, each with every hook.
    async fn listening(&self) -> Result<Vec<Todo>>;
    /// Records a hook's firing and its next time, `None` once spent.
    async fn hook_fired(
        &self,
        hook_id: &str,
        fired_at: DateTime<Utc>,
        next_fire_at: Option<DateTime<Utc>>,
    ) -> Result<()>;
    /// Records a push about a to-do: when, the questions asked so far, and no follow-up pending.
    async fn pushed(&self, todo_id: &str, at: DateTime<Utc>, asked: u32) -> Result<()>;

    /// The owner's places, oldest first.
    async fn places(&self, owner: &str) -> Result<Vec<Place>>;
    async fn add_place(&self, place: &Place) -> Result<()>;
    /// Deletes the owner's place and the hooks that named it; `false` when they have none by
    /// that id.
    async fn delete_place(&self, owner: &str, id: &str) -> Result<bool>;
}

/// The `hook_events` queue: every producer raises, the hook engine reads and marks.
#[async_trait]
pub trait HookEventQueue: Send + Sync {
    async fn raise(&self, event: &HookEvent) -> Result<()>;
    /// Events not yet handled, oldest first.
    async fn unhandled(&self, limit: usize) -> Result<Vec<HookEvent>>;
    async fn mark_handled(&self, ids: &[String], at: DateTime<Utc>) -> Result<()>;
    /// Deletes events handled before `before`; returns how many.
    async fn prune_handled(&self, before: DateTime<Utc>) -> Result<u64>;
}
