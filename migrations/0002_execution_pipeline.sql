-- Execution pipeline schema.
--
-- One ACTIVE execution per workflow run keeps job routing
-- unambiguous: the partial unique index enforces it at the storage
-- layer, not by check-then-insert races.
--
-- Plan steps are SNAPSHOTTED into execution_steps when the execution
-- starts. The execution owns its copy of progress; later plan
-- supersession cannot mutate history that already ran.

CREATE TABLE executions (
    id               UUID PRIMARY KEY,
    organization_id  UUID NOT NULL REFERENCES organizations(id),
    task_id          UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    run_id           UUID NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    plan_id          UUID NOT NULL REFERENCES plans(id),
    status           TEXT NOT NULL DEFAULT 'active',
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT executions_status_ck CHECK (status IN ('active','passed','failed'))
);

-- Exactly one live execution per run; finished executions keep their
-- rows as evidence of prior attempts.
CREATE UNIQUE INDEX executions_run_active_uidx ON executions (run_id)
    WHERE status = 'active';
CREATE INDEX executions_run_idx ON executions (run_id);
CREATE INDEX executions_org_idx ON executions (organization_id);

CREATE TABLE execution_steps (
    id            UUID PRIMARY KEY,
    execution_id  UUID NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    step_id       UUID NOT NULL REFERENCES plan_steps(id),
    position      INT  NOT NULL,
    action        TEXT NOT NULL,
    verification  TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'pending',
    summary       TEXT NOT NULL DEFAULT '',
    files_changed JSONB NOT NULL DEFAULT '[]'::jsonb,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT execution_steps_status_ck CHECK (status IN ('pending','completed','failed')),
    CONSTRAINT execution_steps_pos_ck CHECK (position >= 1),
    CONSTRAINT execution_steps_summary_len_ck CHECK (char_length(summary) BETWEEN 0 AND 8192)
);

CREATE UNIQUE INDEX execution_steps_exec_step_uidx ON execution_steps (execution_id, step_id);
CREATE INDEX execution_steps_exec_pos_idx ON execution_steps (execution_id, position);

-- Append-only verification evidence. Every suite run records its own
-- row keyed by cycle (= prior row count for the execution); the count
-- of FAILED rows is the fix-loop budget ledger owned by the execution
-- layer (ADR-007).
CREATE TABLE verifications (
    id               UUID PRIMARY KEY,
    organization_id  UUID NOT NULL REFERENCES organizations(id),
    execution_id     UUID NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    run_id           UUID NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    cycle            INT  NOT NULL,
    passed           BOOLEAN NOT NULL,
    report           JSONB NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT verifications_cycle_ck CHECK (cycle >= 1)
);

CREATE INDEX verifications_exec_cycle_idx ON verifications (execution_id, cycle);
