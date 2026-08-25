//! Delivery persistence: merge gates and build evidence.
//!
//! A merge is recorded as a decision on the run's open `merge`
//! approval gate - the same mechanism, table and event payload as
//! every other human gate, because merging IS a gate decision (the
//! external act of humans or CI accepting the change set).
//!
//! Builds are durable evidence rows bootstrapped when the merge
//! decision is applied, mirroring how executions are bootstrapped at
//! the approval gate. State-affecting writes append their event in
//! the SAME transaction; all queries are tenant-scoped.

use chrono::{DateTime, Utc};
use hephaestus_core::id::{BuildId, ExecutionId, OrganizationId, TaskId, WorkflowRunId};
use hephaestus_core::{Error, Result};
use uuid::Uuid;

use crate::planning::ApprovalRow;
use crate::store::Db;

/// A persisted build attempt.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BuildRow {
    /// Build id.
    pub id: Uuid,
    /// Owning task.
    pub task_id: Uuid,
    /// Driving workflow run.
    pub run_id: Uuid,
    /// Execution whose verified change set was built.
    pub execution_id: Uuid,
    /// "running", "succeeded" or "failed".
    pub status: String,
    /// Workspace-relative path of the primary artifact directory.
    pub artifact_path: Option<String>,
    /// SHA-256 over the produced artifacts (hex), when built.
    pub artifact_sha256: Option<String>,
    /// When the build row was created.
    pub created_at: DateTime<Utc>,
}

impl Db {
    /// Open the merge gate for a run if not already open. Returns the
    /// gate id; idempotent under job redelivery.
    ///
    /// Unlike plan approvals this gate is opened by the review stage
    /// when a run parks at `awaiting_merge`, so operators (and the
    /// API layer) can enumerate pending merges.
    pub async fn open_merge_gate(
        &self,
        org: OrganizationId,
        task: TaskId,
        run: WorkflowRunId,
    ) -> Result<Uuid> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT a.id FROM approvals a
             JOIN tasks t ON t.id = a.task_id
             WHERE a.run_id = $1 AND a.gate = 'merge' AND a.decision IS NULL
               AND t.organization_id = $2
             LIMIT 1",
        )
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        if let Some(id) = existing {
            return Ok(id);
        }

        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO approvals (id, task_id, run_id, gate, required_role)
             SELECT $1, t.id, $3, 'merge', 'maintainer'
             FROM tasks t WHERE t.id = $2 AND t.organization_id = $4",
        )
        .bind(id)
        .bind(task.as_uuid())
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(id)
    }

    /// The most recent merge-gate row for a run, decided or not.
    pub async fn get_merge_gate(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
    ) -> Result<Option<ApprovalRow>> {
        sqlx::query_as::<_, ApprovalRow>(
            "SELECT a.id, a.task_id, a.run_id, a.gate, a.required_role, a.decision
             FROM approvals a
             JOIN tasks t ON t.id = a.task_id
             WHERE a.run_id = $1 AND a.gate = 'merge' AND t.organization_id = $2
             ORDER BY a.created_at DESC LIMIT 1",
        )
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// Record the merge decision on the OPEN merge gate of a run and
    /// emit the typed event in the SAME transaction.
    ///
    /// There is no "rejected" merge: rejecting a change set happens at
    /// the review gate or by cancelling the task. Conflict when no
    /// undecided gate exists - double merges are refused, not absorbed.
    pub async fn decide_merge_gate(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
        principal: &str,
        reason: Option<&str>,
    ) -> Result<Uuid> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        let updated: Option<(Uuid, Uuid)> = sqlx::query_as(
            "UPDATE approvals a
                SET decision = 'approved', decided_by = $3, decided_at = now(), reason = $4
               FROM tasks t
              WHERE a.task_id = t.id AND a.run_id = $1
                AND a.gate = 'merge' AND a.decision IS NULL
                AND t.organization_id = $2
            RETURNING a.id, a.task_id",
        )
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .bind(principal)
        .bind(reason)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        let Some((approval_id, task_id)) = updated else {
            return Err(Error::Conflict {
                message: "no open merge gate for this run".into(),
            });
        };

        let payload = serde_json::to_value(hephaestus_core::event::EventPayload::ApprovalDecided {
            approval_id: hephaestus_core::id::HephaestusId(approval_id),
            approved: true,
            principal: principal.to_string(),
        })
        .map_err(|e| Error::Storage(Box::new(e)))?;
        sqlx::query(
            "INSERT INTO events
               (id, schema_version, organization_id, aggregate, aggregate_id,
                correlation_id, provenance, payload)
             SELECT $1, $2, $3, 'task', $4, r.correlation_id, 'user', $6
             FROM workflow_runs r WHERE r.id = $5",
        )
        .bind(Uuid::now_v7())
        .bind(i32::try_from(hephaestus_core::event::EVENT_ENVELOPE_VERSION).unwrap_or(i32::MAX))
        .bind(org.as_uuid())
        .bind(task_id)
        .bind(run.as_uuid())
        .bind(payload)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(approval_id)
    }

    /// Create the RUNNING build row for a run over the execution whose
    /// change set is being delivered.
    ///
    /// Idempotent under redelivery AND concurrent workers: the partial
    /// unique index admits exactly one running row per run, and a lost
    /// insert resolves to the winner's row instead of erroring.
    pub async fn create_build_for_run(
        &self,
        org: OrganizationId,
        task: TaskId,
        run: WorkflowRunId,
        exec: ExecutionId,
    ) -> Result<BuildId> {
        if let Some(existing) = self.active_build_for_run(org, run).await? {
            return Ok(BuildId::from_uuid(existing.id));
        }

        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        // Ownership guard: run, task and execution must line up inside
        // this tenant before anything is inserted.
        let ok: Option<Uuid> = sqlx::query_scalar(
            "SELECT e.id FROM executions e
             JOIN workflow_runs r ON r.id = e.run_id
             JOIN tasks t ON t.id = e.task_id
             WHERE e.id = $1 AND e.run_id = $2 AND e.task_id = $3
               AND r.organization_id = $4 AND t.id = $3",
        )
        .bind(exec.as_uuid())
        .bind(run.as_uuid())
        .bind(task.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        if ok.is_none() {
            return Err(Error::NotFound {
                entity: "execution",
            });
        }

        let build = BuildId::generate();
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO builds (id, organization_id, task_id, run_id, execution_id)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (run_id) WHERE status = 'running' DO NOTHING
             RETURNING id",
        )
        .bind(build.as_uuid())
        .bind(org.as_uuid())
        .bind(task.as_uuid())
        .bind(run.as_uuid())
        .bind(exec.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        // Lost a concurrent race: deliver the winner's identity so
        // every caller converges on the same durable row.
        let id = match inserted {
            Some(id) => id,
            None => sqlx::query_scalar(
                "SELECT id FROM builds WHERE run_id = $1 AND organization_id = $2
                   AND status = 'running'",
            )
            .bind(run.as_uuid())
            .bind(org.as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(crate::map_sqlx)?
            .ok_or(Error::Conflict {
                message: "build row vanished during concurrent creation".into(),
            })?,
        };
        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(BuildId::from_uuid(id))
    }

    /// The RUNNING build row for a run, if any.
    pub async fn active_build_for_run(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
    ) -> Result<Option<BuildRow>> {
        sqlx::query_as::<_, BuildRow>(
            "SELECT id, task_id, run_id, execution_id, status, artifact_path,
                    artifact_sha256, created_at
             FROM builds
             WHERE run_id = $1 AND organization_id = $2 AND status = 'running'",
        )
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// The most recent build row for a run regardless of status.
    pub async fn latest_build_for_run(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
    ) -> Result<Option<BuildRow>> {
        sqlx::query_as::<_, BuildRow>(
            "SELECT id, task_id, run_id, execution_id, status, artifact_path,
                    artifact_sha256, created_at
             FROM builds
             WHERE run_id = $1 AND organization_id = $2
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// Close a build row with its terminal outcome and append the
    /// typed build event in the SAME transaction: state and audit
    /// trail never diverge.
    pub async fn finish_build(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
        build: BuildId,
        ok: bool,
        artifact_path: Option<&str>,
        artifact_sha256: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        let updated = sqlx::query(
            "UPDATE builds
                SET status = $3, artifact_path = $4, artifact_sha256 = $5,
                    updated_at = now()
              WHERE id = $1 AND organization_id = $2 AND status = 'running'",
        )
        .bind(build.as_uuid())
        .bind(org.as_uuid())
        .bind(if ok { "succeeded" } else { "failed" })
        .bind(artifact_path)
        .bind(artifact_sha256)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        if updated.rows_affected() != 1 {
            return Err(Error::Conflict {
                message: "build is not running; refusing to finish it twice".into(),
            });
        }

        let payload = serde_json::to_value(hephaestus_core::event::EventPayload::BuildCompleted {
            build_id: hephaestus_core::id::HephaestusId(build.as_uuid()),
            ok,
            artifact_sha256: artifact_sha256.map(str::to_string),
        })
        .map_err(|e| Error::Storage(Box::new(e)))?;
        sqlx::query(
            "INSERT INTO events
               (id, schema_version, organization_id, aggregate, aggregate_id,
                correlation_id, provenance, payload)
             SELECT $1, $2, $3, 'build', $4, r.correlation_id, 'computed', $6
             FROM workflow_runs r WHERE r.id = $5",
        )
        .bind(Uuid::now_v7())
        .bind(i32::try_from(hephaestus_core::event::EVENT_ENVELOPE_VERSION).unwrap_or(i32::MAX))
        .bind(org.as_uuid())
        .bind(build.as_uuid())
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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::tasks::NewTask;
    use crate::testutil::test_db;
    use hephaestus_core::id::ProjectId;

    /// Fresh tenant + task + run, mirroring the workflow store tests.
    async fn seeded(db: &Db) -> (OrganizationId, TaskId, WorkflowRunId, ExecutionId) {
        let org = OrganizationId::generate();
        let proj = ProjectId::generate();
        let repo = hephaestus_core::id::RepositoryId::generate();
        let slug = format!("dlv-{}", org.as_uuid().simple());
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
                title: "delivery",
                description: "",
                priority: "medium",
                risk: "low",
                labels: &[],
                idempotency_key: None,
            })
            .await
            .expect("task");
        let run = db.create_run(org, tid).await.expect("run");

        // An execution row to anchor builds to.
        let plan = hephaestus_core::domain::Plan {
            id: hephaestus_core::id::PlanId::generate(),
            task_id: tid,
            objective: "objective".into(),
            steps: vec![hephaestus_core::domain::PlanStep {
                id: hephaestus_core::id::StepId::generate(),
                plan_id: hephaestus_core::id::PlanId::from_uuid(uuid::Uuid::nil()),
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
        }
        .tap(|p| p.steps[0].plan_id = p.id);
        db.create_plan(org, tid, run, &plan).await.expect("plan");
        let exec = db
            .create_execution_for_run(org, tid, run, plan.id)
            .await
            .expect("execution");
        (org, tid, run, exec)
    }

    /// Tiny combinator keeping the seed readable without expect-chains.
    trait Tap: Sized {
        fn tap<F: FnOnce(&mut Self)>(mut self, f: F) -> Self {
            f(&mut self);
            self
        }
    }
    impl<T> Tap for T {}

    #[tokio::test(flavor = "multi_thread")]
    async fn merge_gate_opens_once_and_decides_once() {
        let db = test_db().await;
        let (org, task, run, _exec) = seeded(&db).await;

        let first = db.open_merge_gate(org, task, run).await.expect("open gate");
        let second = db
            .open_merge_gate(org, task, run)
            .await
            .expect("reopen is idempotent");
        assert_eq!(first, second, "gate must not duplicate");

        let open = db
            .get_merge_gate(org, run)
            .await
            .expect("read")
            .expect("gate");
        assert_eq!(open.decision, None);
        assert_eq!(open.gate, "merge");

        let decided = db
            .decide_merge_gate(org, run, "tester", Some("merged PR 7"))
            .await
            .expect("decide");
        assert_eq!(decided, first);

        // A second merge decision is refused loudly, not absorbed.
        let err = db
            .decide_merge_gate(org, run, "tester", None)
            .await
            .expect_err("double decide");
        assert!(matches!(err, Error::Conflict { .. }));

        // The decision event is durable with user provenance,
        // recorded on the task aggregate exactly like plan-approval
        // decisions; the payload itself carries the gate id.
        let prov: Vec<(String,)> = sqlx::query_as(
            "SELECT provenance FROM events WHERE payload->>'type'='approval_decided'
              AND organization_id=$1
              AND payload->'data'->>'approval_id'=$2",
        )
        .bind(org.as_uuid())
        .bind(decided.to_string())
        .fetch_all(db.pool())
        .await
        .expect("events");
        assert_eq!(prov.len(), 1);
        assert_eq!(prov[0].0, "user");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn merge_gate_requires_tenant_scope() {
        let db = test_db().await;
        let (org, task, run, _exec) = seeded(&db).await;
        db.open_merge_gate(org, task, run).await.expect("open");

        let stranger = OrganizationId::generate();
        let gate = db.get_merge_gate(stranger, run).await.expect("read");
        assert!(gate.is_none(), "foreign tenant must not see the gate");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_rows_are_idempotent_and_terminate_once() {
        let db = test_db().await;
        let (org, task, run, exec) = seeded(&db).await;

        let b1 = db
            .create_build_for_run(org, task, run, exec)
            .await
            .expect("create");
        let b2 = db
            .create_build_for_run(org, task, run, exec)
            .await
            .expect("re-create is idempotent");
        assert_eq!(b1, b2);

        let active = db
            .active_build_for_run(org, run)
            .await
            .expect("read")
            .expect("running build");
        assert_eq!(active.execution_id, exec.as_uuid());

        db.finish_build(
            org,
            run,
            b1,
            true,
            Some("target/debug"),
            Some(&"a".repeat(64)),
        )
        .await
        .expect("finish");

        let finished = db
            .latest_build_for_run(org, run)
            .await
            .expect("read")
            .expect("build row");
        assert_eq!(finished.status, "succeeded");
        assert_eq!(
            finished.artifact_sha256.as_deref(),
            Some("a").map(|s| s.repeat(64)).as_deref()
        );

        // Finishing twice is refused: the row already left 'running'.
        let err = db
            .finish_build(org, run, b1, false, None, None)
            .await
            .expect_err("double finish");
        assert!(matches!(err, Error::Conflict { .. }));

        // Exactly one build_completed event carries the hash.
        let rows: Vec<(bool, Option<String>)> = sqlx::query_as(
            "SELECT (payload->'data'->>'ok')::boolean, payload->'data'->>'artifact_sha256'
             FROM events WHERE aggregate='build' AND aggregate_id=$1
               AND payload->>'type'='build_completed'",
        )
        .bind(b1.as_uuid())
        .fetch_all(db.pool())
        .await
        .expect("events");
        assert_eq!(rows, [(true, Some("a".repeat(64)))]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_creation_rejects_foreign_execution() {
        let db = test_db().await;
        let (org, task, run, _exec) = seeded(&db).await;
        let stranger_exec = ExecutionId::generate();
        let err = db
            .create_build_for_run(org, task, run, stranger_exec)
            .await
            .expect_err("foreign execution");
        assert!(matches!(
            err,
            Error::NotFound {
                entity: "execution"
            }
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_build_records_failure_event() {
        let db = test_db().await;
        let (org, task, run, exec) = seeded(&db).await;
        let build = db
            .create_build_for_run(org, task, run, exec)
            .await
            .expect("create");
        db.finish_build(org, run, build, false, None, None)
            .await
            .expect("finish failed");

        let row = db
            .latest_build_for_run(org, run)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(row.status, "failed");
        assert!(row.artifact_sha256.is_none());

        let ok: Vec<(bool,)> = sqlx::query_as(
            "SELECT (payload->'data'->>'ok')::boolean FROM events
              WHERE aggregate='build' AND aggregate_id=$1",
        )
        .bind(build.as_uuid())
        .fetch_all(db.pool())
        .await
        .expect("events");
        assert_eq!(ok, [(false,)]);
    }
}
