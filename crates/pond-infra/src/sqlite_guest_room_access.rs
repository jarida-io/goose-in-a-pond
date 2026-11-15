//! SQLite-backed implementation of `GuestRoomAccess` over the `guest_accessible_rooms` table.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::user_data::ports::guest_room_access::GuestRoomAccess;
use sqlx::{Pool, Sqlite};
use std::collections::HashSet;

pub struct SqliteGuestRoomAccess {
    pool: Pool<Sqlite>,
}

impl SqliteGuestRoomAccess {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl GuestRoomAccess for SqliteGuestRoomAccess {
    async fn set_guest_accessible(&self, room_id: &str, accessible: bool) -> Result<()> {
        if accessible {
            sqlx::query("INSERT OR IGNORE INTO guest_accessible_rooms (room_id) VALUES (?)")
                .bind(room_id)
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query("DELETE FROM guest_accessible_rooms WHERE room_id = ?")
                .bind(room_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    async fn accessible_room_ids(&self) -> Result<HashSet<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT room_id FROM guest_accessible_rooms")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn is_guest_accessible(&self, room_id: &str) -> Result<bool> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT room_id FROM guest_accessible_rooms WHERE room_id = ?")
                .bind(room_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::sqlite_room_registry::SqliteRoomRegistry;
    use pond_core::user_data::ports::room_registry::RoomRegistry;
    use tempfile::tempdir;

    async fn make() -> (SqliteGuestRoomAccess, SqliteRoomRegistry, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (
            SqliteGuestRoomAccess::new(db.system.clone()),
            SqliteRoomRegistry::new(db.system.clone()),
            tmp,
        )
    }

    #[tokio::test]
    async fn a_room_starts_inaccessible_to_guests() {
        let (access, rooms, _tmp) = make().await;
        let kitchen = rooms.create("Kitchen").await.unwrap();
        assert!(!access.is_guest_accessible(&kitchen.id).await.unwrap());
    }

    #[tokio::test]
    async fn granting_then_revoking_access_round_trips() {
        let (access, rooms, _tmp) = make().await;
        let kitchen = rooms.create("Kitchen").await.unwrap();

        access
            .set_guest_accessible(&kitchen.id, true)
            .await
            .unwrap();
        assert!(access.is_guest_accessible(&kitchen.id).await.unwrap());
        assert_eq!(
            access.accessible_room_ids().await.unwrap(),
            HashSet::from([kitchen.id.clone()])
        );

        access
            .set_guest_accessible(&kitchen.id, false)
            .await
            .unwrap();
        assert!(!access.is_guest_accessible(&kitchen.id).await.unwrap());
    }

    #[tokio::test]
    async fn granting_twice_is_idempotent() {
        let (access, rooms, _tmp) = make().await;
        let kitchen = rooms.create("Kitchen").await.unwrap();
        access
            .set_guest_accessible(&kitchen.id, true)
            .await
            .unwrap();
        access
            .set_guest_accessible(&kitchen.id, true)
            .await
            .unwrap();
        assert_eq!(access.accessible_room_ids().await.unwrap().len(), 1);
    }
}
