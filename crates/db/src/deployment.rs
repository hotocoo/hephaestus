//! Deployment persistence (ADR-013).
//!
//! A deployment row is bootstrapped by the build stage once the merge
//! and build evidence exist, mirroring how builds are bootstrapped at
//! the merge gate. The row carries the deterministic outcome of the
//! configured deploy command plus the mandatory post-deployment
//! verification hooks. State-affecting writes append their typed event
//! in the SAME transaction; every query is tenant-scoped.

use chrono::{DateTime, Utc};
use hephaestus_core::id::{BuildId, DeploymentId, OrganizationId, TaskId, WorkflowRunId};
use hephaestus_core::{Error, Result};
use uuid::Uuid;

use crate::store::Db;

/// A persisted deployment attempt.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeploymentRow {
    /// Deployment id.
    pub id: Uuid,
    /// Owning task.
    pub task_id: Uuid,
    /// Driving workflow run.
    pub run_id: Uuid,
    /// Build whose verified change set shipped.
    pub build_id: Uuid,
    /// Configured target name the plan cited.
    pub target: String,
    /// "running", "succeeded" or "failed".
    pub status: String,
    /// Why the deployment failed, when it failed.
    pub failure_reason: Option<String>,
    /// When the row was created.
    pub created_at: DateTime<Utc>,
}

impl Db {
    /// Create the RUNNING deployment row for a run over the build whose
    /// change set ships to `target`.
    ///
    /// Idempotent under redelivery AND concurrent workers: the partial
    /// unique index admits exactly one running row per run, and a lost
    /// insert resolves to the winner's row instead of erroring. The
    /// build must already be terminal-succeeded: deployments ship built
    /// change sets, never intentions.
    pub async fn create_deployment_for_build(
        &self,
        org: OrganizationId,
        task: TaskId,
        run: WorkflowRunId,
        build: BuildId,
        target: &str,
    ) -> Result<DeploymentId> {
        if target.trim().is_empty() {
            return Err(Error::Validation {
                field: "target".into(),
                message: "deployment requires a named target".into(),
            });
        }
        if let Some(existing) = self.active_deployment_for_run(org, run).await? {
            return Ok(DeploymentId::from_uuid(existing.id));
        }

        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        // Ownership guard: the build must belong to this tenant, this
        // task, this run - and must have succeeded.
        let ok: Option<Uuid> = sqlx::query_scalar(
            "SELECT b.id FROM builds b
             JOIN tasks t ON t.id = b.task_id
             WHERE b.id = $1 AND b.run_id = $2 AND b.task_id = $3
               AND t.organization_id = $4 AND b.status = 'succeeded'",
        )
        .bind(build.as_uuid())
        .bind(run.as_uuid())
        .bind(task.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        if ok.is_none() {
            return Err(Error::NotFound { entity: "build" });
        }

        let deployment = DeploymentId::generate();
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO deployments (id, organization_id, task_id, run_id, build_id, target)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (run_id) WHERE status = 'running' DO NOTHING
             RETURNING id",
        )
        .bind(deployment.as_uuid())
        .bind(org.as_uuid())
        .bind(task.as_uuid())
        .bind(run.as_uuid())
        .bind(build.as_uuid())
        .bind(target)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        // Lost a concurrent race: deliver the winner's identity so
        // every caller converges on the same durable row.
        let id = match inserted {
            Some(id) => id,
            None => sqlx::query_scalar(
                "SELECT id FROM deployments WHERE run_id = $1 AND organization_id = $2
                   AND status = 'running'",
            )
            .bind(run.as_uuid())
            .bind(org.as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(crate::map_sqlx)?
            .ok_or(Error::Conflict {
                message: "deployment row vanished during concurrent creation".into(),
            })?,
        };
        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(DeploymentId::from_uuid(id))
    }

    /// The RUNNING deployment row for a run, if any.
    pub async fn active_deployment_for_run(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
    ) -> Result<Option<DeploymentRow>> {
        sqlx::query_as::<_, DeploymentRow>(
            "SELECT id, task_id, run_id, build_id, target, status, failure_reason, created_at
             FROM deployments
             WHERE run_id = $1 AND organization_id = $2 AND status = 'running'",
        )
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// The most recent deployment row for a run regardless of status.
    pub async fn latest_deployment_for_run(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
    ) -> Result<Option<DeploymentRow>> {
        sqlx::query_as::<_, DeploymentRow>(
            "SELECT id, task_id, run_id, build_id, target, status, failure_reason, created_at
             FROM deployments
             WHERE run_id = $1 AND organization_id = $2
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// Close a deployment row with its terminal outcome and append the
    /// typed DeploymentOutcome event in the SAME transaction: state and
    /// audit trail never diverge. `verified` is true only when the
    /// post-deployment verification hooks all passed.
    pub async fn finish_deployment(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
        deployment: DeploymentId,
        verified: bool,
        failure_reason: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        let updated: Option<(Uuid, String)> = sqlx::query_as(
            "UPDATE deployments
                SET status = $3, failure_reason = $4, updated_at = now()
              WHERE id = $1 AND organization_id = $2 AND status = 'running'
            RETURNING id, target",
        )
        .bind(deployment.as_uuid())
        .bind(org.as_uuid())
        .bind(if verified { "succeeded" } else { "failed" })
        .bind(failure_reason)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        let Some((_, target)) = updated else {
            return Err(Error::Conflict {
                message: "deployment is not running; refusing to finish it twice".into(),
            });
        };

        let payload =
            serde_json::to_value(hephaestus_core::event::EventPayload::DeploymentOutcome {
                deployment_id: hephaestus_core::id::HephaestusId(deployment.as_uuid()),
                verified,
                environment: target,
            })
            .map_err(|e| Error::Storage(Box::new(e)))?;
        sqlx::query(
            "INSERT INTO events
               (id, schema_version, organization_id, aggregate, aggregate_id,
                correlation_id, provenance, payload)
             SELECT $1, $2, $3, 'deployment', $4, r.correlation_id, 'computed', $6
             FROM workflow_runs r WHERE r.id = $5",
        )
        .bind(Uuid::now_v7())
        .bind(i32::try_from(hephaestus_core::event::EVENT_ENVELOPE_VERSION).unwrap_or(i32::MAX))
        .bind(org.as_uuid())
        .bind(deployment.as_uuid())
        .bind(run.as_uuid())
        .bind(payload)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::tasks::NewTask;
    use crate::testutil::test_db;
    use hephaestus_core::id::{PlanId, ProjectId, RepositoryId, StepId};

    /// Fresh tenant rows plus a SUCCEEDED build to anchor deployments.
    async fn seeded(db: &Db) -> (OrganizationId, TaskId, WorkflowRunId, BuildId) {
        let org = OrganizationId::generate();
        let proj = ProjectId::generate();
        let repo = RepositoryId::generate();
        let slug = format!("dep-{}", org.as_uuid().simple());
        sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
            .bind(org.as_uuid())
            .bind(&slug)
            .execute(db.pool())
            .await
            .unwrap_or_default();
        sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
            .bind(proj.as_uuid())
            .bind(org.as_uuid())
            .bind(format!("dp-{}", proj.as_uuid().simple()))
            .execute(db.pool())
            .await
            .unwrap_or_default();
        sqlx::query(
            "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
             VALUES ($1,$2,$3,'https://example.invalid/d.git','D')",
        )
        .bind(repo.as_uuid())
        .bind(org.as_uuid())
        .bind(proj.as_uuid())
        .execute(db.pool())
        .await
        .unwrap_or_default();
        let tid = db
            .create_task(&NewTask {
                organization_id: org,
                project_id: proj,
                repository_id: repo,
                title: "deployment",
                description: "",
                priority: "medium",
                risk: "low",
                labels: &[],
                idempotency_key: None,
            })
            .await
            .expect("task");
        let run = db.create_run(org, tid).await.expect("run");

        let plan_id = PlanId::generate();
        let mut plan = hephaestus_core::domain::Plan {
            id: plan_id,
            task_id: tid,
            objective: "objective".into(),
            steps: vec![hephaestus_core::domain::PlanStep {
                id: StepId::generate(),
                plan_id,
                position: 1,
                action: "do".into(),
                verification: "unit:x".into(),
                risks: vec![],
            }],
            affected_components: vec![],
            affected_symbols: vec![],
            strategy: hephaestus_core::domain::StrategyNotes::default(),
            created_at: chrono::Utc::now(),
            prompt_version: None,
        };
        // The step must reference its own plan once inserted.
        plan.steps[0].plan_id = plan.id;
        db.create_plan(org, tid, run, &plan).await.expect("plan");
        let exec = db
            .create_execution_for_run(org, tid, run, plan.id)
            .await
            .expect("execution");
        db.finish_execution(org, exec, "passed")
            .await
            .expect("passed");
        let build = db
            .create_build_for_run(org, tid, run, exec)
            .await
            .expect("build");
        db.finish_build(
            org,
            run,
            build,
            true,
            Some("target/debug"),
            Some(&"a".repeat(64)),
        )
        .await
        .expect("build succeeded");
        (org, tid, run, build)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deployment_rows_are_idempotent_and_terminate_once() {
        let db = test_db().await;
        let (org, task, run, build) = seeded(&db).await;

        let d1 = db
            .create_deployment_for_build(org, task, run, build, "staging")
            .await
            .expect("create");
        let d2 = db
            .create_deployment_for_build(org, task, run, build, "staging")
            .await
            .expect("re-create is idempotent");
        assert_eq!(d1, d2);

        let active = db
            .active_deployment_for_run(org, run)
            .await
            .expect("read")
            .expect("running deployment");
        assert_eq!(active.build_id, build.as_uuid());
        assert_eq!(active.target, "staging");

        db.finish_deployment(org, run, d1, true, None)
            .await
            .expect("finish verified");

        let finished = db
            .latest_deployment_for_run(org, run)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(finished.status, "succeeded");
        assert!(finished.failure_reason.is_none());

        // Finishing twice is refused: the row already left 'running'.
        let err = db
            .finish_deployment(org, run, d1, false, Some("late failure"))
            .await
            .expect_err("double finish");
        assert!(matches!(err, Error::Conflict { .. }));

        // Exactly one typed outcome event carries target and verdict.
        let rows: Vec<(bool, String)> = sqlx::query_as(
            "SELECT (payload->'data'->>'verified')::boolean, payload->'data'->>'environment'
             FROM events WHERE aggregate='deployment' AND aggregate_id=$1
               AND payload->>'type'='deployment_outcome'",
        )
        .bind(d1.as_uuid())
        .fetch_all(db.pool())
        .await
        .expect("events");
        assert_eq!(rows, [(true, "staging".to_string())]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_deployment_records_reason_and_event() {
        let db = test_db().await;
        let (org, task, run, build) = seeded(&db).await;
        let dep = db
            .create_deployment_for_build(org, task, run, build, "staging")
            .await
            .expect("create");
        db.finish_deployment(org, run, dep, false, Some("hook exited 3"))
            .await
            .expect("finish failed");

        let row = db
            .latest_deployment_for_run(org, run)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(row.status, "failed");
        assert_eq!(row.failure_reason.as_deref(), Some("hook exited 3"));

        let ok: Vec<(bool,)> = sqlx::query_as(
            "SELECT (payload->'data'->>'verified')::boolean FROM events
              WHERE aggregate='deployment' AND aggregate_id=$1",
        )
        .bind(dep.as_uuid())
        .fetch_all(db.pool())
        .await
        .expect("events");
        assert_eq!(ok, [(false,)]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deployment_requires_succeeded_build_and_tenant_scope() {
        let db = test_db().await;
        let (org, task, run, _build) = seeded(&db).await;

        // A foreign build id is not found, whatever it claims.
        let stranger_build = BuildId::generate();
        let err = db
            .create_deployment_for_build(org, task, run, stranger_build, "staging")
            .await
            .expect_err("foreign build");
        assert!(matches!(err, Error::NotFound { entity: "build" }));

        // Another tenant cannot see this run's deployments.
        let stranger_org = OrganizationId::generate();
        assert!(
            db.active_deployment_for_run(stranger_org, run)
                .await
                .expect("read")
                .is_none()
        );
        assert!(
            db.latest_deployment_for_run(stranger_org, run)
                .await
                .expect("read")
                .is_none()
        );
    }
}
