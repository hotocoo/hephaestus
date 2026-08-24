//! Database handle: pooling and migrations.

use std::path::Path;
use std::str::FromStr;

use hephaestus_core::Result;
use sqlx::Executor;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

/// Handle to the Hephaestus PostgreSQL database.
///
/// Cloneable; the inner pool is shared.
#[derive(Debug, Clone)]
pub struct Db {
    pool: PgPool,
}

impl Db {
    /// Open a pooled connection.
    pub async fn connect(url: &str) -> Result<Self> {
        let opts = PgConnectOptions::from_str(url)
            .map_err(|e| hephaestus_core::Error::Config(format!("invalid database URL: {e}")))?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect_with(opts)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "database connect failed");
                hephaestus_core::Error::Storage(Box::new(e))
            })?;
        Ok(Self { pool })
    }

    /// Connect with an explicit max pool size (used by tests/workers).
    pub async fn connect_with_max(url: &str, max: u32) -> Result<Self> {
        let opts = PgConnectOptions::from_str(url)
            .map_err(|e| hephaestus_core::Error::Config(format!("invalid database URL: {e}")))?;
        let pool = PgPoolOptions::new()
            .max_connections(max)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect_with(opts)
            .await
            .map_err(|e| hephaestus_core::Error::Storage(Box::new(e)))?;
        Ok(Self { pool })
    }

    /// Apply pending migrations from the bundled migrations directory.
    ///
    /// Migration files live in the repository under migrations/ and are
    /// embedded at build time; runtime never mutates schema outside of
    /// this versioned path.
    pub async fn migrate(&self) -> Result<()> {
        // Relative to this crate: ../../migrations
        let manifest = std::fs::canonicalize(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations"),
        )
        .map_err(|e| hephaestus_core::Error::Config(format!("migrations dir missing: {e}")))?;
        let migrator = Migrator::new(manifest)
            .await
            .map_err(|e| hephaestus_core::Error::Storage(Box::new(e)))?;
        migrator
            .run(&self.pool)
            .await
            .map_err(|e| hephaestus_core::Error::Storage(Box::new(e)))?;
        Ok(())
    }

    /// Current applied migration version, if any.
    pub async fn migration_version(&self) -> Result<Option<(i64, String)>> {
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT version, description FROM _sqlx_migrations              ORDER BY version DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::map_sqlx)?;
        Ok(row)
    }

    /// Expose the pool for stores in this crate.
    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Run a bare statement (used by health checks).
    pub async fn ping(&self) -> Result<()> {
        self.pool
            .execute(sqlx::query("SELECT 1"))
            .await
            .map_err(crate::map_sqlx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use crate::testutil::test_db;

    #[tokio::test(flavor = "multi_thread")]
    async fn ping_and_migrations_work() {
        let db = test_db().await;
        db.ping().await.expect("ping");
        let v = db.migration_version().await.expect("version query");
        assert!(v.is_some(), "migrations must have run");
        assert_eq!(v.unwrap_or((0, String::new())).1, "initial schema");
    }
}
