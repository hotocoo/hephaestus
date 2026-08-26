//! Durable workflow runs: legal transitions only, CAS on state.

use chrono::{DateTime, Utc};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_core::{Error, Result};
use uuid::Uuid;

use hephaestus_core::id::{OrganizationId, TaskId, WorkflowRunId};

use crate::store::Db;

/// A persisted workflow run.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkflowRunRow {
    /// Run id.
    pub id: Uuid,
    /// Owning task.
    pub task_id: Uuid,
    /// Tenant scope.
    pub organization_id: Uuid,
    /// Current state name.
    pub state: String,
    /// Attempt counter.
    pub attempt: i32,
    /// Correlation id for tracing.
    pub correlation_id: Uuid,
    /// Current lease owner if any.
    pub lease_owner: Option<String>,
    /// Lease expiry if leased.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// Last transition time.
    pub last_transition_at: DateTime<Utc>,
}

impl Db {
    /// Create the initial workflow run for a task (state=created).
    pub async fn create_run(&self, org: OrganizationId, task: TaskId) -> Result<WorkflowRunId> {
        let id = WorkflowRunId::generate();
        let correlation = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO workflow_runs (id, task_id, organization_id, correlation_id)
             VALUES ($1,$2,$3,$4)",
        )
        .bind(id.as_uuid())
        .bind(task.as_uuid())
        .bind(org.as_uuid())
        .bind(correlation)
        .execute(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(id)
    }

    /// Fetch a run scoped by organization.
    pub async fn get_run(&self, org: OrganizationId, id: WorkflowRunId) -> Result<WorkflowRunRow> {
        sqlx::query_as::<_, WorkflowRunRow>(
            "SELECT id, task_id, organization_id, state, attempt, correlation_id,
                    lease_owner, lease_expires_at, last_transition_at
             FROM workflow_runs WHERE id = $1 AND organization_id = $2",
        )
        .bind(id.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)?
        .ok_or(Error::NotFound {
            entity: "workflow_run",
        })
    }

    /// Fetch the current run of a task, scoped by organization.
    ///
    /// Intake bootstraps exactly one run per task; ordering by id keeps
    /// the answer deterministic even if that invariant is ever relaxed,
    /// because ids are UUIDv7 and therefore time-ordered.
    pub async fn get_run_for_task(
        &self,
        org: OrganizationId,
        task: TaskId,
    ) -> Result<WorkflowRunRow> {
        sqlx::query_as::<_, WorkflowRunRow>(
            "SELECT id, task_id, organization_id, state, attempt, correlation_id,
                    lease_owner, lease_expires_at, last_transition_at
             FROM workflow_runs WHERE task_id = $1 AND organization_id = $2
             ORDER BY id DESC LIMIT 1",
        )
        .bind(task.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)?
        .ok_or(Error::NotFound {
            entity: "workflow_run",
        })
    }

    /// Apply a legal transition with optimistic concurrency:
    /// succeeds only if the row is still in \`expected\`.
    ///
    /// Returns the new state. Illegal or lost-race attempts fail closed
    /// with distinct errors so callers can distinguish policy from race.
    pub async fn transition_run(
        &self,
        org: OrganizationId,
        id: WorkflowRunId,
        expected: WorkflowState,
        trigger: TransitionEvent,
        actor: &str,
    ) -> Result<WorkflowState> {
        let next = expected.transition(trigger)?;
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        let updated = sqlx::query(
            "UPDATE workflow_runs
             SET state = $3, last_transition_at = now(), updated_at = now()
             WHERE id = $1 AND organization_id = $2 AND state = $4",
        )
        .bind(id.as_uuid())
        .bind(org.as_uuid())
        .bind(next.name())
        .bind(expected.name())
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        if updated.rows_affected() != 1 {
            return Err(Error::Conflict {
                message: format!(
                    "workflow moved past {} before applying {:?}",
                    expected.name(),
                    trigger
                ),
            });
        }

        // Event written in the SAME transaction as the state change:
        // never one without the other.
        let payload = serde_json::json!({
            "type": "workflow_state_changed",
            "data": {
                "run_id": id.to_string(),
                "from": expected.name(),
                "to": next.name(),
                "trigger": trigger_name(trigger),
                "actor": actor,
            }
        });
        sqlx::query(
            "INSERT INTO events
               (id, schema_version, organization_id, aggregate, aggregate_id,
                correlation_id, provenance, payload)
             SELECT $1, $2, $3, 'workflow_run', $4, correlation_id, 'computed', $5
             FROM workflow_runs WHERE id = $4",
        )
        .bind(Uuid::now_v7())
        .bind(i32::try_from(hephaestus_core::event::EVENT_ENVELOPE_VERSION).unwrap_or(i32::MAX))
        .bind(org.as_uuid())
        .bind(id.as_uuid())
        .bind(payload)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(next)
    }

    /// Acquire a lease on a runnable run (worker claim).
    ///
    /// Only runs in non-terminal states without an unexpired lease can
    /// be claimed; uses atomic conditional UPDATE.
    pub async fn try_claim_run(
        &self,
        id: WorkflowRunId,
        owner: &str,
        ttl_secs: i64,
    ) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE workflow_runs
             SET lease_owner = $2,
                 lease_expires_at = now() + make_interval(secs => $3),
                 updated_at = now()
             WHERE id = $1
               AND state NOT IN ('completed','failed','cancelled')
               AND (lease_owner IS NULL OR lease_expires_at < now())",
        )
        .bind(id.as_uuid())
        .bind(owner)
        .bind(ttl_secs as f64)
        .execute(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(res.rows_affected() == 1)
    }

    /// Release a lease (normal completion of step).
    pub async fn release_run_lease(&self, id: WorkflowRunId, owner: &str) -> Result<()> {
        sqlx::query(
            "UPDATE workflow_runs SET lease_owner = NULL, lease_expires_at = NULL
             WHERE id = $1 AND lease_owner = $2",
        )
        .bind(id.as_uuid())
        .bind(owner)
        .execute(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(())
    }

    /// Resolve the tenant scope and current state of a run.
    ///
    /// Job payloads carry only ids; workers are trusted infrastructure
    /// and may look up scope by run id alone. Unknown runs fail loudly.
    pub async fn run_scope(&self, id: WorkflowRunId) -> Result<RunScope> {
        let row: (Uuid, Uuid, String) = sqlx::query_as(
            "SELECT organization_id, task_id, state FROM workflow_runs WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)?
        .ok_or(Error::NotFound {
            entity: "workflow_run",
        })?;

        let state = WorkflowState::from_name(&row.2).ok_or_else(|| Error::Validation {
            field: "state".into(),
            message: format!("unknown persisted workflow state {:?}", row.2),
        })?;
        Ok(RunScope {
            organization_id: OrganizationId::from_uuid(row.0),
            task_id: TaskId::from_uuid(row.1),
            state,
        })
    }
}

/// Tenant scope plus current state of one workflow run.
#[derive(Debug, Clone)]
pub struct RunScope {
    /// Tenant owning the run.
    pub organization_id: OrganizationId,
    /// Task driving the run.
    pub task_id: TaskId,
    /// Current machine state.
    pub state: WorkflowState,
}

fn trigger_name(t: TransitionEvent) -> &'static str {
    match t {
        TransitionEvent::StartAnalysis => "start_analysis",
        TransitionEvent::StartPlanning => "start_planning",
        TransitionEvent::SubmitForApproval => "submit_for_approval",
        TransitionEvent::BeginImplementation => "begin_implementation",
        TransitionEvent::Approve => "approve",
        TransitionEvent::RejectPlan => "reject_plan",
        TransitionEvent::StartVerification => "start_verification",
        TransitionEvent::VerificationPassed => "verification_passed",
        TransitionEvent::VerificationFailed => "verification_failed",
        TransitionEvent::ChangesRequested => "changes_requested",
        TransitionEvent::ReviewPassed => "review_passed",
        TransitionEvent::Merged => "merged",
        TransitionEvent::BuildSucceeded => "build_succeeded",
        TransitionEvent::SkipDeployment => "skip_deployment",
        TransitionEvent::DeploymentFinished => "deployment_finished",
        TransitionEvent::DeploymentVerified => "deployment_verified",
        TransitionEvent::RollbackRequested => "rollback_requested",
        TransitionEvent::Fail => "fail",
        TransitionEvent::Cancel => "cancel",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::tasks::NewTask;
    use crate::testutil::test_db;
    use hephaestus_core::id::ProjectId;

    async fn seeded_task(db: &Db) -> (OrganizationId, TaskId) {
        let org = OrganizationId::generate();
        let proj = ProjectId::generate();
        let repo = hephaestus_core::id::RepositoryId::generate();
        let slug = format!("wfo-{}", org.as_uuid().simple());
        sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
            .bind(org.as_uuid())
            .bind(&slug)
            .execute(db.pool())
            .await
            .unwrap_or_default();
        sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
            .bind(proj.as_uuid())
            .bind(org.as_uuid())
            .bind(format!("wp-{}", proj.as_uuid().simple()))
            .execute(db.pool())
            .await
            .unwrap_or_default();
        sqlx::query(
            "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
             VALUES ($1,$2,$3,'https://example.invalid/w.git','W')",
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
                title: "wf",
                description: "",
                priority: "medium",
                risk: "low",
                labels: &[],
                idempotency_key: None,
            })
            .await
            .expect("task");
        (org, tid)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn legal_transition_persists_state_and_event_atomically() {
        let db = test_db().await;
        let (org, tid) = seeded_task(&db).await;
        let run = db.create_run(org, tid).await.expect("run");

        let next = db
            .transition_run(
                org,
                run,
                WorkflowState::Created,
                TransitionEvent::StartAnalysis,
                "test-worker",
            )
            .await
            .expect("transition");
        assert_eq!(next, WorkflowState::Analyzing);

        let row = db.get_run(org, run).await.expect("row");
        assert_eq!(row.state, "analyzing");

        // Exactly one event recorded for this run.
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM events WHERE aggregate='workflow_run' AND aggregate_id=$1",
        )
        .bind(run.as_uuid())
        .fetch_one(db.pool())
        .await
        .expect("count");
        assert_eq!(count.0, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn illegal_transition_rejected_and_state_unchanged() {
        let db = test_db().await;
        let (org, tid) = seeded_task(&db).await;
        let run = db.create_run(org, tid).await.expect("run");
        let err = db
            .transition_run(
                org,
                run,
                WorkflowState::Created,
                TransitionEvent::BuildSucceeded,
                "test",
            )
            .await;
        assert!(matches!(err, Err(Error::IllegalTransition { .. })));
        let row = db.get_run(org, run).await.expect("row");
        assert_eq!(
            row.state, "created",
            "state must not change on illegal transition"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cas_detects_lost_race() {
        let db = test_db().await;
        let (org, tid) = seeded_task(&db).await;
        let run = db.create_run(org, tid).await.expect("run");
        // First worker advances created->analyzing.
        db.transition_run(
            org,
            run,
            WorkflowState::Created,
            TransitionEvent::StartAnalysis,
            "a",
        )
        .await
        .expect("first");
        // Second worker still believes it is Created: conflict, not silent.
        let err = db
            .transition_run(
                org,
                run,
                WorkflowState::Created,
                TransitionEvent::StartAnalysis,
                "b",
            )
            .await;
        assert!(matches!(err, Err(Error::Conflict { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn leases_are_exclusive_until_expired() {
        let db = test_db().await;
        let (org, tid) = seeded_task(&db).await;
        let run = db.create_run(org, tid).await.expect("run");
        db.transition_run(
            org,
            run,
            WorkflowState::Created,
            TransitionEvent::StartAnalysis,
            "seed",
        )
        .await
        .expect("advance");
        assert!(db.try_claim_run(run, "w1", 60).await.expect("claim1"));
        assert!(
            !db.try_claim_run(run, "w2", 60).await.expect("claim2"),
            "second claim while lease live must fail"
        );
        db.release_run_lease(run, "w1").await.expect("release");
        assert!(db.try_claim_run(run, "w2", 60).await.expect("re-claim"));
    }
}
