-- Artifact registry schema (ADR-015).
--
-- One row per executable file a successful build produced, recorded
-- in the SAME transaction that closes the build row, so build state
-- and per-file evidence never diverge. Paths are workspace-relative
-- and validated at both ends: absolute paths and `..` segments are
-- refused here as defense-in-depth beside the Rust-side validation,
-- and reads resolve only under the run's workspace directory.
-- Rebuilding a run appends a new evidence set under its own build id;
-- rows are never mutated or deleted.

CREATE TABLE artifacts (
    id               UUID PRIMARY KEY,
    organization_id  UUID NOT NULL REFERENCES organizations(id),
    task_id          UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    run_id           UUID NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    build_id         UUID NOT NULL REFERENCES builds(id) ON DELETE CASCADE,
    path             TEXT NOT NULL,
    sha256           TEXT NOT NULL,
    size_bytes       BIGINT NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT artifacts_path_ck CHECK (
        path <> ''
        AND path !~ '^/'
        AND path !~ '(^|/)\.\.(/|$)'),
    CONSTRAINT artifacts_sha_format_ck CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT artifacts_size_ck CHECK (size_bytes >= 0),
    CONSTRAINT artifacts_build_path_uidx UNIQUE (build_id, path)
);

CREATE INDEX artifacts_run_idx ON artifacts (run_id);
CREATE INDEX artifacts_org_idx ON artifacts (organization_id);
