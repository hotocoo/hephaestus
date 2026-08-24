//! Shared integration-test helpers.
//!
//! Tests run against a real PostgreSQL database. Set
//! HEPHAESTUS_TEST_DATABASE_URL (e.g. postgres://localhost/hephaestus_test).
//! Missing configuration fails the test loudly - never silently skips,
//! so CI cannot degrade into testing nothing.

#![allow(dead_code)]
#![allow(clippy::panic)]

use std::sync::Once;

use crate::Db;

static MIGRATE_ONCE: Once = Once::new();

/// Test database URL from the environment.
pub fn test_url() -> String {
    std::env::var("HEPHAESTUS_TEST_DATABASE_URL").unwrap_or_else(|_| {
        panic!(
            "HEPHAESTUS_TEST_DATABASE_URL must be set for database tests              (e.g. postgres://localhost/hephaestus_test)"
        )
    })
}

/// Connect and ensure migrations ran exactly once for this process.
pub async fn test_db() -> Db {
    let url = test_url();
    let db = Db::connect(&url)
        .await
        .unwrap_or_else(|e| panic!("cannot connect to test database: {e}"));
    MIGRATE_ONCE.call_once(|| {
        // Migration execution needs a blocking-free runtime: we are
        // inside tokio already; block_in_place keeps nested awaits safe.
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                db.migrate()
                    .await
                    .unwrap_or_else(|e| panic!("migrations failed: {e}"));
            });
        });
    });
    db
}
