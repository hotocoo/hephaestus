//! Atomic task intake.
//!
//! Task row + initial workflow run + first queue job + creation event
//! are written in ONE transaction: a task can never exist without its
//! run, and a run never exists without schedulable work. Idempotency
//! keys make client retries safe under concurrency (unique index is
//! the arbiter, not check-then-insert).

use chrono::Utc;
use hephaestus_core::Result;
use uuid::Uuid;

use crate::store::Db;

/// Everything needed to bootstrap a task atomically.
#[derive(Debug, Clone)]
pub struct IntakeCommand {
    /// Tenant scope.
    pub organization_id: Uuid,
    /// Project scope.
    pub project_id: Uuid,
    /// Repository scope.
    pub repository_id: Uuid,
    /// Pre-validated title (engine validates domain rules first).
    pub title: String,
    /// Description (untrusted input; provenance handled downstream).
    pub description: String,
    /// Priority value from the closed set.
    pub priority: String,
    /// Risk value from the closed set.
    pub risk: String,
    /// Labels (normalized).
    pub labels: Vec<String>,
    /// Optional idempotency key.
    pub idempotency_key: Option<String>,
    /// Queue name for the bootstrap job.
    pub first_queue: String,
    /// Queue priority (-100..=100).
    pub first_priority: i16,
    /// Serialized job payload envelope (schema-versioned).
    ///
    /// Identifiers inside `payload.data` are PLACEHOLDERS: this layer
    /// overwrites task_id/run_id with the real values it mints, so
    /// queued work always references persisted rows.
    pub first_payload: serde_json::Value,
}

/// Receipt of an accepted task.
#[derive(Debug, Clone)]
pub struct IntakeRecord {
    /// Task id (pre-existing when deduplicated).
    pub task_id: Uuid,
    /// Run id (pre-existing when deduplicated).
    pub run_id: Uuid,
    /// True when an existing task matched the idempotency key.
    pub deduplicated: bool,
}

impl Db {
    /// Execute atomic intake. See module docs for guarantees.
    pub async fn intake_task(&self, cmd: &IntakeCommand) -> Result<IntakeRecord> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        // Fast path: same idempotency key -> return existing identity
        // without creating duplicates. Unique index still arbitrates
        // concurrent inserts below.
        if let Some(key) = &cmd.idempotency_key {
            let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
                "SELECT t.id, (SELECT r.id FROM workflow_runs r WHERE r.task_id = t.id LIMIT 1)
                 FROM tasks t
                 WHERE t.organization_id = $1 AND t.idempotency_key = $2",
            )
            .bind(cmd.organization_id)
            .bind(key)
            .fetch_optional(&mut *tx)
            .await
            .map_err(crate::map_sqlx)?;
            if let Some((task_id, Some(run_id))) = row {
                tx.commit().await.map_err(crate::map_sqlx)?;
                return Ok(IntakeRecord {
                    task_id,
                    run_id,
                    deduplicated: true,
                });
            }
            if let Some((task_id, None)) = row {
                // Repair path: task exists but no run (should not happen
                // post-this-migration; create run+job only).
                let run_id = self
                    .create_run_and_job_for(
                        &mut tx,
                        cmd.organization_id,
                        task_id,
                        &cmd.first_queue,
                        cmd.first_priority,
                        &cmd.first_payload,
                    )
                    .await?;
                tx.commit().await.map_err(crate::map_sqlx)?;
                return Ok(IntakeRecord {
                    task_id,
                    run_id,
                    deduplicated: true,
                });
            }
        }

        let task_id = Uuid::now_v7();
        let created_at = Utc::now();
        sqlx::query(
            "INSERT INTO tasks
               (id, organization_id, project_id, repository_id,
                title, description, priority, risk, labels, idempotency_key, created_at, updated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$11)",
        )
        .bind(task_id)
        .bind(cmd.organization_id)
        .bind(cmd.project_id)
        .bind(cmd.repository_id)
        .bind(&cmd.title)
        .bind(&cmd.description)
        .bind(&cmd.priority)
        .bind(&cmd.risk)
        .bind(sqlx::types::Json(&cmd.labels))
        .bind(cmd.idempotency_key.as_deref())
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        let run_id = self
            .create_run_and_job_for(
                &mut tx,
                cmd.organization_id,
                task_id,
                &cmd.first_queue,
                cmd.first_priority,
                &cmd.first_payload,
            )
            .await?;

        // Creation event rides the same transaction.
        let event = serde_json::json!({
            "type": "task_created",
            "data": { "task_id": task_id }
        });
        sqlx::query(
            "INSERT INTO events
               (id, schema_version, organization_id, aggregate, aggregate_id,
                correlation_id, provenance, payload)
             VALUES ($1, 1, $2, 'task', $3, $4, 'system', $5)",
        )
        .bind(Uuid::now_v7())
        .bind(cmd.organization_id)
        .bind(task_id)
        .bind(run_id) // correlation: run id doubles here as correlation seed
        .bind(event)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(IntakeRecord {
            task_id,
            run_id,
            deduplicated: false,
        })
    }

    async fn create_run_and_job_for(
        &self,
        tx: &mut sqlx::PgConnection,
        organization_id: Uuid,
        task_id: Uuid,
        queue: &str,
        priority: i16,
        payload: &serde_json::Value,
    ) -> Result<Uuid> {
        let run_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO workflow_runs (id, task_id, organization_id, correlation_id)
             VALUES ($1,$2,$3,$1)",
        )
        .bind(run_id)
        .bind(task_id)
        .bind(organization_id)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        // Bind real identifiers into the payload envelope. Callers
        // build the envelope with placeholder ids because only this
        // layer knows them at insert time; the convention (and its
        // enforcement point) is documented on IntakeCommand.
        let mut bound = payload.clone();
        bound["payload"]["data"]["task_id"] = serde_json::json!(task_id);
        bound["payload"]["data"]["run_id"] = serde_json::json!(run_id);
        sqlx::query(
            "INSERT INTO jobs (id, queue, priority, payload, idempotency_key)
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(queue)
        .bind(priority as i32)
        .bind(bound)
        .bind(format!("bootstrap-{}", task_id))
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        Ok(run_id)
    }
}
