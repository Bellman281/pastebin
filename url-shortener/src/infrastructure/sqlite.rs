//! Production [`LinkRepository`] backed by SQLite via `sqlx`.
//!
//! Memory & lifecycle notes: a single bounded [`SqlitePool`] owns all
//! connections; `max_connections` caps concurrent connection memory. The pool
//! is cloned cheaply (an `Arc` internally) and is drained on drop, so there are
//! no leaked connections. Queries use the runtime API (not the compile-time
//! `query!` macros) so the build needs no live database or offline metadata.

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use std::time::Duration;

use crate::domain::{BoxedError, Link, LinkRepository, RepoError, ShortCode, TargetUrl};

/// SQLite-backed link repository.
#[derive(Clone)]
pub struct SqliteLinkRepository {
    pool: SqlitePool,
}

impl SqliteLinkRepository {
    /// Open (creating the file if needed) a bounded pool and run migrations.
    ///
    /// Concurrency tuning applied to every connection:
    /// - **WAL** journal mode lets many readers run concurrently with a single
    ///   writer (the default rollback journal blocks readers during writes).
    /// - **`synchronous = NORMAL`** is the safe, recommended pairing with WAL:
    ///   durable across app crashes, far fewer fsyncs than `FULL`.
    /// - **`busy_timeout`** makes a contended writer wait briefly instead of
    ///   failing immediately with `SQLITE_BUSY`.
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

    /// Build a repository from an already-configured pool (useful in tests).
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn backend(err: impl std::error::Error + Send + Sync + 'static) -> RepoError {
    RepoError::Backend(Box::new(err))
}

#[async_trait::async_trait]
impl LinkRepository for SqliteLinkRepository {
    async fn insert(&self, link: &Link) -> Result<(), RepoError> {
        let result = sqlx::query(
            "INSERT INTO links (code, target, created_at, hits) VALUES (?, ?, ?, ?)",
        )
        .bind(link.code.as_str())
        .bind(link.target.as_str())
        .bind(link.created_at)
        .bind(link.hits)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Err(RepoError::Conflict),
            Err(err) => Err(backend(err)),
        }
    }

    async fn get(&self, code: &ShortCode) -> Result<Option<Link>, RepoError> {
        let row = sqlx::query("SELECT code, target, created_at, hits FROM links WHERE code = ?")
            .bind(code.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;

        let Some(row) = row else { return Ok(None) };

        // Values were validated before insertion, so reconstruct without
        // re-running (potentially failing) validation.
        let code: String = row.try_get("code").map_err(backend)?;
        let target: String = row.try_get("target").map_err(backend)?;
        let created_at: i64 = row.try_get("created_at").map_err(backend)?;
        let hits: i64 = row.try_get("hits").map_err(backend)?;

        Ok(Some(Link {
            code: ShortCode::from_trusted(code),
            target: TargetUrl::from_trusted(target),
            created_at,
            hits,
        }))
    }

    async fn increment_hits(&self, code: &ShortCode) -> Result<bool, RepoError> {
        let result = sqlx::query("UPDATE links SET hits = hits + 1 WHERE code = ?")
            .bind(code.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete(&self, code: &ShortCode) -> Result<bool, RepoError> {
        let result = sqlx::query("DELETE FROM links WHERE code = ?")
            .bind(code.as_str())
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

    // A shared in-memory DB requires a single connection, otherwise each pooled
    // connection would see its own empty database.
    async fn repo() -> SqliteLinkRepository {
        SqliteLinkRepository::connect("sqlite::memory:", 1)
            .await
            .expect("connect + migrate")
    }

    fn sample() -> Link {
        Link::new(
            ShortCode::parse("abc123").unwrap(),
            TargetUrl::parse("https://example.com/x").unwrap(),
            1_700_000_000,
        )
    }

    #[tokio::test]
    async fn insert_get_increment_delete() {
        let repo = repo().await;
        let code = ShortCode::parse("abc123").unwrap();

        repo.insert(&sample()).await.unwrap();
        assert!(matches!(repo.insert(&sample()).await, Err(RepoError::Conflict)));

        let fetched = repo.get(&code).await.unwrap().unwrap();
        assert_eq!(fetched.target.as_str(), "https://example.com/x");
        assert_eq!(fetched.hits, 0);

        assert!(repo.increment_hits(&code).await.unwrap());
        assert_eq!(repo.get(&code).await.unwrap().unwrap().hits, 1);

        assert!(repo.delete(&code).await.unwrap());
        assert!(repo.get(&code).await.unwrap().is_none());
        assert!(!repo.delete(&code).await.unwrap());
    }
}
