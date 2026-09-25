//! SQLite-backed implementation of the PeerDirectory port.

use async_trait::async_trait;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::domain::trust_scope::TrustScope;
use pond_core::mesh::ports::peer_directory::{PeerDirectory, PeerDirectoryError};
use sqlx::{Pool, Sqlite};

fn trust_scope_to_str(scope: TrustScope) -> &'static str {
    match scope {
        TrustScope::SelfOwned => "self_owned",
        TrustScope::Circle => "circle",
    }
}

fn str_to_trust_scope(s: &str) -> Result<TrustScope, PeerDirectoryError> {
    match s {
        "self_owned" => Ok(TrustScope::SelfOwned),
        "circle" => Ok(TrustScope::Circle),
        other => Err(PeerDirectoryError::General(format!(
            "unknown trust_scope in DB: '{other}'"
        ))),
    }
}

pub struct SqlitePeerDirectory {
    pool: Pool<Sqlite>,
}

impl SqlitePeerDirectory {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PeerDirectory for SqlitePeerDirectory {
    async fn add_trusted_peer(
        &self,
        peer: PeerId,
        scope: TrustScope,
    ) -> Result<(), PeerDirectoryError> {
        sqlx::query(
            "INSERT INTO mesh_trusted_peers (peer_id, trust_scope, created_at) \
             VALUES (?, ?, datetime('now')) \
             ON CONFLICT(peer_id) DO UPDATE SET trust_scope = excluded.trust_scope",
        )
        .bind(peer.to_string())
        .bind(trust_scope_to_str(scope))
        .execute(&self.pool)
        .await
        .map_err(|e| PeerDirectoryError::General(e.to_string()))?;
        Ok(())
    }

    async fn remove_trusted_peer(&self, peer: PeerId) -> Result<(), PeerDirectoryError> {
        sqlx::query("DELETE FROM mesh_trusted_peers WHERE peer_id = ?")
            .bind(peer.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| PeerDirectoryError::General(e.to_string()))?;
        Ok(())
    }

    async fn list_trusted_peers(
        &self,
        scope: Option<TrustScope>,
    ) -> Result<Vec<PeerId>, PeerDirectoryError> {
        let rows: Vec<(String,)> = match scope {
            Some(scope) => {
                sqlx::query_as("SELECT peer_id FROM mesh_trusted_peers WHERE trust_scope = ?")
                    .bind(trust_scope_to_str(scope))
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| PeerDirectoryError::General(e.to_string()))?
            }
            None => sqlx::query_as("SELECT peer_id FROM mesh_trusted_peers")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| PeerDirectoryError::General(e.to_string()))?,
        };

        rows.into_iter()
            .map(|(id,)| {
                id.parse::<PeerId>()
                    .map_err(|e| PeerDirectoryError::General(e.to_string()))
            })
            .collect()
    }

    async fn trust_scope_of(&self, peer: PeerId) -> Result<Option<TrustScope>, PeerDirectoryError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT trust_scope FROM mesh_trusted_peers WHERE peer_id = ?")
                .bind(peer.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| PeerDirectoryError::General(e.to_string()))?;
        row.map(|(s,)| str_to_trust_scope(&s)).transpose()
    }

    async fn record_peer_address(
        &self,
        peer: PeerId,
        address: String,
    ) -> Result<(), PeerDirectoryError> {
        sqlx::query("UPDATE mesh_trusted_peers SET last_known_address = ? WHERE peer_id = ?")
            .bind(address)
            .bind(peer.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| PeerDirectoryError::General(e.to_string()))?;
        Ok(())
    }

    async fn known_addresses(&self) -> Result<Vec<(PeerId, String)>, PeerDirectoryError> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT peer_id, last_known_address FROM mesh_trusted_peers \
             WHERE last_known_address IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PeerDirectoryError::General(e.to_string()))?;

        rows.into_iter()
            .map(|(id, addr)| {
                id.parse::<PeerId>()
                    .map(|peer| (peer, addr))
                    .map_err(|e| PeerDirectoryError::General(e.to_string()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::sync::Arc;
    use tempfile::tempdir;

    async fn make_directory() -> (SqlitePeerDirectory, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (SqlitePeerDirectory::new(db.system), tmp)
    }

    #[tokio::test]
    async fn trait_object_conformance() {
        let (dir, _tmp) = make_directory().await;
        let _: Arc<dyn PeerDirectory> = Arc::new(dir);
    }

    #[tokio::test]
    async fn add_then_lookup_scope() {
        let (dir, _tmp) = make_directory().await;
        let peer = PeerId::from([1u8; 32]);
        dir.add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
        assert_eq!(
            dir.trust_scope_of(peer).await.unwrap(),
            Some(TrustScope::Circle)
        );
    }

    #[tokio::test]
    async fn unknown_peer_has_no_scope() {
        let (dir, _tmp) = make_directory().await;
        let peer = PeerId::from([2u8; 32]);
        assert_eq!(dir.trust_scope_of(peer).await.unwrap(), None);
    }

    #[tokio::test]
    async fn remove_clears_trust() {
        let (dir, _tmp) = make_directory().await;
        let peer = PeerId::from([3u8; 32]);
        dir.add_trusted_peer(peer, TrustScope::SelfOwned)
            .await
            .unwrap();
        dir.remove_trusted_peer(peer).await.unwrap();
        assert_eq!(dir.trust_scope_of(peer).await.unwrap(), None);
    }

    #[tokio::test]
    async fn list_trusted_peers_filters_by_scope() {
        let (dir, _tmp) = make_directory().await;
        let self_owned = PeerId::from([4u8; 32]);
        let circle = PeerId::from([5u8; 32]);
        dir.add_trusted_peer(self_owned, TrustScope::SelfOwned)
            .await
            .unwrap();
        dir.add_trusted_peer(circle, TrustScope::Circle)
            .await
            .unwrap();

        let circle_only = dir
            .list_trusted_peers(Some(TrustScope::Circle))
            .await
            .unwrap();
        assert_eq!(circle_only, vec![circle]);

        let all = dir.list_trusted_peers(None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn re_adding_a_peer_updates_scope() {
        let (dir, _tmp) = make_directory().await;
        let peer = PeerId::from([6u8; 32]);
        dir.add_trusted_peer(peer, TrustScope::SelfOwned)
            .await
            .unwrap();
        dir.add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
        assert_eq!(
            dir.trust_scope_of(peer).await.unwrap(),
            Some(TrustScope::Circle)
        );
    }
}
