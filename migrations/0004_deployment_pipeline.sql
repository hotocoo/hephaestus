-- Deployment pipeline schema (ADR-013).
--
-- A deployment row is bootstrapped by the build stage when the merged
-- change set must ship to a configured target (bootstrap-before-
-- transition, mirroring builds and executions). It carries the
-- deterministic outcome of the deploy command and of the mandatory
-- post-deployment verification hooks. One RUNNING row per run is a
-- storage guarantee via partial unique index, so job redelivery can
-- never double-deploy; finished rows stay as evidence.
--
-- The events table's aggregate CHECK already admits 'deployment'
-- (0001), and the typed DeploymentOutcome payload was declared with
-- the v2 event envelope - this migration adds the durable row only.

CREATE TABLE deployments (
    id               UUID PRIMARY KEY,
    organization_id  UUID NOT NULL REFERENCES organizations(id),
    task_id          UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    run_id           UUID NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    build_id         UUID NOT NULL REFERENCES builds(id) ON DELETE CASCADE,
    target           TEXT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'running',
    failure_reason   TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT deployments_status_ck CHECK (status IN ('running','succeeded','failed')),
    -- A failed row says why; a succeeded row carries no stale reason.
    CONSTRAINT deployments_failure_reason_ck CHECK (
        (status = 'failed') OR (failure_reason IS NULL))
);

CREATE UNIQUE INDEX deployments_run_active_uidx ON deployments (run_id)
    WHERE status = 'running';
CREATE INDEX deployments_run_idx ON deployments (run_id);
CREATE INDEX deployments_org_idx ON deployments (organization_id);
