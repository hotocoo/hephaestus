-- Delivery pipeline schema (ADR-008).
--
-- The merge gate itself reuses the existing approvals table (gate =
-- 'merge', already permitted by 0001's CHECK): a merge is a human or
-- CI decision recorded exactly like any other gate decision, not a
-- new mechanism.
--
-- This migration adds durable BUILD evidence. A build row is created
-- when the merge decision is applied (bootstrap-before-transition,
-- mirroring executions) and carries the deterministic outcome of
-- building the merged change set. One RUNNING build per run is a
-- storage guarantee via partial unique index, so job redelivery can
-- never double-build.

CREATE TABLE builds (
    id               UUID PRIMARY KEY,
    organization_id  UUID NOT NULL REFERENCES organizations(id),
    task_id          UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    run_id           UUID NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    execution_id     UUID NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    status           TEXT NOT NULL DEFAULT 'running',
    artifact_path    TEXT,
    artifact_sha256  TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT builds_status_ck CHECK (status IN ('running','succeeded','failed')),
    CONSTRAINT builds_sha_format_ck CHECK (
        artifact_sha256 IS NULL OR artifact_sha256 ~ '^[0-9a-f]{64}$')
);

-- Exactly one live build per run; finished builds keep their rows as
-- evidence of prior attempts.
CREATE UNIQUE INDEX builds_run_active_uidx ON builds (run_id)
    WHERE status = 'running';
CREATE INDEX builds_run_idx ON builds (run_id);
CREATE INDEX builds_org_idx ON builds (organization_id);
