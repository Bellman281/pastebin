//! Production [`PasteRepository`] backed by SQLite via `sqlx`.
//!
//! A single bounded [`SqlitePool`] owns all connections (`max_connections` caps
//! connection memory); it drains on drop, so no connections leak. Queries use
//! the runtime API (not the compile-time `query!` macros) so the build needs no
//! live database. WAL + a busy timeout improve read/write concurrency.

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use std::time::Duration;

use crate::domain::{BoxedError, Content, Paste, PasteId, PasteRepository, RepoError};

/// SQLite-backed paste repository.
#[derive(Clone)]
pub struct SqlitePasteRepository {
    pool: SqlitePool,
}

impl SqlitePasteRepository {
    /// Open (creating the file if needed) a bounded pool and run migrations.
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, BoxedError> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(Self { pool })
    }

    /// Build from an already-configured pool (useful in tests).
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn backend(err: impl std::error::Error + Send + Sync + 'static) -> RepoError {
    RepoError::Backend(Box::new(err))
}

#[async_trait::async_trait]
impl PasteRepository for SqlitePasteRepository {
    async fn insert(&self, paste: &Paste) -> Result<(), RepoError> {
        let result = sqlx::query(
            "INSERT INTO pastes (id, content, syntax, created_at, expires_at, one_shot, views) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(paste.id.as_str())
        .bind(paste.content.as_str())
        .bind(paste.syntax.as_deref())
        .bind(paste.created_at)
        .bind(paste.expires_at)
        .bind(paste.one_shot)
        .bind(paste.views)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Err(RepoError::Conflict),
            Err(err) => Err(backend(err)),
        }
    }

    async fn get(&self, id: &PasteId) -> Result<Option<Paste>, RepoError> {
        let row = sqlx::query(
            "SELECT id, content, syntax, created_at, expires_at, one_shot, views \
             FROM pastes WHERE id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;

        let Some(row) = row else { return Ok(None) };

        // Values were validated before insertion; reconstruct without re-running
        // (potentially failing) validation.
        let id: String = row.try_get("id").map_err(backend)?;
        let content: String = row.try_get("content").map_err(backend)?;
        let syntax: Option<String> = row.try_get("syntax").map_err(backend)?;
        let created_at: i64 = row.try_get("created_at").map_err(backend)?;
        let expires_at: Option<i64> = row.try_get("expires_at").map_err(backend)?;
        let one_shot: bool = row.try_get("one_shot").map_err(backend)?;
        let views: i64 = row.try_get("views").map_err(backend)?;

        Ok(Some(Paste {
            id: PasteId::from_trusted(id),
            content: Content::from_trusted(content),
            syntax,
            created_at,
            expires_at,
            one_shot,
            views,
        }))
    }

    async fn increment_views_by(&self, id: &PasteId, n: i64) -> Result<bool, RepoError> {
        let result = sqlx::query("UPDATE pastes SET views = views + ? WHERE id = ?")
            .bind(n)
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete(&self, id: &PasteId) -> Result<bool, RepoError> {
        let result = sqlx::query("DELETE FROM pastes WHERE id = ?")
            .bind(id.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(result.rows_affected() > 0)
    }

    async fn ping(&self) -> Result<(), RepoError> {
        sqlx::query("SELECT 1").execute(&self.pool).await.map_err(backend)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A shared in-memory DB needs a single connection, else each pooled
    // connection sees its own empty database.
    async fn repo() -> SqlitePasteRepository {
        SqlitePasteRepository::connect("sqlite::memory:", 1)
            .await
            .expect("connect + migrate")
    }

    fn sample(one_shot: bool, expires_at: Option<i64>) -> Paste {
        Paste::new(
            PasteId::parse("abc123").unwrap(),
            Content::parse("body text").unwrap(),
            Some("rust".to_owned()),
            1_700_000_000,
            expires_at,
            one_shot,
        )
    }

    #[tokio::test]
    async fn insert_get_increment_delete_roundtrip() {
        let repo = repo().await;
        let id = PasteId::parse("abc123").unwrap();

        repo.insert(&sample(false, None)).await.unwrap();
        let fetched = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.content.as_str(), "body text");
        assert_eq!(fetched.syntax.as_deref(), Some("rust"));
        assert_eq!(fetched.views, 0);

        assert!(repo.increment_views(&id).await.unwrap());
        assert_eq!(repo.get(&id).await.unwrap().unwrap().views, 1);

        // Batched increment writes `views = views + n` in a single statement.
        assert!(repo.increment_views_by(&id, 9).await.unwrap());
        assert_eq!(repo.get(&id).await.unwrap().unwrap().views, 10);

        assert!(repo.delete(&id).await.unwrap());
        assert!(repo.get(&id).await.unwrap().is_none());
        assert!(!repo.delete(&id).await.unwrap());
    }

    #[tokio::test]
    async fn duplicate_id_conflicts() {
        let repo = repo().await;
        repo.insert(&sample(false, None)).await.unwrap();
        assert!(matches!(
            repo.insert(&sample(false, None)).await,
            Err(RepoError::Conflict)
        ));
    }

    #[tokio::test]
    async fn one_shot_and_expiry_round_trip() {
        let repo = repo().await;
        let id = PasteId::parse("abc123").unwrap();
        repo.insert(&sample(true, Some(1_700_009_999))).await.unwrap();
        let p = repo.get(&id).await.unwrap().unwrap();
        assert!(p.one_shot);
        assert_eq!(p.expires_at, Some(1_700_009_999));
    }

    #[tokio::test]
    async fn ping_ok() {
        assert!(repo().await.ping().await.is_ok());
    }
}
