//! Driven port: the cards members keep.

use anyhow::Result;
use async_trait::async_trait;

use crate::shared::domain::saved_item::SavedItem;

/// What a member may change on their card.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavedEdit {
    pub title: Option<String>,
    pub body: Option<String>,
    /// `Some(None)` takes it out of its collection.
    pub collection: Option<Option<String>>,
    pub shared: Option<bool>,
}

#[async_trait]
pub trait SavedItemRepository: Send + Sync {
    async fn create(&self, item: &SavedItem) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<SavedItem>>;
    /// The member's own cards and the household's shared ones, newest first; only `collection`'s
    /// and `kind`'s when given.
    async fn list_visible(
        &self,
        member: &str,
        collection: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<SavedItem>>;
    /// Visible cards whose title or text holds every word of `query`, newest first.
    async fn search(&self, member: &str, query: &str, limit: usize) -> Result<Vec<SavedItem>>;
    async fn count_for(&self, owner: &str) -> Result<usize>;
    /// The collections the member can see, with how many cards each holds.
    async fn collections(&self, member: &str) -> Result<Vec<(String, usize)>>;
    /// Applies `edit` to the owner's card; `false` when they have none by that id.
    async fn edit(&self, owner: &str, id: &str, edit: &SavedEdit) -> Result<bool>;
    async fn delete(&self, owner: &str, id: &str) -> Result<bool>;
}
