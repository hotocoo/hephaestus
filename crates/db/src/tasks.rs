//! Task persistence.

use chrono::{DateTime, Utc};
use hephaestus_core::{Error, Result};
use serde_json::json;
use uuid::Uuid;

use hephaestus_core::id::{OrganizationId, ProjectId, RepositoryId, RequirementId, TaskId};

use crate::store::Db;

/// A persisted task row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskRow {
    /// Task id.
    pub id: Uuid,
    /// Tenant scope.
    pub organization_id: Uuid,
    /// Project scope.
    pub project_id: Uuid,
    /// Repository scope.
    pub repository_id: Uuid,
    /// Title.
    pub title: String,
    /// Description.
    pub description: String,
    /// Priority value.
    pub priority: String,
    /// Risk value.
    pub risk: String,
    /// Labels array (JSON).
    pub labels: serde_json::Value,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

/// Create a task. Idempotent per (organization, idempotency_key):
/// a repeated call returns the existing row instead of erroring.
pub struct NewTask<'a> {
    /// Organization scope.
    pub organization_id: OrganizationId,
    /// Project scope.
    pub project_id: ProjectId,
    /// Repository scope.
    pub repository_id: RepositoryId,
    /// Title.
    pub title: &'a str,
    /// Description.
    pub description: &'a str,
    /// Priority.
    pub priority: &'a str,
    /// Risk.
    pub risk: &'a str,
    /// Labels.
    pub labels: &'a [String],
    /// Optional idempotency key.
    pub idempotency_key: Option<&'a str>,
}

impl Db {
    /// Insert a task, returning its id.
    pub async fn create_task(&self, t: &NewTask<'_>) -> Result<TaskId> {
        let labels = json!(t.labels);
        let id = TaskId::generate();
        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO tasks
               (id, organization_id, project_id, repository_id,
                title, description, priority, risk, labels, idempotency_key)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
             ON CONFLICT (organization_id, idempotency_key)
               WHERE idempotency_key IS NOT NULL
             DO UPDATE SET updated_at = tasks.updated_at
             RETURNING id",
        )
        .bind(id.as_uuid())
        .bind(t.organization_id.as_uuid())
        .bind(t.project_id.as_uuid())
        .bind(t.repository_id.as_uuid())
        .bind(t.title)
        .bind(t.description)
        .bind(t.priority)
        .bind(t.risk)
        .bind(labels)
        .bind(t.idempotency_key)
        .fetch_one(self.pool())
        .await
        .map_err(crate::map_sqlx)
        .map(TaskId::from_uuid)
    }

    /// Fetch a task scoped to an organization.
    pub async fn get_task(&self, org: OrganizationId, id: TaskId) -> Result<TaskRow> {
        sqlx::query_as::<_, TaskRow>(
            "SELECT id, organization_id, project_id, repository_id,
                    title, description, priority, risk, labels, created_at, updated_at
             FROM tasks WHERE id = $1 AND organization_id = $2",
        )
        .bind(id.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)?
        .ok_or(Error::NotFound { entity: "task" })
    }

    /// List tasks for an organization, newest first, bounded page.
    pub async fn list_tasks(
        &self,
        org: OrganizationId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<TaskRow>> {
        let limit = limit.clamp(1, 200);
        let offset = offset.max(0);
        sqlx::query_as::<_, TaskRow>(
            "SELECT id, organization_id, project_id, repository_id,
                    title, description, priority, risk, labels, created_at, updated_at
             FROM tasks WHERE organization_id = $1
             ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(org.as_uuid())
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// Insert a requirement for a task.
    pub async fn add_requirement(
        &self,
        task_id: TaskId,
        category: &str,
        kind: &str,
        statement: &str,
        source: &str,
        gating: bool,
    ) -> Result<RequirementId> {
        let id = RequirementId::generate();
        sqlx::query(
            "INSERT INTO requirements
                (id, task_id, category, kind, statement, source, gating)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(id.as_uuid())
        .bind(task_id.as_uuid())
        .bind(category)
        .bind(kind)
        .bind(statement)
        .bind(source)
        .bind(gating)
        .execute(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(id)
    }

    /// Count requirements by kind for a task (requirement engine stats).
    pub async fn count_requirements_by_kind(
        &self,
        task_id: TaskId,
    ) -> Result<std::collections::HashMap<String, i64>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT kind, COUNT(*) FROM requirements WHERE task_id = $1 GROUP BY kind",
        )
        .bind(task_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(rows.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::testutil::test_db;

    async fn seed(db: &Db) -> (OrganizationId, ProjectId, RepositoryId) {
        let org = OrganizationId::generate();
        let proj = ProjectId::generate();
        let repo = RepositoryId::generate();
        let slug = format!("org-{}", org.as_uuid().simple());
        sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'Test',$2)")
            .bind(org.as_uuid())
            .bind(&slug)
            .execute(db.pool())
            .await
            .expect("org insert");
        sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
            .bind(proj.as_uuid())
            .bind(org.as_uuid())
            .bind(format!("p-{}", proj.as_uuid().simple()))
            .execute(db.pool())
            .await
            .expect("proj insert");
        sqlx::query(
            "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
             VALUES ($1,$2,$3,'https://example.invalid/x.git','X')",
        )
        .bind(repo.as_uuid())
        .bind(org.as_uuid())
        .bind(proj.as_uuid())
        .execute(db.pool())
        .await
        .expect("repo insert");
        (org, proj, repo)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_and_get_task_scoped() {
        let db = test_db().await;
        let (org, proj, repo) = seed(&db).await;
        let id = db
            .create_task(&NewTask {
                organization_id: org,
                project_id: proj,
                repository_id: repo,
                title: "Fix null deref",
                description: "d",
                priority: "high",
                risk: "medium",
                labels: &["bug".into()],
                idempotency_key: None,
            })
            .await
            .expect("insert");
        let row = db.get_task(org, id).await.expect("get");
        assert_eq!(row.title, "Fix null deref");
        assert_eq!(row.priority, "high");

        // Tenant isolation: another org cannot see it.
        let other_org = OrganizationId::generate();
        let res = db.get_task(other_org, id).await;
        assert!(matches!(res, Err(Error::NotFound { entity: "task" })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn idempotency_key_returns_same_task() {
        let db = test_db().await;
        let (org, proj, repo) = seed(&db).await;
        let mk = |k: Option<&'static str>| NewTask {
            organization_id: org,
            project_id: proj,
            repository_id: repo,
            title: "same",
            description: "",
            priority: "low",
            risk: "low",
            labels: &[],
            idempotency_key: k,
        };
        let a = db.create_task(&mk(Some("key-1"))).await.expect("first");
        let b = db.create_task(&mk(Some("key-1"))).await.expect("second");
        assert_eq!(a, b, "idempotent create must return same id");
        let c = db.create_task(&mk(None)).await.expect("third");
        assert_ne!(a, c);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requirement_kinds_counted() {
        let db = test_db().await;
        let (org, proj, repo) = seed(&db).await;
        let tid = db
            .create_task(&NewTask {
                organization_id: org,
                project_id: proj,
                repository_id: repo,
                title: "req test",
                description: "",
                priority: "medium",
                risk: "low",
                labels: &[],
                idempotency_key: None,
            })
            .await
            .expect("task");
        db.add_requirement(
            tid,
            "functional",
            "explicit",
            "Upload returns 413 over limit",
            "task-input",
            true,
        )
        .await
        .expect("req1");
        db.add_requirement(
            tid,
            "security",
            "assumption",
            "Maybe auth changed",
            "",
            false,
        )
        .await
        .expect("req2");
        let counts = db.count_requirements_by_kind(tid).await.expect("counts");
        assert_eq!(counts.get("explicit"), Some(&1));
        assert_eq!(counts.get("assumption"), Some(&1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_tasks_paginates() {
        let db = test_db().await;
        let (org, proj, repo) = seed(&db).await;
        for i in 0..5 {
            db.create_task(&NewTask {
                organization_id: org,
                project_id: proj,
                repository_id: repo,
                title: Box::leak(format!("task-{i}").into_boxed_str()),
                description: "",
                priority: "low",
                risk: "low",
                labels: &[],
                idempotency_key: None,
            })
            .await
            .expect("task");
        }
        let page = db.list_tasks(org, 3, 0).await.expect("page1");
        assert_eq!(page.len(), 3);
        let page2 = db.list_tasks(org, 3, 3).await.expect("page2");
        assert_eq!(page2.len(), 2);
    }
}
