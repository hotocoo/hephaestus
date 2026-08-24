//! Append-only event log access.

use chrono::{DateTime, Utc};
use hephaestus_core::event::{AggregateKind, EventEnvelope, EventPayload, Provenance};
use hephaestus_core::{Error, Result};
use uuid::Uuid;

use hephaestus_core::id::{HephaestusId, OrganizationId};

use crate::store::Db;

impl Db {
    /// Append an envelope inside an existing transaction OR standalone.
    ///
    /// Standalone version; workflow transitions embed their own event
    /// write in the same transaction as the state change.
    pub async fn append_event(&self, env: &EventEnvelope) -> Result<()> {
        let payload = serde_json::to_value(&env.payload)
            .map_err(|e| hephaestus_core::Error::Storage(Box::new(e)))?;
        sqlx::query(
            "INSERT INTO events
               (id, schema_version, organization_id, aggregate, aggregate_id,
                correlation_id, provenance, payload, occurred_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(env.id.0)
        .bind(env.schema_version as i32)
        .bind(env.organization_id.as_uuid())
        .bind(aggregate_name(env.aggregate))
        .bind(env.aggregate_id.0)
        .bind(env.correlation_id)
        .bind(provenance_name(env.provenance))
        .bind(payload)
        .bind(env.occurred_at)
        .execute(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(())
    }

    /// List events for an aggregate, oldest first.
    pub async fn list_events(
        &self,
        org: OrganizationId,
        aggregate: AggregateKind,
        aggregate_id: HephaestusId,
        limit: i64,
    ) -> Result<Vec<EventRecord>> {
        let limit = limit.clamp(1, 1000);
        sqlx::query_as::<_, EventRecord>(
            "SELECT id, aggregate, aggregate_id, provenance, payload, occurred_at
             FROM events
             WHERE organization_id = $1 AND aggregate = $2 AND aggregate_id = $3
             ORDER BY occurred_at ASC LIMIT $4",
        )
        .bind(org.as_uuid())
        .bind(aggregate_name(aggregate))
        .bind(aggregate_id.0)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }

    /// List events for an aggregate with typed payloads.
    ///
    /// Returns (provenance, payload) pairs decoded from storage.
    /// Records whose payload fails to decode are skipped and counted:
    /// forward compatibility means old readers must not crash when new
    /// event versions exist. The skip count lets callers notice drift.
    pub async fn list_typed_events(
        &self,
        org: OrganizationId,
        aggregate: AggregateKind,
        aggregate_id: HephaestusId,
        limit: i64,
    ) -> Result<(Vec<(Provenance, EventPayload)>, u64)> {
        let records = self
            .list_events(org, aggregate, aggregate_id, limit)
            .await?;
        let mut out = Vec::with_capacity(records.len());
        let mut skipped = 0u64;
        for rec in &records {
            let prov = parse_provenance(&rec.provenance);
            let agg = parse_aggregate(&rec.aggregate);
            let payload = decode_payload(rec);
            match (prov, agg, payload) {
                (Ok(p), Ok(a), Ok(payload)) if a == aggregate => {
                    out.push((p, payload));
                }
                // Wrong aggregate or unknown future schema: skip, count.
                _ => skipped += 1,
            }
        }
        Ok((out, skipped))
    }

    /// List events across an organization within a time window.
    pub async fn list_recent_events(
        &self,
        org: OrganizationId,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<EventRecord>> {
        sqlx::query_as::<_, EventRecord>(
            "SELECT id, aggregate, aggregate_id, provenance, payload, occurred_at
             FROM events WHERE organization_id = $1 AND occurred_at >= $2
             ORDER BY occurred_at DESC LIMIT $3",
        )
        .bind(org.as_uuid())
        .bind(since)
        .bind(limit.clamp(1, 1000))
        .fetch_all(self.pool())
        .await
        .map_err(crate::map_sqlx)
    }
}

/// A raw event record as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EventRecord {
    /// Event id.
    pub id: Uuid,
    /// Aggregate kind name.
    pub aggregate: String,
    /// Aggregate instance id.
    pub aggregate_id: Uuid,
    /// Provenance name.
    pub provenance: String,
    /// Payload JSON.
    pub payload: serde_json::Value,
    /// Occurrence time.
    pub occurred_at: DateTime<Utc>,
}

/// Decode a stored record's payload into a typed event payload.
///
/// Unknown future payloads return a validation error: consumers must
/// handle forward compatibility explicitly rather than crashing.
/// Provenance and timestamps are read directly from the record.
pub fn decode_payload(rec: &EventRecord) -> Result<EventPayload> {
    serde_json::from_value(rec.payload.clone()).map_err(|_| Error::Validation {
        field: "payload".into(),
        message: "unknown or malformed event payload".into(),
    })
}

fn aggregate_name(a: AggregateKind) -> &'static str {
    match a {
        AggregateKind::Organization => "organization",
        AggregateKind::Project => "project",
        AggregateKind::Repository => "repository",
        AggregateKind::Task => "task",
        AggregateKind::WorkflowRun => "workflow_run",
        AggregateKind::Execution => "execution",
        AggregateKind::AgentRun => "agent_run",
        AggregateKind::Build => "build",
        AggregateKind::Deployment => "deployment",
        AggregateKind::Artifact => "artifact",
    }
}

fn parse_aggregate(s: &str) -> Result<AggregateKind> {
    Ok(match s {
        "organization" => AggregateKind::Organization,
        "project" => AggregateKind::Project,
        "repository" => AggregateKind::Repository,
        "task" => AggregateKind::Task,
        "workflow_run" => AggregateKind::WorkflowRun,
        "execution" => AggregateKind::Execution,
        "agent_run" => AggregateKind::AgentRun,
        "build" => AggregateKind::Build,
        "deployment" => AggregateKind::Deployment,
        "artifact" => AggregateKind::Artifact,
        _ => {
            return Err(Error::Validation {
                field: "aggregate".into(),
                message: format!("unknown aggregate kind {s:?}"),
            });
        }
    })
}

fn provenance_name(p: Provenance) -> &'static str {
    match p {
        Provenance::System => "system",
        Provenance::User => "user",
        Provenance::TaskInput => "task_input",
        Provenance::RepositoryData => "repository_data",
        Provenance::ToolResult => "tool_result",
        Provenance::ModelOutput => "model_output",
        Provenance::Computed => "computed",
    }
}

fn parse_provenance(s: &str) -> Result<Provenance> {
    Ok(match s {
        "system" => Provenance::System,
        "user" => Provenance::User,
        "task_input" => Provenance::TaskInput,
        "repository_data" => Provenance::RepositoryData,
        "tool_result" => Provenance::ToolResult,
        "model_output" => Provenance::ModelOutput,
        "computed" => Provenance::Computed,
        _ => {
            return Err(Error::Validation {
                field: "provenance".into(),
                message: format!("unknown provenance {s:?}"),
            });
        }
    })
}
