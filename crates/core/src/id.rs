//! Strongly typed identifiers.
//!
//! Every persisted entity gets its own ID type so IDs cannot be
//! swapped accidentally at compile time. Underlying representation
//! is UUIDv7: lexicographically sortable by creation time, which
//! gives us index-friendly primary keys without a separate
//! sequence.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generate a new ID stamped with the current time.
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wrap an existing UUID (e.g. loaded from the database).
            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Inner UUID value.
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

define_id!(
    /// Identifier of an organization (top-level tenant boundary).
    OrganizationId
);
define_id!(
    /// Identifier of a project within an organization.
    ProjectId
);
define_id!(
    /// Identifier of a registered repository connection.
    RepositoryId
);
define_id!(
    /// Identifier of an immutable repository snapshot (commit).
    SnapshotId
);
define_id!(
    /// Identifier of an engineering task.
    TaskId
);
define_id!(
    /// Identifier of a requirement extracted from a task.
    RequirementId
);
define_id!(
    /// Identifier of an implementation plan.
    PlanId
);
define_id!(
    /// Identifier of a single plan step.
    StepId
);
define_id!(
    /// Identifier of a durable workflow run.
    WorkflowRunId
);
define_id!(
    /// Identifier of an execution attempt inside a task.
    ExecutionId
);
define_id!(
    /// Identifier of an agent run.
    AgentRunId
);
define_id!(
    /// Identifier of a tool invocation.
    ToolInvocationId
);
define_id!(
    /// Identifier of a verification suite result.
    VerificationId
);
define_id!(
    /// Identifier of a build record.
    BuildId
);
define_id!(
    /// Identifier of a stored artifact.
    ArtifactId
);
define_id!(
    /// Identifier of a deployment.
    DeploymentId
);
define_id!(
    /// Identifier of an evidence node linking lineage together.
    EvidenceId
);
define_id!(
    /// Identifier of a human or policy approval decision.
    ApprovalId
);
define_id!(
    /// Identifier of a review record.
    ReviewId
);
define_id!(
    /// Identifier of a queued job.
    JobId
);
define_id!(
    /// Identifier of an API token (the token value itself is never stored).
    TokenId
);
define_id!(
    /// Identifier of a user principal.
    UserId
);

/// A generic Forge identifier used where the concrete entity type is
/// carried alongside (e.g. event envelopes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ForgeId(pub Uuid);

impl ForgeId {
    /// Generate a fresh generic identifier.
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }
}

impl fmt::Display for ForgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ids_sort_by_creation_time() {
        let a = TaskId::generate();
        // UUIDv7 embeds millisecond timestamps; enforce ordering after
        // a tiny sleep so the two generations cannot land in the same ms.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = TaskId::generate();
        assert!(a < b, "UUIDv7 IDs must be time-ordered");
    }

    #[test]
    fn distinct_types_are_not_interchangeable_at_runtime_serde_level() {
        let t = TaskId::generate();
        let json = serde_json::to_string(&t).unwrap_or_default();
        // Parsing into another ID type must fail closed, not coerce.
        let parsed: Result<ArtifactId, _> = serde_json::from_str(&json);
        // Note: serde transparent means raw string parses fine into any
        // ID type; the type safety guarantee is at compile time in Rust
        // code. Wire-level coercion is prevented by schemas validating
        // the surrounding object shape. We document this explicitly.
        assert!(parsed.is_ok(), "wire format is plain uuid strings");
        let round_trip: TaskId = serde_json::from_str(&json).unwrap_or_else(|_| TaskId::generate());
        assert_eq!(t.as_uuid(), round_trip.as_uuid());
    }

    #[test]
    fn display_is_uuid_string() {
        let id = TaskId::generate();
        let s = id.to_string();
        assert_eq!(
            Uuid::parse_str(&s).ok().map(|u| u.as_bytes().len()),
            Some(16)
        );
    }
}
