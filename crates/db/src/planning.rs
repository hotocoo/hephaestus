//! Planning persistence: requirements, plans, plan steps, approval
//! gates.
//!
//! All queries are tenant-scoped through the owning task (the planning
//! tables carry no organization column of their own; scope is joined
//! via `tasks`). Requirement replacement and plan creation are
//! transactional and append an event in the SAME transaction, so a
//! crash never leaves data without its audit trail.

use chrono::{DateTime, Utc};
use hephaestus_core::domain::{Plan, PlanStep, Requirement, RequirementCategory, RequirementKind};
use hephaestus_core::id::{
    OrganizationId, PlanId, RepositoryId, RequirementId, StepId, TaskId, WorkflowRunId,
};
use hephaestus_core::{Error, Result};
use uuid::Uuid;

use crate::store::Db;

fn category_name(c: RequirementCategory) -> &'static str {
    match c {
        RequirementCategory::Functional => "functional",
        RequirementCategory::NonFunctional => "non_functional",
        RequirementCategory::Security => "security",
        RequirementCategory::Performance => "performance",
        RequirementCategory::Compatibility => "compatibility",
        RequirementCategory::Constraint => "constraint",
    }
}

fn kind_name(k: RequirementKind) -> &'static str {
    match k {
        RequirementKind::Explicit => "explicit",
        RequirementKind::Inferred => "inferred",
        RequirementKind::Assumption => "assumption",
        RequirementKind::Unknown => "unknown",
    }
}

fn parse_category(raw: &str) -> Result<RequirementCategory> {
    match raw {
        "functional" => Ok(RequirementCategory::Functional),
        "non_functional" => Ok(RequirementCategory::NonFunctional),
        "security" => Ok(RequirementCategory::Security),
        "performance" => Ok(RequirementCategory::Performance),
        "compatibility" => Ok(RequirementCategory::Compatibility),
        "constraint" => Ok(RequirementCategory::Constraint),
        other => Err(Error::Validation {
            field: "category".into(),
            message: format!("unknown requirement category {other:?}"),
        }),
    }
}

fn parse_kind(raw: &str) -> Result<RequirementKind> {
    match raw {
        "explicit" => Ok(RequirementKind::Explicit),
        "inferred" => Ok(RequirementKind::Inferred),
        "assumption" => Ok(RequirementKind::Assumption),
        "unknown" => Ok(RequirementKind::Unknown),
        other => Err(Error::Validation {
            field: "kind".into(),
            message: format!("unknown requirement kind {other:?}"),
        }),
    }
}

/// A persisted approval gate row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApprovalRow {
    /// Approval id.
    pub id: Uuid,
    /// Owning task.
    pub task_id: Uuid,
    /// Run this gate blocks (when scoped to one).
    pub run_id: Option<Uuid>,
    /// Which gate.
    pub gate: String,
    /// Role required to decide.
    pub required_role: String,
    /// Decision when made.
    pub decision: Option<String>,
}

impl Db {
    /// Atomically replace ALL requirements for a task and record the
    /// event. Re-runs are safe: the result is the same set.
    pub async fn replace_requirements(
        &self,
        org: OrganizationId,
        task: TaskId,
        run: WorkflowRunId,
        requirements: &[Requirement],
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        // Tenant guard: refuse unknown or foreign tasks loudly.
        let owned: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM tasks WHERE id = $1 AND organization_id = $2")
                .bind(task.as_uuid())
                .bind(org.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(crate::map_sqlx)?;
        if owned.is_none() {
            return Err(Error::NotFound { entity: "task" });
        }

        sqlx::query("DELETE FROM requirements WHERE task_id = $1")
            .bind(task.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(crate::map_sqlx)?;

        for r in requirements {
            sqlx::query(
                "INSERT INTO requirements
                    (id, task_id, category, kind, statement, source, gating)
                 VALUES ($1,$2,$3,$4,$5,$6,$7)",
            )
            .bind(r.id.as_uuid())
            .bind(task.as_uuid())
            .bind(category_name(r.category))
            .bind(kind_name(r.kind))
            .bind(&r.statement)
            .bind(&r.source)
            .bind(r.is_gating())
            .execute(&mut *tx)
            .await
            .map_err(crate::map_sqlx)?;
        }

        // Typed payload serialized through the shared event model:
        // consumers decode it without knowing this table layout.
        let payload = serde_json::to_value(
            hephaestus_core::event::EventPayload::RequirementsExtracted {
                task_id: hephaestus_core::id::HephaestusId(task.as_uuid()),
                count: u32::try_from(requirements.len()).unwrap_or(u32::MAX),
            },
        )
        .map_err(|e| Error::Storage(Box::new(e)))?;
        Self::write_task_event_tx(&mut tx, org, task, run, "model_output", payload).await?;

        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(())
    }

    /// List a task's requirements, oldest first. Empty list is normal.
    pub async fn list_requirements(
        &self,
        org: OrganizationId,
        task: TaskId,
    ) -> Result<Vec<Requirement>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            id: Uuid,
            task_id: Uuid,
            category: String,
            kind: String,
            statement: String,
            source: String,
            created_at: DateTime<Utc>,
        }
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT r.id, r.task_id, r.category, r.kind, r.statement, r.source, r.created_at
             FROM requirements r
             JOIN tasks t ON t.id = r.task_id
             WHERE r.task_id = $1 AND t.organization_id = $2
             ORDER BY r.created_at ASC, r.id ASC",
        )
        .bind(task.as_uuid())
        .bind(org.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(crate::map_sqlx)?;

        rows.into_iter()
            .map(|row| {
                Ok(Requirement {
                    id: RequirementId::from_uuid(row.id),
                    task_id: TaskId::from_uuid(row.task_id),
                    category: parse_category(&row.category)?,
                    kind: parse_kind(&row.kind)?,
                    statement: row.statement,
                    source: row.source,
                    created_at: row.created_at,
                })
            })
            .collect()
    }

    /// Insert a plan with its steps transactionally, superseding any
    /// previous current plan for the task, and record the event.
    pub async fn create_plan(
        &self,
        org: OrganizationId,
        task: TaskId,
        run: WorkflowRunId,
        plan: &Plan,
    ) -> Result<()> {
        // Domain invariants are re-checked at the persistence boundary:
        // nothing reaches SQL unless the pure model says it is legal.
        plan.validate()?;

        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;
        let owned: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM tasks WHERE id = $1 AND organization_id = $2")
                .bind(task.as_uuid())
                .bind(org.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(crate::map_sqlx)?;
        if owned.is_none() {
            return Err(Error::NotFound { entity: "task" });
        }

        let strategy =
            serde_json::to_value(&plan.strategy).map_err(|e| Error::Storage(Box::new(e)))?;
        sqlx::query(
            "INSERT INTO plans
                (id, task_id, objective, affected_components, affected_symbols, strategy, prompt_version)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(plan.id.as_uuid())
        .bind(task.as_uuid())
        .bind(&plan.objective)
        .bind(serde_json::to_value(&plan.affected_components).map_err(|e| Error::Storage(Box::new(e)))?)
        .bind(serde_json::to_value(&plan.affected_symbols).map_err(|e| Error::Storage(Box::new(e)))?)
        .bind(strategy)
        .bind(&plan.prompt_version)
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        // Supersede every still-current plan; exactly one survives as
        // current at any time. Runs after the insert because the
        // superseded_by self-reference needs its target row to exist.
        sqlx::query(
            "UPDATE plans SET superseded_by = $2
             WHERE task_id = $1 AND superseded_by IS NULL AND id <> $2",
        )
        .bind(task.as_uuid())
        .bind(plan.id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;

        for step in &plan.steps {
            sqlx::query(
                "INSERT INTO plan_steps (id, plan_id, position, action, verification, risks)
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(step.id.as_uuid())
            .bind(plan.id.as_uuid())
            .bind(i32::try_from(step.position).unwrap_or(i32::MAX))
            .bind(&step.action)
            .bind(&step.verification)
            .bind(serde_json::to_value(&step.risks).map_err(|e| Error::Storage(Box::new(e)))?)
            .execute(&mut *tx)
            .await
            .map_err(crate::map_sqlx)?;
        }

        let payload = serde_json::to_value(hephaestus_core::event::EventPayload::PlanGenerated {
            task_id: hephaestus_core::id::HephaestusId(task.as_uuid()),
            plan_id: hephaestus_core::id::HephaestusId(plan.id.as_uuid()),
        })
        .map_err(|e| Error::Storage(Box::new(e)))?;
        Self::write_task_event_tx(&mut tx, org, task, run, "model_output", payload).await?;

        tx.commit().await.map_err(crate::map_sqlx)?;
        Ok(())
    }

    /// The task's current (non-superseded) plan with ordered steps.
    /// None before the first plan exists.
    pub async fn get_current_plan(
        &self,
        org: OrganizationId,
        task: TaskId,
    ) -> Result<Option<Plan>> {
        #[derive(sqlx::FromRow)]
        struct PlanRecord {
            id: Uuid,
            objective: String,
            affected_components: serde_json::Value,
            affected_symbols: serde_json::Value,
            strategy: serde_json::Value,
            prompt_version: Option<String>,
            created_at: DateTime<Utc>,
        }

        let record: Option<PlanRecord> = sqlx::query_as(
            "SELECT p.id, p.objective, p.affected_components, p.affected_symbols,
                    p.strategy, p.prompt_version, p.created_at
             FROM plans p
             JOIN tasks t ON t.id = p.task_id
             WHERE p.task_id = $1 AND t.organization_id = $2 AND p.superseded_by IS NULL
             ORDER BY p.created_at DESC, p.id DESC
             LIMIT 1",
        )
        .bind(task.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)?;

        let Some(rec) = record else { return Ok(None) };

        let step_rows: Vec<(Uuid, i32, String, String, serde_json::Value)> = sqlx::query_as(
            "SELECT id, position, action, verification, risks
             FROM plan_steps WHERE plan_id = $1 ORDER BY position ASC",
        )
        .bind(rec.id)
        .fetch_all(self.pool())
        .await
        .map_err(crate::map_sqlx)?;

        let plan_id = PlanId::from_uuid(rec.id);
        let steps = step_rows
            .into_iter()
            .map(|(id, pos, action, verification, risks)| PlanStep {
                id: StepId::from_uuid(id),
                plan_id,
                position: u32::try_from(pos.max(0)).unwrap_or(0),
                action,
                verification,
                risks: risks
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();

        Ok(Some(Plan {
            id: plan_id,
            task_id: TaskId::from_uuid(task.as_uuid()),
            objective: rec.objective,
            steps,
            affected_components: json_strings(&rec.affected_components),
            affected_symbols: json_strings(&rec.affected_symbols),
            strategy: serde_json::from_value(rec.strategy).unwrap_or_default(),
            created_at: rec.created_at,
            prompt_version: rec.prompt_version,
        }))
    }

    /// Open the plan approval gate for a run if not already open.
    /// Returns the gate id; idempotent under job redelivery.
    pub async fn open_plan_approval(
        &self,
        org: OrganizationId,
        task: TaskId,
        run: WorkflowRunId,
    ) -> Result<Uuid> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT a.id FROM approvals a
             JOIN tasks t ON t.id = a.task_id
             WHERE a.run_id = $1 AND a.gate = 'plan' AND a.decision IS NULL
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
             SELECT $1, t.id, $3, 'plan', 'maintainer'
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

    /// The open plan-approval decision state for a run, if any.
    pub async fn get_plan_approval(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
    ) -> Result<Option<ApprovalRow>> {
        sqlx::query_as::<_, ApprovalRow>(
            "SELECT a.id, a.task_id, a.run_id, a.gate, a.required_role, a.decision
             FROM approvals a
             JOIN tasks t ON t.id = a.task_id
             WHERE a.run_id = $1 AND a.gate = 'plan' AND t.organization_id = $2
             ORDER BY a.created_at DESC LIMIT 1",
        )
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// Record the human decision on the OPEN plan-approval gate of a
    /// run and emit the typed event in the SAME transaction.
    ///
    /// Conflict when no undecided gate exists - double decisions and
    /// decisions on never-gated runs are refused, not absorbed.
    pub async fn decide_plan_approval(
        &self,
        org: OrganizationId,
        run: WorkflowRunId,
        approved: bool,
        principal: &str,
        reason: Option<&str>,
    ) -> Result<Uuid> {
        let mut tx = self.pool().begin().await.map_err(crate::map_sqlx)?;

        let updated: Option<(Uuid, Uuid)> = sqlx::query_as(
            "UPDATE approvals a
                SET decision = $3, decided_by = $4, decided_at = now(), reason = $5
               FROM tasks t
              WHERE a.task_id = t.id AND a.run_id = $1
                AND a.gate = 'plan' AND a.decision IS NULL
                AND t.organization_id = $2
            RETURNING a.id, a.task_id",
        )
        .bind(run.as_uuid())
        .bind(org.as_uuid())
        .bind(if approved { "approved" } else { "rejected" })
        .bind(principal)
        .bind(reason)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::map_sqlx)?;
        let Some((approval_id, task_id)) = updated else {
            return Err(Error::Conflict {
                message: "no open plan approval for this run".into(),
            });
        };

        let payload = serde_json::to_value(hephaestus_core::event::EventPayload::ApprovalDecided {
            approval_id: hephaestus_core::id::HephaestusId(approval_id),
            approved,
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

    /// Remote URL of a repository within an organization.
    pub async fn repository_remote(
        &self,
        org: OrganizationId,
        repo: RepositoryId,
    ) -> Result<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT remote_url FROM repositories WHERE id = $1 AND organization_id = $2",
        )
        .bind(repo.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)?
        .ok_or(Error::NotFound {
            entity: "repository",
        })
    }

    /// Append one task-scoped event inside an existing transaction,
    /// borrowing the run's correlation id for tracing continuity.
    /// `payload` must already be serialized in envelope wire shape
    /// (`{"type": .., "data": ..}`), e.g. from [hephaestus_core::event::EventPayload].
    async fn write_task_event_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        org: OrganizationId,
        task: TaskId,
        run: WorkflowRunId,
        provenance: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO events
               (id, schema_version, organization_id, aggregate, aggregate_id,
                correlation_id, provenance, payload)
             SELECT $1, $2, $3, 'task', $4, correlation_id, $6, $7
             FROM workflow_runs WHERE id = $5",
        )
        .bind(Uuid::now_v7())
        .bind(i32::try_from(hephaestus_core::event::EVENT_ENVELOPE_VERSION).unwrap_or(i32::MAX))
        .bind(org.as_uuid())
        .bind(task.as_uuid())
        .bind(run.as_uuid())
        .bind(provenance)
        .bind(payload)
        .execute(&mut **tx)
        .await
        .map_err(crate::map_sqlx)?;
        Ok(())
    }
}

fn json_strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::tasks::NewTask;
    use crate::testutil::test_db;
    use hephaestus_core::domain::{RequirementCategory, StrategyNotes};
    use hephaestus_core::id::{OrganizationId, ProjectId, RepositoryId};

    async fn seed(db: &Db) -> (OrganizationId, TaskId, WorkflowRunId) {
        let org = OrganizationId::generate();
        let proj = ProjectId::generate();
        let repo = RepositoryId::generate();
        let slug = format!("pln-{}", org.as_uuid().simple());
        sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
            .bind(org.as_uuid())
            .bind(&slug)
            .execute(db.pool())
            .await
            .expect("org");
        sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
            .bind(proj.as_uuid())
            .bind(org.as_uuid())
            .bind(format!("pp-{}", proj.as_uuid().simple()))
            .execute(db.pool())
            .await
            .expect("proj");
        sqlx::query(
            "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
             VALUES ($1,$2,$3,'https://example.invalid/p.git','P')",
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
                title: "planning store test",
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

    fn requirement(task: TaskId, statement: &str, kind: RequirementKind) -> Requirement {
        Requirement::new(
            hephaestus_core::id::RequirementId::generate(),
            task,
            RequirementCategory::Functional,
            kind,
            statement.to_string(),
            "test-source".into(),
            Utc::now(),
        )
        .expect("valid requirement")
    }

    fn plan_with_steps(task: TaskId, n: u32) -> Plan {
        let plan_id = PlanId::generate();
        Plan {
            id: plan_id,
            task_id: task,
            objective: "objective".into(),
            steps: (1..=n)
                .map(|pos| PlanStep {
                    id: StepId::generate(),
                    plan_id,
                    position: pos,
                    action: format!("action {pos}"),
                    verification: format!("unit:check-{pos}"),
                    risks: vec!["risk".into()],
                })
                .collect(),
            affected_components: vec!["comp".into()],
            affected_symbols: vec![],
            strategy: StrategyNotes {
                rollback: Some("revert".into()),
                deployment: None,
                verification: vec!["unit".into()],
            },
            created_at: Utc::now(),
            prompt_version: Some("test-v1".into()),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requirements_replace_and_list_round_trip() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;

        let first = vec![
            requirement(
                task,
                "Upload returns 413 over limit",
                RequirementKind::Explicit,
            ),
            requirement(task, "Maybe auth changed", RequirementKind::Assumption),
        ];
        db.replace_requirements(org, task, run, &first)
            .await
            .expect("replace");
        let listed = db.list_requirements(org, task).await.expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].statement, "Upload returns 413 over limit");
        assert_eq!(listed[0].kind, RequirementKind::Explicit);
        assert!(listed[0].is_gating());
        // Assumption WITH source keeps its epistemic status.
        assert_eq!(listed[1].kind, RequirementKind::Assumption);
        assert!(!listed[1].is_gating());

        // Replace semantics: the new set fully replaces the old.
        let second = vec![requirement(task, "only now", RequirementKind::Inferred)];
        db.replace_requirements(org, task, run, &second)
            .await
            .expect("replace again");
        let listed = db.list_requirements(org, task).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].statement, "only now");

        // Typed event recorded with model_output provenance.
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT provenance, payload->>'type' FROM events
             WHERE aggregate='task' AND aggregate_id=$1 ORDER BY occurred_at",
        )
        .bind(task.as_uuid())
        .fetch_all(db.pool())
        .await
        .expect("events");
        assert!(
            rows.iter()
                .any(|(p, t)| p == "model_output" && t == "requirements_extracted")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requirements_are_tenant_scoped() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;
        let reqs = vec![requirement(
            task,
            "secret org data",
            RequirementKind::Explicit,
        )];
        db.replace_requirements(org, task, run, &reqs)
            .await
            .expect("replace");

        let other = OrganizationId::generate();
        let listed = db.list_requirements(other, task).await.expect("empty list");
        assert!(listed.is_empty(), "foreign org must see nothing");

        let err = db
            .replace_requirements(other, task, run, &reqs)
            .await
            .expect_err("foreign replace must fail");
        assert!(matches!(err, Error::NotFound { entity: "task" }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn plan_supersedes_previous_and_keeps_step_order() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;

        let a = plan_with_steps(task, 2);
        db.create_plan(org, task, run, &a).await.expect("plan a");
        let current = db
            .get_current_plan(org, task)
            .await
            .expect("current")
            .expect("some");
        assert_eq!(current.id, a.id);
        assert_eq!(
            current.steps.iter().map(|s| s.position).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(current.strategy.rollback.as_deref(), Some("revert"));

        let b = plan_with_steps(task, 3);
        db.create_plan(org, task, run, &b).await.expect("plan b");
        let current = db
            .get_current_plan(org, task)
            .await
            .expect("current")
            .expect("some");
        assert_eq!(current.id, b.id, "newest non-superseded plan is current");
        assert_eq!(current.steps.len(), 3);

        let superseded: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM plans WHERE task_id=$1 AND superseded_by IS NOT NULL",
        )
        .bind(task.as_uuid())
        .fetch_one(db.pool())
        .await
        .expect("count");
        assert_eq!(superseded.0, 1);

        // plan_generated event rides the same transaction.
        let typed: Vec<String> = sqlx::query_scalar(
            "SELECT payload->>'type' FROM events WHERE aggregate='task' AND aggregate_id=$1",
        )
        .bind(task.as_uuid())
        .fetch_all(db.pool())
        .await
        .expect("events");
        assert!(typed.iter().any(|t| t == "plan_generated"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_plans_never_reach_sql() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;
        let mut bad = plan_with_steps(task, 2);
        bad.steps[1].position = 5; // gap: positions must be contiguous
        let err = db
            .create_plan(org, task, run, &bad)
            .await
            .expect_err("invalid plan refused");
        assert!(matches!(err, Error::Validation { .. }));
        assert!(
            db.get_current_plan(org, task)
                .await
                .expect("none")
                .is_none()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn approval_gate_opens_exactly_once() {
        let db = test_db().await;
        let (org, task, run) = seed(&db).await;

        let gate = db.open_plan_approval(org, task, run).await.expect("gate");
        let again = db.open_plan_approval(org, task, run).await.expect("again");
        assert_eq!(gate, again, "redelivery must not duplicate the gate");

        let row = db
            .get_plan_approval(org, run)
            .await
            .expect("row")
            .expect("open");
        assert_eq!(row.gate, "plan");
        assert_eq!(row.decision, None);
        assert_eq!(row.task_id, task.as_uuid());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn repository_remote_is_tenant_checked() {
        let db = test_db().await;
        let (org, _task, _run) = seed(&db).await;
        let repo = RepositoryId::generate();
        let proj = ProjectId::generate();
        sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
            .bind(proj.as_uuid())
            .bind(org.as_uuid())
            .bind(format!("rp-{}", proj.as_uuid().simple()))
            .execute(db.pool())
            .await
            .expect("proj");
        sqlx::query(
            "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
             VALUES ($1,$2,$3,'https://example.invalid/r.git','R')",
        )
        .bind(repo.as_uuid())
        .bind(org.as_uuid())
        .bind(proj.as_uuid())
        .execute(db.pool())
        .await
        .expect("repo");

        let url = db.repository_remote(org, repo).await.expect("url");
        assert_eq!(url, "https://example.invalid/r.git");
        let err = db
            .repository_remote(OrganizationId::generate(), repo)
            .await
            .expect_err("foreign org refused");
        assert!(matches!(
            err,
            Error::NotFound {
                entity: "repository"
            }
        ));
    }
}
