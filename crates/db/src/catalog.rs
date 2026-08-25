//! Tenant catalog reads: projects and repositories.
//!
//! The API layer needs read-only discovery surfaces so clients can
//! learn which projects and repositories exist inside their tenant
//! before submitting tasks. These queries are strictly scoped by
//! organization like every other store query.

use uuid::Uuid;

use hephaestus_core::Error;
use hephaestus_core::Result;
use hephaestus_core::id::{OrganizationId, ProjectId};

use crate::store::Db;

/// A persisted project row (catalog view).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProjectRow {
    /// Project id.
    pub id: Uuid,
    /// Tenant scope.
    pub organization_id: Uuid,
    /// Display name.
    pub name: String,
    /// URL-safe slug, unique per organization.
    pub slug: String,
}

/// A persisted repository row (catalog view).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RepositoryRow {
    /// Repository id.
    pub id: Uuid,
    /// Tenant scope.
    pub organization_id: Uuid,
    /// Owning project.
    pub project_id: Uuid,
    /// Remote URL the pipeline clones from.
    pub remote_url: String,
    /// Branch intake snapshots default to.
    pub default_branch: String,
    /// Display name.
    pub display_name: String,
}

impl Db {
    /// List projects for an organization, oldest first, bounded page.
    pub async fn list_projects(
        &self,
        org: OrganizationId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ProjectRow>> {
        let limit = limit.clamp(1, 200);
        let offset = offset.max(0);
        sqlx::query_as::<_, ProjectRow>(
            "SELECT id, organization_id, name, slug
             FROM projects WHERE organization_id = $1
             ORDER BY created_at ASC, id ASC LIMIT $2 OFFSET $3",
        )
        .bind(org.as_uuid())
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// List repositories for an organization, optionally narrowed to
    /// one project. Oldest first, bounded page.
    pub async fn list_repositories(
        &self,
        org: OrganizationId,
        project: Option<ProjectId>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<RepositoryRow>> {
        let limit = limit.clamp(1, 200);
        let offset = offset.max(0);
        match project {
            Some(project) => sqlx::query_as::<_, RepositoryRow>(
                "SELECT id, organization_id, project_id, remote_url, default_branch, display_name
                     FROM repositories
                     WHERE organization_id = $1 AND project_id = $2
                     ORDER BY created_at ASC, id ASC LIMIT $3 OFFSET $4",
            )
            .bind(org.as_uuid())
            .bind(project.as_uuid())
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pool())
            .await
            .map_err(crate::map_sqlx),
            None => sqlx::query_as::<_, RepositoryRow>(
                "SELECT id, organization_id, project_id, remote_url, default_branch, display_name
                     FROM repositories
                     WHERE organization_id = $1
                     ORDER BY created_at ASC, id ASC LIMIT $2 OFFSET $3",
            )
            .bind(org.as_uuid())
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pool())
            .await
            .map_err(crate::map_sqlx),
        }
    }

    /// Fetch one project scoped to an organization.
    pub async fn get_project(&self, org: OrganizationId, id: ProjectId) -> Result<ProjectRow> {
        sqlx::query_as::<_, ProjectRow>(
            "SELECT id, organization_id, name, slug
             FROM projects WHERE id = $1 AND organization_id = $2",
        )
        .bind(id.as_uuid())
        .bind(org.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(crate::map_sqlx)?
        .ok_or(Error::NotFound { entity: "project" })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::testutil::test_db;
    use hephaestus_core::id::RepositoryId;

    async fn seed(db: &Db) -> (OrganizationId, ProjectId, RepositoryId) {
        let org = OrganizationId::generate();
        let proj = ProjectId::generate();
        let repo = RepositoryId::generate();
        let slug = format!("cat-{}", org.as_uuid().simple());
        sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
            .bind(org.as_uuid())
            .bind(&slug)
            .execute(db.pool())
            .await
            .expect("org insert");
        sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
            .bind(proj.as_uuid())
            .bind(org.as_uuid())
            .bind(format!("cp-{}", proj.as_uuid().simple()))
            .execute(db.pool())
            .await
            .expect("proj insert");
        sqlx::query(
            "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
             VALUES ($1,$2,$3,'https://example.invalid/c.git','C')",
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
    async fn lists_are_tenant_scoped() {
        let db = test_db().await;
        let (org, proj, repo) = seed(&db).await;

        let projects = db.list_projects(org, 50, 0).await.expect("projects");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, proj.as_uuid());

        let repos = db.list_repositories(org, None, 50, 0).await.expect("repos");
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].id, repo.as_uuid());
        assert_eq!(repos[0].default_branch, "main");

        let in_project = db
            .list_repositories(org, Some(proj), 50, 0)
            .await
            .expect("repos by project");
        assert_eq!(in_project.len(), 1);

        // Another tenant sees nothing of this catalog.
        let other = OrganizationId::generate();
        assert!(
            db.list_projects(other, 50, 0)
                .await
                .expect("empty")
                .is_empty()
        );
        assert!(
            db.list_repositories(other, None, 50, 0)
                .await
                .expect("empty")
                .is_empty()
        );

        // Direct fetch is scoped too.
        let fetched = db.get_project(org, proj).await.expect("project");
        assert_eq!(fetched.slug, projects[0].slug);
        assert!(matches!(
            db.get_project(other, proj).await,
            Err(Error::NotFound { entity: "project" })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pagination_bounds_apply() {
        let db = test_db().await;
        let (org, _proj, _repo) = seed(&db).await;
        let first_page = db.list_projects(org, 1, 0).await.expect("page one");
        assert_eq!(first_page.len(), 1);
        let second_page = db.list_projects(org, 1, 1).await.expect("page two");
        assert!(second_page.is_empty());
        // Out-of-range values clamp instead of erroring.
        let clamped = db.list_projects(org, 0, -5).await.expect("clamped");
        assert!(!clamped.is_empty());
    }
}
