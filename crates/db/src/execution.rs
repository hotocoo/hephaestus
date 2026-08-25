//! Execution persistence: attempts over approved plans, per-step
//! progress snapshots, and verification evidence.
//!
//! All queries are tenant-scoped through `organization_id` columns or
//! joins into scoped tables. State-affecting writes append their event
//! in the SAME transaction, so a crash never leaves data without its
//! audit trail. The fix-loop budget lives HERE as a count of failed
//! verification rows - the ledger is the data, not a separate counter
//! that could drift from it (ADR-007).

use chrono::{DateTime, Utc};
use hephaestus_core::id::{
    ExecutionId, OrganizationId, StepId, TaskId, VerificationId, WorkflowRunId,
};
use hephaestus_core::{Error, Result};
use uuid::Uuid;

use crate::store::Db;

/// A persisted execution attempt.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExecutionRow {
    /// Execution id.
    pub id: Uuid,
    /// Owning task.
    pub task_id: Uuid,
    /// Driving workflow run.
    pub run_id: Uuid,
    /// The approved plan being executed.
    pub plan_id: Uuid,
    /// "active", "passed" or "failed".
    pub status: String,
}

/// One snapshotted plan step and its progress inside an execution.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExecutionStepRow {
    /// Progress-row id (distinct from the plan step id).
    pub id: Uuid,
    /// The originating plan step.
    pub step_id: Uuid,
    /// Owning execution.
    pub execution_id: Uuid,
    /// Execution order (copied from the plan).
    pub position: i32,
    /// Action text snapshot.
    pub action: String,
    /// Verification reference snapshot.
    pub verification: String,
    /// "pending", "completed" or "failed".
    pub status: String,
    /// Implementer summary once finished.
    pub summary: String,
    /// Workspace-relative paths the implementer reported changing.
    pub files_changed: serde_json::Value,
}

/// One recorded verification suite run.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct VerificationRow {
    /// Verification id.
    pub id: Uuid,
    /// Owning execution.
    pub execution_id: Uuid,
    /// 1-based suite index for this execution.
    pub cycle: i32,
    /// Whether every required layer passed.
    pub passed: bool,
    /// Serialized [hephaestus_verify::VerificationReport].
    pub report: serde_json::Value,
    /// When the suite ran.
    pub created_at: DateTime<Utc>,
}

fn step_event_payload(outcome: &str, exec: ExecutionId, step: StepId) -> Result<serde_json::Value> {
    serde_json::to_value(hephaestus_core::event::EventPayload::StepExecuted {
        execution_id: hephaestus_core::id::HephaestusId(exec.as_uuid()),
        step_id: hephaestus_core::id::HephaestusId(step.as_uuid()),
        outcome: outcome.to_string(),
    })
    .map_err(|e| Error::Storage(Box::new(e)))
}

/// Append one execution-scoped event inside an existing transaction,
/// borrowing the driving run's correlation id for trace continuity.
async fn write_execution_event_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: OrganizationId,
    exec: ExecutionId,
    run: WorkflowRunId,
    provenance: &str,
    payload: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO events
           (id, schema_version, organization_id, aggregate, aggregate_id,
            correlation_id, provenance, payload)
         SELECT $1, $2, $3, 'execution', $4, correlation_id, $6, $7
         FROM workflow_runs WHERE id = $5",
    )
    .bind(Uuid::now_v7())
    .bind(i32::try_from(hephaestus_core::event::EVENT_ENVELOPE_VERSION).unwrap_or(i32::MAX))
    .bind(org.as_uuid())
    .bind(exec.as_uuid())
    .bind(run.as_uuid())
    .bind(provenance)
    .bind(payload)
    .execute(&mut **tx)
    .await
    .map_err(crate::map_sqlx)?;
    Ok(())
}

impl Db {
    /// Create the ACTIVE execution for a run over its approved plan,
    /// snapshotting every plan step into the execution's own progress
    /// table.
    ///
    /// Idempotent under redelivery and races: when an active execution
    /// already exists it is returned untouched; a concurrent insert
    /// losing the partial unique index resolves to the winner's row.
    pub async fn create_execution_for_run(
        &self,
        org: OrganizationId,
        task: TaskId,
        run: WorkflowRunId,
        plan: hephaestus_core::id::PlanId,
    ) -> Result<ExecutionId> {
        if let Some(existing) = self.active_execution_for_run(org, run).await? {
            return Ok(ExecutionId::from_uuid(existing.id));
        }

        let exec = ExecutionId::generate();
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        // Tenant + ownership guard: the run must belong to this org and
        // task, and the plan must be the task's current one.
        let ok: Option<Uuid> = sqlx::query_scalar(
            "SELECT p.id FROM plans p
             JOIN tasks t ON t.id = p.task_id
             JOIN workflow_runs r ON r.task_id = t.id
             WHERE p.id = $1 AND p.superseded_by IS NULL
               AND r.id = $2 AND r.organization_id = $3 AND t.id = $4",
        )
        .bind(plan.as_uuid())
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .bind(task.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        if ok.is_none() {
            return Err(Error::NotFound { entity: "plan" });
        }

        sqlx::query(
            "INSERT INTO executions (id, organization_id, task_id, run_id, plan_id)
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(exec.as_uuid())
        .bind(org.as_uuid())
        .bind(task.as_uuid())
        .bind(run.as_uuid())
        .bind(plan.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        // Snapshot step content application-side so ids stay UUIDv7
        // from here: later plan supersession cannot rewrite what an
        // execution is committed to do.
        let snapshots: Vec<(Uuid, i32, String, String)> = sqlx::query_as(
            "SELECT id, position, action, verification FROM plan_steps
             WHERE plan_id = $1 ORDER BY position ASC",
        )
        .bind(plan.as_uuid())
        .fetch_all(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        for (step_id, position, action, verification) in snapshots {
            sqlx::query(
                "INSERT INTO execution_steps
                    (id, execution_id, step_id, position, action, verification)
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(Uuid::now_v7())
            .bind(exec.as_uuid())
            .bind(step_id)
            .bind(position)
            .bind(action)
            .bind(verification)
            .execute(&mut *tx)
            .await
            .map_err(crate::map_sqlx)?;
        }

        let payload =
            serde_json::to_value(hephaestus_core::event::EventPayload::ExecutionStarted {
                execution_id: hephaestus_core::id::HephaestusId(exec.as_uuid()),
                plan_id: hephaestus_core::id::HephaestusId(plan.as_uuid()),
            })
            .map_err(|e| Error::Storage(Box::new(e)))?;
        write_execution_event_tx(&mut tx, org, exec, run, "system", payload).await?;

        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(exec)
    }

    /// The active execution for a run, if any.
    pub async fn active_execution_for_run(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
    ) -> Result<Option<ExecutionRow>> {
        sqlx::query_as::<_, ExecutionRow>(
            "SELECT e.id, e.task_id, e.run_id, e.plan_id, e.status
             FROM executions e
             WHERE e.run_id = $1 AND e.organization_id = $2 AND e.status = 'active'",
        )
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// Fetch one execution; foreign or unknown ids are NotFound.
    pub async fn get_execution(
        &self,
        org: OrganizationId,
        exec: ExecutionId,
    ) -> Result<ExecutionRow> {
        sqlx::query_as::<_, ExecutionRow>(
            "SELECT id, task_id, run_id, plan_id, status
             FROM executions WHERE id = $1 AND organization_id = $2",
        )
        .bind(exec.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)?
        .ok_or(Error::NotFound {
            entity: "execution",
        })
    }

    /// All snapshotted steps of an execution in execution order.
    pub async fn list_execution_steps(
        &self,
        org: OrganizationId,
        exec: ExecutionId,
    ) -> Result<Vec<ExecutionStepRow>> {
        sqlx::query_as::<_, ExecutionStepRow>(
            "SELECT es.id, es.step_id, es.execution_id, es.position, s.action,
                    s.verification, es.status, es.summary, es.files_changed
             FROM execution_steps es
             JOIN executions e ON e.id = es.execution_id
             JOIN plan_steps s ON s.id = es.step_id
             WHERE es.execution_id = $1 AND e.organization_id = $2
             ORDER BY es.position ASC",
        )
        .bind(exec.as_uuid())
        .bind(org.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// The lowest-position still-pending step, if any.
    pub async fn next_pending_step(
        &self,
        org: OrganizationId,
        exec: ExecutionId,
    ) -> Result<Option<ExecutionStepRow>> {
        Ok(self
            .list_execution_steps(org, exec)
            .await?
            .into_iter()
            .find(|s| s.status == "pending"))
    }

    /// Mark one step completed with its implementer-reported summary
    /// and changed-file list. Fails with Conflict unless the step was
    /// pending or previously failed (a repair round may complete it).
    pub async fn complete_execution_step(
        &self,
        org: OrganizationId,
        exec: ExecutionId,
        step: StepId,
        summary: &str,
        files_changed: &[String],
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;
        let run: Option<Uuid> = sqlx::query_scalar(
            "UPDATE execution_steps es
                SET status = 'completed', summary = $3,
                    files_changed = $4, updated_at = now()
               FROM executions e
              WHERE es.execution_id = e.id AND es.step_id = $2
                AND e.id = $1 AND e.organization_id = $5
                AND es.status IN ('pending','failed')
            RETURNING e.run_id",
        )
        .bind(exec.as_uuid())
        .bind(step.as_uuid())
        .bind(summary)
        .bind(serde_json::to_value(files_changed).map_err(|e| Error::Storage(Box::new(e)))?)
        .bind(org.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        let Some(run_id) = run else {
            return Err(Error::Conflict {
                message: "step is not completable in its current state".into(),
            });
        };
        let payload = step_event_payload("completed", exec, step)?;
        write_execution_event_tx(
            &mut tx,
            org,
            exec,
            WorkflowRunId::from_uuid(run_id),
            "model_output",
            payload,
        )
        .await?;
        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(())
    }

    /// Record a step failure with its reason. A previously failed step
    /// may be failed again with an updated reason (repair rounds).
    pub async fn fail_execution_step(
        &self,
        org: OrganizationId,
        exec: ExecutionId,
        step: StepId,
        summary: &str,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;
        let run: Option<Uuid> = sqlx::query_scalar(
            "UPDATE execution_steps es
                SET status = 'failed', summary = $3, updated_at = now()
               FROM executions e
              WHERE es.execution_id = e.id AND es.step_id = $2
                AND e.id = $1 AND e.organization_id = $4
                AND es.status <> 'completed'
            RETURNING e.run_id",
        )
        .bind(exec.as_uuid())
        .bind(step.as_uuid())
        .bind(summary)
        .bind(org.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        let Some(run_id) = run else {
            return Err(Error::Conflict {
                message: "completed steps cannot be marked failed".into(),
            });
        };
        let payload = step_event_payload("failed", exec, step)?;
        write_execution_event_tx(
            &mut tx,
            org,
            exec,
            WorkflowRunId::from_uuid(run_id),
            "model_output",
            payload,
        )
        .await?;
        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(())
    }

    /// Record one verification suite run with its serialized report.
    ///
    /// The cycle number is derived inside the transaction (prior
    /// count + 1), so repeated suites take distinct cycles instead of
    /// colliding. The verification table IS the loop ledger.
    pub async fn record_verification(
        &self,
        org: OrganizationId,
        exec: ExecutionId,
        run: WorkflowRunId,
        passed: bool,
        report: &serde_json::Value,
    ) -> Result<VerificationId> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        // Tenant guard through the owning execution.
        let owned: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM executions WHERE id = $1 AND organization_id = $2")
                .bind(exec.as_uuid())
                .bind(org.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(crate::map_sqlx)?;
        if owned.is_none() {
            return Err(Error::NotFound {
                entity: "execution",
            });
        }

        let cycle: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM verifications WHERE execution_id = $1")
                .bind(exec.as_uuid())
                .fetch_one(&mut *tx)
                .await
                .map_err(crate::map_sqlx)?;

        let id = VerificationId::generate();
        sqlx::query(
            "INSERT INTO verifications (id, organization_id, execution_id, run_id, cycle, passed, report)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(id.as_uuid())
        .bind(org.as_uuid())
        .bind(exec.as_uuid())
        .bind(run.as_uuid())
        .bind(i32::try_from(cycle + 1).unwrap_or(i32::MAX))
        .bind(passed)
        .bind(report)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        let payload = serde_json::to_value(
            hephaestus_core::event::EventPayload::VerificationCompleted {
                execution_id: hephaestus_core::id::HephaestusId(exec.as_uuid()),
                verification_id: hephaestus_core::id::HephaestusId(id.as_uuid()),
                passed,
            },
        )
        .map_err(|e| Error::Storage(Box::new(e)))?;
        write_execution_event_tx(&mut tx, org, exec, run, "computed", payload).await?;

        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(id)
    }

    /// The newest verification record for an execution.
    pub async fn latest_verification(
        &self,
        org: OrganizationId,
        exec: ExecutionId,
    ) -> Result<Option<VerificationRow>> {
        sqlx::query_as::<_, VerificationRow>(
            "SELECT v.id, v.execution_id, v.cycle, v.passed, v.report, v.created_at
             FROM verifications v
             JOIN executions e ON e.id = v.execution_id
             WHERE v.execution_id = $1 AND e.organization_id = $2
             ORDER BY v.cycle DESC LIMIT 1",
        )
        .bind(exec.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// Failed-suite count for an execution: THE fix-loop ledger.
    pub async fn count_failed_verifications(
        &self,
        org: OrganizationId,
        exec: ExecutionId,
    ) -> Result<i64> {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM verifications v
             JOIN executions e ON e.id = v.execution_id
             WHERE v.execution_id = $1 AND e.organization_id = $2 AND v.passed = FALSE",
        )
        .bind(exec.as_uuid())
        .bind(org.as_uuid())
        .fetch_one(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// Close an active execution with a terminal status ("passed" or
    /// "failed"). Conflict when nothing is active.
    pub async fn finish_execution(
        &self,
        org: OrganizationId,
        exec: ExecutionId,
        status: &str,
    ) -> Result<()> {
        let res = sqlx::query(
            "UPDATE executions SET status = $3, updated_at = now()
             WHERE id = $1 AND organization_id = $2 AND status = 'active'",
        )
        .bind(exec.as_uuid())
        .bind(org.as_uuid())
        .bind(status)
        .execute(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        if res.rows_affected() != 1 {
            return Err(Error::Conflict {
                message: "no active execution to finish".into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::tasks::NewTask;
    use crate::testutil::test_db;
    use hephaestus_core::domain::{Plan, PlanStep, StrategyNotes};
    use hephaestus_core::id::{OrganizationId, PlanId, ProjectId, RepositoryId};

    async fn seed(db: &Db) -> (OrganizationId, TaskId, WorkflowRunId) {
        let org = OrganizationId::generate();
        let proj = ProjectId::generate();
        let repo = RepositoryId::generate();
        let slug = format!("exe-{}", org.as_uuid().simple());
        sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
            .bind(org.as_uuid())
            .bind(&slug)
            .execute(db.pool())
            .await
            .expect("org");
        sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
            .bind(proj.as_uuid())
            .bind(org.as_uuid())
            .bind(format!("ep-{}", proj.as_uuid().simple()))
            .execute(db.pool())
            .await
            .expect("proj");
        sqlx::query(
            "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
             VALUES ($1,$2,$3,'https://example.invalid/e.git','E')",
        )
        .bind(repo.as_uuid())
        .bind(org.as_uuid())
        .bind(proj.as_uuid())
        .execute(db.pool())
        .await
        .expect("repo");
        let task = db
            .create_task(&NewTask {
                organization_id: org,
                project_id: proj,
                repository_id: repo,
                title: "execution store test",
                description: "d",
                priority: "medium",
                risk: "low",
                labels: &[],
                idempotency_key: None,
            })
            .await
            .expect("task");
        let run = db.create_run(org, task).await.expect("run");
        (org, task, run)
    }

    async fn seed_plan(
        db: &Db,
        org: OrganizationId,
        task: TaskId,
        run: WorkflowRunId,
        steps: u32,
    ) -> Plan {
        let plan_id = PlanId::generate();
        let plan = Plan {
            id: plan_id,
            task_id: task,
            objective: "objective".into(),
            steps: (1..=steps)
                .map(|pos| PlanStep {
                    id: StepId::generate(),
                    plan_id,
                    position: pos,
                    action: format!("action {pos}"),
                    verification: format!("unit:check-{pos}"),
                    risks: vec![],
                })
                .collect(),
            affected_components: vec![],
            affected_symbols: vec![],
            strategy: StrategyNotes::default(),
            created_at: Utc::now(),
            prompt_version: None,
        };
        db.create_plan(org, task, run, &plan).await.expect("plan");
        plan
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execution_creation_snapshots_steps_and_is_idempotent() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;
        let plan = seed_plan(&db, org, task, run, 2).await;

        let exec = db
            .create_execution_for_run(org, task, run, plan.id)
            .await
            .expect("create");
        // Redelivery returns the same active execution untouched.
        let again = db
            .create_execution_for_run(org, task, run, plan.id)
            .await
            .expect("again");
        assert_eq!(exec.as_uuid(), again.as_uuid());

        let steps = db.list_execution_steps(org, exec).await.expect("steps");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].status, "pending");
        assert_eq!(
            steps.iter().map(|s| s.step_id).collect::<Vec<_>>(),
            plan.steps
                .iter()
                .map(|s| s.id.as_uuid())
                .collect::<Vec<_>>()
        );

        let started: Vec<String> = sqlx::query_scalar(
            "SELECT payload->>'type' FROM events WHERE aggregate='execution' AND aggregate_id=$1",
        )
        .bind(exec.as_uuid())
        .fetch_all(db.pool())
        .await
        .expect("events");
        assert!(started.iter().any(|t| t == "execution_started"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execution_requires_current_plan_and_tenant_match() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;
        let plan = seed_plan(&db, org, task, run, 1).await;

        let err = db
            .create_execution_for_run(OrganizationId::generate(), task, run, plan.id)
            .await
            .expect_err("foreign org refused");
        assert!(matches!(err, Error::NotFound { entity: "plan" }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn step_progress_walks_positions_in_order() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;
        let plan = seed_plan(&db, org, task, run, 3).await;
        let exec = db
            .create_execution_for_run(org, task, run, plan.id)
            .await
            .expect("create");

        for expected in 1..=3u32 {
            let next = db
                .next_pending_step(org, exec)
                .await
                .expect("query")
                .expect("pending remains");
            assert_eq!(next.position, expected as i32);
            let step = StepId::from_uuid(next.step_id);
            db.complete_execution_step(org, exec, step, "did it", &["src/a.rs".into()])
                .await
                .expect("complete");
        }
        assert!(
            db.next_pending_step(org, exec)
                .await
                .expect("query")
                .is_none(),
            "no pending steps remain"
        );

        // Completing again conflicts - the step left its mutable state.
        let done = StepId::from_uuid(plan.steps[0].id.as_uuid());
        let err = db
            .complete_execution_step(org, exec, done, "again", &[])
            .await
            .expect_err("double-complete refused");
        assert!(matches!(err, Error::Conflict { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_steps_are_repairable_then_completable() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;
        let plan = seed_plan(&db, org, task, run, 1).await;
        let exec = db
            .create_execution_for_run(org, task, run, plan.id)
            .await
            .expect("create");

        let step = StepId::from_uuid(plan.steps[0].id.as_uuid());
        db.fail_execution_step(org, exec, step, "blocked: missing context")
            .await
            .expect("fail");
        // Repair rounds may complete a previously failed step.
        db.complete_execution_step(org, exec, step, "recovered", &[])
            .await
            .expect("repair-complete");
        // ...but a completed step can never be failed afterwards.
        let err = db
            .fail_execution_step(org, exec, step, "too late")
            .await
            .expect_err("fail-after-complete refused");
        assert!(matches!(err, Error::Conflict { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn verification_cycles_record_monotonically() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;
        let plan = seed_plan(&db, org, task, run, 1).await;
        let exec = db
            .create_execution_for_run(org, task, run, plan.id)
            .await
            .expect("create");

        let report = serde_json::json!({ "results": [] });
        db.record_verification(org, exec, run, false, &report)
            .await
            .expect("record fail");
        db.record_verification(org, exec, run, true, &report)
            .await
            .expect("record pass");

        let latest = db
            .latest_verification(org, exec)
            .await
            .expect("query")
            .expect("some");
        assert_eq!(latest.cycle, 2);
        assert!(latest.passed);
        assert_eq!(
            db.count_failed_verifications(org, exec)
                .await
                .expect("count"),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn finishing_closes_the_active_execution_once() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;
        let plan = seed_plan(&db, org, task, run, 1).await;
        let exec = db
            .create_execution_for_run(org, task, run, plan.id)
            .await
            .expect("create");

        db.finish_execution(org, exec, "passed")
            .await
            .expect("finish");
        let row = db.get_execution(org, exec).await.expect("row");
        assert_eq!(row.status, "passed");
        let err = db
            .finish_execution(org, exec, "failed")
            .await
            .expect_err("second finish refused");
        assert!(matches!(err, Error::Conflict { .. }));

        // A finished execution no longer counts as active.
        assert!(
            db.active_execution_for_run(org, run)
                .await
                .expect("query")
                .is_none()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execution_queries_are_tenant_scoped() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;
        let plan = seed_plan(&db, org, task, run, 1).await;
        let exec = db
            .create_execution_for_run(org, task, run, plan.id)
            .await
            .expect("create");

        let other = OrganizationId::generate();
        assert!(db.get_execution(other, exec).await.is_err());
        assert!(
            db.list_execution_steps(other, exec)
                .await
                .expect("empty")
                .is_empty()
        );
        assert!(
            db.latest_verification(other, exec)
                .await
                .expect("none")
                .is_none()
        );
        let err = db
            .record_verification(other, exec, run, true, &serde_json::json!({}))
            .await
            .expect_err("foreign recording refused");
        assert!(matches!(
            err,
            Error::NotFound {
                entity: "execution"
            }
        ));
    }
}
