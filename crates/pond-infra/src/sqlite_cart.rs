//! SQLite-backed `CartRepository` over `cart_items` and `cart_prices` (migration 0088).

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::shared::domain::cart::{CartItem, CartStatus, Notify, PricePoint};
use pond_core::shared::ports::cart::CartRepository;

pub struct SqliteCartRepository {
    pool: Pool<Sqlite>,
}

impl SqliteCartRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, profile_id, url, title, site, picture_key, currency, price, \
     previous_price, lowest_price, highest_price, target_price, in_stock, status, failures, \
     every_hours, notify, collection, checked_at, next_check_at, created_at, updated_at";

fn time(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .with_context(|| format!("bad cart timestamp {raw:?}"))
}

fn optional_time(raw: Option<String>) -> Result<Option<DateTime<Utc>>> {
    raw.as_deref().map(time).transpose()
}

fn item_of(row: &SqliteRow) -> Result<CartItem> {
    Ok(CartItem {
        id: row.try_get("id")?,
        owner: row.try_get("profile_id")?,
        url: row.try_get("url")?,
        title: row.try_get("title")?,
        site: row.try_get("site")?,
        picture_key: row.try_get("picture_key")?,
        currency: row.try_get("currency")?,
        price: row.try_get("price")?,
        previous: row.try_get("previous_price")?,
        lowest: row.try_get("lowest_price")?,
        highest: row.try_get("highest_price")?,
        target: row.try_get("target_price")?,
        in_stock: row.try_get::<Option<i64>, _>("in_stock")?.map(|v| v != 0),
        status: CartStatus::parse(&row.try_get::<String, _>("status")?),
        failures: row.try_get::<i64, _>("failures")?.max(0) as u32,
        every_hours: row.try_get::<i64, _>("every_hours")?.max(1) as u32,
        notify: Notify::parse(&row.try_get::<String, _>("notify")?).unwrap_or_default(),
        collection: row.try_get("collection")?,
        checked_at: optional_time(row.try_get("checked_at")?)?,
        next_check_at: optional_time(row.try_get("next_check_at")?)?,
        created_at: time(&row.try_get::<String, _>("created_at")?)?,
        updated_at: time(&row.try_get::<String, _>("updated_at")?)?,
    })
}

#[async_trait]
impl CartRepository for SqliteCartRepository {
    async fn create(&self, item: &CartItem) -> Result<bool> {
        let done = sqlx::query(&format!(
            "INSERT INTO cart_items ({COLUMNS}) VALUES \
             (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (profile_id, url) DO NOTHING"
        ))
        .bind(&item.id)
        .bind(&item.owner)
        .bind(&item.url)
        .bind(&item.title)
        .bind(&item.site)
        .bind(&item.picture_key)
        .bind(&item.currency)
        .bind(item.price)
        .bind(item.previous)
        .bind(item.lowest)
        .bind(item.highest)
        .bind(item.target)
        .bind(item.in_stock.map(i64::from))
        .bind(item.status.as_str())
        .bind(i64::from(item.failures))
        .bind(i64::from(item.every_hours))
        .bind(item.notify.as_str())
        .bind(&item.collection)
        .bind(item.checked_at.map(|t| t.to_rfc3339()))
        .bind(item.next_check_at.map(|t| t.to_rfc3339()))
        .bind(item.created_at.to_rfc3339())
        .bind(item.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    async fn get(&self, owner: &str, id: &str) -> Result<Option<CartItem>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM cart_items WHERE id = ? AND profile_id = ?"
        ))
        .bind(id)
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(item_of).transpose()
    }

    async fn find_by_url(&self, owner: &str, url: &str) -> Result<Option<CartItem>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM cart_items WHERE profile_id = ? AND url = ?"
        ))
        .bind(owner)
        .bind(url)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(item_of).transpose()
    }

    async fn list_for(&self, owner: &str) -> Result<Vec<CartItem>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM cart_items WHERE profile_id = ? \
             ORDER BY created_at DESC, rowid DESC"
        ))
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(item_of).collect()
    }

    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<CartItem>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM cart_items WHERE next_check_at IS NOT NULL \
             AND next_check_at <= ? ORDER BY next_check_at LIMIT ?"
        ))
        .bind(now.to_rfc3339())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(item_of).collect()
    }

    async fn save(&self, item: &CartItem) -> Result<()> {
        sqlx::query(
            "UPDATE cart_items SET title = ?, site = ?, picture_key = ?, currency = ?, price = ?, \
             previous_price = ?, lowest_price = ?, highest_price = ?, target_price = ?, \
             in_stock = ?, status = ?, failures = ?, every_hours = ?, notify = ?, collection = ?, \
             checked_at = ?, next_check_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&item.title)
        .bind(&item.site)
        .bind(&item.picture_key)
        .bind(&item.currency)
        .bind(item.price)
        .bind(item.previous)
        .bind(item.lowest)
        .bind(item.highest)
        .bind(item.target)
        .bind(item.in_stock.map(i64::from))
        .bind(item.status.as_str())
        .bind(i64::from(item.failures))
        .bind(i64::from(item.every_hours))
        .bind(item.notify.as_str())
        .bind(&item.collection)
        .bind(item.checked_at.map(|t| t.to_rfc3339()))
        .bind(item.next_check_at.map(|t| t.to_rfc3339()))
        .bind(item.updated_at.to_rfc3339())
        .bind(&item.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn add_price(&self, id: &str, point: &PricePoint) -> Result<()> {
        sqlx::query("INSERT INTO cart_prices (cart_id, at, price, in_stock) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(point.at.to_rfc3339())
            .bind(point.price)
            .bind(point.in_stock.map(i64::from))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn history(&self, id: &str, limit: usize) -> Result<Vec<PricePoint>> {
        let rows = sqlx::query(
            "SELECT at, price, in_stock FROM cart_prices WHERE cart_id = ? \
             ORDER BY at DESC, rowid DESC LIMIT ?",
        )
        .bind(id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut points = rows
            .iter()
            .map(|row| {
                Ok(PricePoint {
                    at: time(&row.try_get::<String, _>("at")?)?,
                    price: row.try_get("price")?,
                    in_stock: row.try_get::<Option<i64>, _>("in_stock")?.map(|v| v != 0),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        points.reverse();
        Ok(points)
    }

    async fn clear_history(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM cart_prices WHERE cart_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete(&self, owner: &str, id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let found = sqlx::query("SELECT 1 FROM cart_items WHERE id = ? AND profile_id = ?")
            .bind(id)
            .bind(owner)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
        if found {
            sqlx::query("DELETE FROM cart_prices WHERE cart_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM cart_items WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use chrono::Duration;

    async fn repo_with_member() -> (SqliteCartRepository, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::init(dir.path()).await.unwrap();
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('jerry', 'Jerry')")
            .execute(&db.system)
            .await
            .unwrap();
        (SqliteCartRepository::new(db.system.clone()), dir)
    }

    fn kettle(now: DateTime<Utc>) -> CartItem {
        CartItem {
            id: "c1".into(),
            owner: "jerry".into(),
            url: "https://www.jumia.co.ke/kettle-177871891.html".into(),
            site: "jumia.co.ke".into(),
            every_hours: 6,
            next_check_at: Some(now),
            created_at: now,
            updated_at: now,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn an_item_round_trips_and_is_its_owners_alone() {
        let (repo, _dir) = repo_with_member().await;
        let now = Utc::now();
        assert!(repo.create(&kettle(now)).await.unwrap());
        assert!(
            !repo.create(&kettle(now)).await.unwrap(),
            "same address twice"
        );
        let mut item = repo.get("jerry", "c1").await.unwrap().unwrap();
        assert_eq!(item.status, CartStatus::Checking);
        assert!(repo.get("emmanuel", "c1").await.unwrap().is_none());
        item.title = "Ramtons kettle".into();
        item.price = Some(1799.0);
        item.currency = Some("KES".into());
        item.in_stock = Some(true);
        item.target = Some(1500.0);
        item.notify = Notify::Target;
        item.status = CartStatus::Ok;
        item.checked_at = Some(now);
        item.next_check_at = Some(now + Duration::hours(6));
        repo.save(&item).await.unwrap();
        let back = repo.get("jerry", "c1").await.unwrap().unwrap();
        assert_eq!(back.price, Some(1799.0));
        assert_eq!(back.in_stock, Some(true));
        assert_eq!(back.notify, Notify::Target);
        assert!(repo.due(now, 8).await.unwrap().is_empty());
        assert_eq!(
            repo.due(now + Duration::hours(7), 8).await.unwrap().len(),
            1
        );
        for (i, price) in [1799.0, 1599.0].into_iter().enumerate() {
            repo.add_price(
                "c1",
                &PricePoint {
                    at: now + Duration::hours(i as i64),
                    price,
                    in_stock: Some(true),
                },
            )
            .await
            .unwrap();
        }
        let history = repo.history("c1", 60).await.unwrap();
        assert_eq!(
            history.iter().map(|p| p.price).collect::<Vec<_>>(),
            vec![1799.0, 1599.0]
        );
        assert!(!repo.delete("emmanuel", "c1").await.unwrap());
        assert!(repo.delete("jerry", "c1").await.unwrap());
        assert!(repo.history("c1", 60).await.unwrap().is_empty());
    }
}
