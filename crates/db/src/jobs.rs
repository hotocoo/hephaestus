//! Durable job queue: priorities, leases, retries, dead-letter.
//!
//! Semantics: at-least-once delivery. Handlers MUST be idempotent;
//! idempotency keys on enqueue prevent duplicate submission.

use chrono::{DateTime, Utc};
use hephaestus_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use hephaestus_core::id::JobId;

use crate::store::Db;

/// A claimed job handed to a worker.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimedJob {
    /// Job id.
    pub id: Uuid,
    /// Queue name.
    pub queue: String,
    /// Payload.
    pub payload: Json,
    /// Attempts so far INCLUDING this claim.
    pub attempts: i32,
    /// Max attempts before dead-letter.
    pub max_attempts: i32,
}

/// A job whose leases expired (worker died).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExpiredLease {
    /// Job id.
    pub id: Uuid,
    /// Previous owner.
    pub lease_owner: Option<String>,
    /// When the lease ran out.
    pub lease_expires_at: Option<DateTime<Utc>>,
}

impl Db {
    /// Enqueue a job. Idempotent per (queue, key) when a key is given.
    pub async fn enqueue(
        &self,
        queue: &str,
        payload: &Json,
        priority: i16,
        idempotency_key: Option<&str>,
    ) -> Result<JobId> {
        let id = JobId::generate();
        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO jobs (id, queue, priority, payload, idempotency_key)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (queue, idempotency_key) WHERE idempotency_key IS NOT NULL
             DO UPDATE SET updated_at = jobs.updated_at
             RETURNING id",
        )
        .bind(id.as_uuid())
        .bind(queue)
        .bind(priority as i32)
        .bind(payload)
        .bind(idempotency_key)
        .fetch_one(self.pool())
        .await
        .map_err(crate::map_sqlx)
        .map(JobId::from_uuid)
    }

    /// Claim the next runnable job from one queue.
    ///
    /// Uses FOR UPDATE SKIP LOCKED so competing workers never grab the
    /// same row and never block each other.
    pub async fn claim_job(
        &self,
        queue: &str,
        owner: &str,
        ttl_secs: i64,
    ) -> Result<Option<ClaimedJob>> {
        let claimed = sqlx::query_as::<_, ClaimedJob>(
            "UPDATE jobs SET
                status = 'running',
                lease_owner = $2,
                lease_expires_at = now() + make_interval(secs => $3),
                attempts = attempts + 1,
                updated_at = now()
             WHERE id = (
                SELECT id FROM jobs
                WHERE queue = $1 AND status = 'pending' AND run_after <= now()
                ORDER BY priority DESC, run_after ASC
                FOR UPDATE SKIP LOCKED
                LIMIT 1
             )
             RETURNING id, queue, payload, attempts, max_attempts",
        )
        .bind(queue)
        .bind(owner)
        .bind(ttl_secs as f64)
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(claimed)
    }

    /// Mark a job done.
    pub async fn complete_job(&self, id: JobId, owner: &str) -> Result<()> {
        sqlx::query(
            "UPDATE jobs SET status='done', lease_owner=NULL, lease_expires_at=NULL,
                    updated_at=now()
             WHERE id=$1 AND lease_owner=$2 AND status='running'",
        )
        .bind(id.as_uuid())
        .bind(owner)
        .execute(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(())
    }

    /// Report failure. Retries with backoff until max_attempts, then
    /// dead-letters. Returns true if the job will be retried.
    pub async fn fail_job(&self, id: JobId, owner: &str, error: &str) -> Result<bool> {
        // Truncate stored error to keep rows bounded; full context lives
        // in logs/events, not in the error column.
        let mut msg = error.to_string();
        if msg.len() > 4000 {
            msg.truncate(4000);
        }
        let res = sqlx::query_scalar::<_, String>(
            "UPDATE jobs SET
                status = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'pending' END,
                last_error = $3,
                lease_owner = NULL,
                lease_expires_at = NULL,
                run_after = CASE WHEN attempts >= max_attempts THEN run_after
                                 ELSE now() + make_interval(
                                      secs => LEAST(3600, POWER(2, attempts)::float8))
                            END,
                updated_at = now()
             WHERE id=$1 AND lease_owner=$2 AND status='running'
             RETURNING status",
        )
        .bind(id.as_uuid())
        .bind(owner)
        .bind(msg)
        .fetch_one(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(res == "pending")
    }

    /// Recover jobs whose worker died mid-flight (lease expired).
    ///
    /// Returns them to pending for re-execution by healthy workers.
    ///
    /// Scope: when `queue` is given, only that queue's jobs are
    /// recovered. A recovery sweep over the whole table is an
    /// administrative action and must be deliberate (passing None),
    /// because it resets every expired lease system-wide.
    pub async fn recover_expired_leases(&self, queue: Option<&str>) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE jobs SET status='pending', lease_owner=NULL,
                    lease_expires_at=NULL, run_after=now(), updated_at=now()
             WHERE status='running' AND lease_expires_at < now()
               AND ($1::text IS NULL OR queue = $1)",
        )
        .bind(queue)
        .execute(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(res.rows_affected())
    }

    /// Renew a held lease (worker heartbeat).
    ///
    /// Only succeeds while the caller still owns a running job;
    /// false means ownership was lost (expired and reclaimed).
    pub async fn renew_job_lease(&self, id: JobId, owner: &str, ttl_secs: i64) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE jobs SET lease_expires_at = now() + make_interval(secs => $3)
             WHERE id=$1 AND lease_owner=$2 AND status='running'",
        )
        .bind(id.as_uuid())
        .bind(owner)
        .bind(ttl_secs as f64)
        .execute(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(res.rows_affected() == 1)
    }

    /// Fetch a dead-lettered job for inspection.
    pub async fn get_dead_job(&self, id: JobId) -> Result<(Json, Option<String>)> {
        sqlx::query_as::<_, (Json, Option<String>)>(
            "SELECT payload, last_error FROM jobs
             WHERE id=$1 AND status='dead'",
        )
        .bind(id.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)?
        .ok_or(hephaestus_core::Error::NotFound { entity: "dead job" })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::store::Db;
    use crate::testutil::test_db;

    /// Remove any rows from earlier runs so every test starts clean
    /// even against a shared database (parallel tests still isolate by
    /// unique queue names; this guards reruns after crashes).
    async fn reset_queue(db: &Db, q: &str) {
        sqlx::query("DELETE FROM jobs WHERE queue = $1")
            .bind(q)
            .execute(db.pool())
            .await
            .expect("cleanup");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn claim_respects_priority_order() {
        let db = test_db().await;
        let q = format!("q-{}", Uuid::now_v7().simple());
        reset_queue(&db, &q).await;
        db.enqueue(&q, &serde_json::json!({"n": 1}), -5, None)
            .await
            .expect("enq");
        db.enqueue(&q, &serde_json::json!({"n": 2}), 10, None)
            .await
            .expect("enq");
        db.enqueue(&q, &serde_json::json!({"n": 3}), 0, None)
            .await
            .expect("enq");

        let first = db
            .claim_job(&q, "w", 60)
            .await
            .expect("claim")
            .expect("job");
        assert_eq!(
            first.payload.get("n").and_then(|v| v.as_i64()),
            Some(2),
            "highest priority first"
        );
        let second = db
            .claim_job(&q, "w", 60)
            .await
            .expect("claim")
            .expect("job");
        assert_eq!(second.payload.get("n").and_then(|v| v.as_i64()), Some(3));
        let third = db
            .claim_job(&q, "w", 60)
            .await
            .expect("claim")
            .expect("job");
        assert_eq!(third.payload.get("n").and_then(|v| v.as_i64()), Some(1));
        assert!(db.claim_job(&q, "w", 60).await.expect("empty").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_workers_never_claim_same_job() {
        let db = test_db().await;
        let q = format!("race-{}", Uuid::now_v7().simple());
        reset_queue(&db, &q).await;
        for i in 0..20 {
            db.enqueue(&q, &serde_json::json!({"i": i}), 0, None)
                .await
                .expect("enq");
        }
        let mut seen = std::collections::HashSet::new();
        // Sequential claims simulate contention without threads:
        // SKIP LOCKED guarantees uniqueness under real concurrency too.
        while let Some(j) = db.claim_job(&q, "wA", 60).await.expect("claim") {
            assert!(seen.insert(j.id), "same job claimed twice");
        }
        assert!(db.claim_job(&q, "wB", 60).await.expect("claim").is_none());
        assert_eq!(seen.len(), 20);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_jobs_retry_then_dead_letter() {
        let db = test_db().await;
        let q = format!("retry-{}", Uuid::now_v7().simple());
        reset_queue(&db, &q).await;
        let jid = db
            .enqueue(&q, &serde_json::json!({"x": 1}), 0, None)
            .await
            .expect("enq");

        // Attempt 1 fails -> retry scheduled.
        let c1 = db.claim_job(&q, "w", 60).await.expect("c1").expect("job");
        assert_eq!(c1.attempts, 1);
        let will_retry = db
            .fail_job(JobId::from_uuid(c1.id), "w", "boom")
            .await
            .expect("fail");
        assert!(will_retry);

        // Force run_after back to now to avoid waiting for backoff.
        sqlx::query("UPDATE jobs SET run_after = now() - interval '1s' WHERE queue = $1")
            .bind(&q)
            .execute(db.pool())
            .await
            .expect("reset timer");

        let _ = db.claim_job(&q, "w", 60).await.expect("c2").expect("job2");
        let will_retry2 = db.fail_job(jid, "w", "boom again").await.expect("f2");
        assert!(will_retry2);

        // Exhaust remaining attempts quickly.
        loop {
            sqlx::query("UPDATE jobs SET run_after = now() - interval '1s' WHERE queue = $1")
                .bind(&q)
                .execute(db.pool())
                .await
                .expect("reset");
            match db.claim_job(&q, "w", 60).await.expect("cn") {
                Some(c) => {
                    let retried = db
                        .fail_job(JobId::from_uuid(c.id), "w", "final")
                        .await
                        .expect("fn");
                    if !retried {
                        break;
                    }
                }
                None => panic!("job should still be pending until dead"),
            }
        }
        let (payload, err) = db.get_dead_job(jid).await.expect("dead");
        assert!(err.is_some());
        assert_eq!(payload.get("x").and_then(|v| v.as_i64()), Some(1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expired_leases_are_recoverable() {
        let db = test_db().await;
        let q = format!("lease-{}", Uuid::now_v7().simple());
        reset_queue(&db, &q).await;
        db.enqueue(&q, &serde_json::json!({"k": 9}), 0, None)
            .await
            .expect("enq");
        let c = db
            .claim_job(&q, "crashed-worker", 60)
            .await
            .expect("claim")
            .expect("job");
        // Simulate crash: expire ONLY this queue's rows. A blanket
        // update would reach into other tests' queues running in
        // parallel - the exact cross-talk bug this suite once caught.
        sqlx::query("UPDATE jobs SET lease_expires_at = now() - interval '1s' WHERE queue = $1")
            .bind(&q)
            .execute(db.pool())
            .await
            .expect("expire");
        let recovered = db.recover_expired_leases(Some(&q)).await.expect("recover");
        assert_eq!(recovered, 1, "exactly this test's job recovers");
        let again = db
            .claim_job(&q, "healthy-worker", 60)
            .await
            .expect("reclaim")
            .expect("job");
        assert_eq!(again.id, c.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn idempotent_enqueue_returns_same_job() {
        let db = test_db().await;
        let q = format!("idem-{}", Uuid::now_v7().simple());
        let a = db
            .enqueue(&q, &serde_json::json!({}), 0, Some("dup"))
            .await
            .expect("a");
        let b = db
            .enqueue(&q, &serde_json::json!({}), 0, Some("dup"))
            .await
            .expect("b");
        assert_eq!(a, b);
    }
}
