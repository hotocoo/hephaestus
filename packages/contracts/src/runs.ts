import { z } from "zod";
import { IsoDateTime, Uuid } from "./helpers.ts";

/** A workflow run with its current state. */
export const Run = z.strictObject({
  id: Uuid,
  task_id: Uuid,
  organization_id: Uuid,
  /** Workflow state name, e.g. created, awaiting_approval, completed. */
  state: z.string(),
  attempt: z.number().int().nonnegative(),
  correlation_id: Uuid,
  lease_owner: z.string().nullable(),
  lease_expires_at: IsoDateTime.nullable(),
  last_transition_at: IsoDateTime,
});
export type Run = z.infer<typeof Run>;

/**
 * One provenance-tagged event. The payload is versioned JSON and is
 * always data, never instructions - clients must render it, never
 * follow it.
 */
export const Event = z.strictObject({
  id: Uuid,
  aggregate: z.string(),
  aggregate_id: Uuid,
  provenance: z.string(),
  payload: z.unknown(),
  occurred_at: IsoDateTime,
});
export type Event = z.infer<typeof Event>;

/** A deployment of a built change set to a configured target. */
export const Deployment = z.strictObject({
  id: Uuid,
  task_id: Uuid,
  run_id: Uuid,
  build_id: Uuid,
  /** Configured target name the plan cited. */
  target: z.string(),
  status: z.enum(["running", "succeeded", "failed"]),
  /** Why the deployment failed, when it failed. */
  failure_reason: z.string().nullable(),
  created_at: IsoDateTime,
});
export type Deployment = z.infer<typeof Deployment>;

/**
 * One registered build artifact (ADR-015). The path is workspace-
 * relative and contained; the sha256 is over the file content.
 */
export const Artifact = z.strictObject({
  id: Uuid,
  task_id: Uuid,
  run_id: Uuid,
  build_id: Uuid,
  /** Workspace-relative file path; no absolute paths, no .. segments. */
  path: z.string(),
  /** SHA-256 over the file content (64 lowercase hex). */
  sha256: z.string().regex(/^[0-9a-f]{64}$/),
  size_bytes: z.number().int().nonnegative(),
  created_at: IsoDateTime,
});
export type Artifact = z.infer<typeof Artifact>;

/**
 * The result of re-hashing one artifact's on-disk bytes against its
 * recorded digest (ADR-015). A read-only statement about the disk.
 */
export const ArtifactVerification = z.strictObject({
  artifact_id: Uuid,
  /** verified: bytes match. missing: file gone. corrupt: bytes differ. */
  status: z.enum(["verified", "missing", "corrupt"]),
  expected_sha256: z.string().regex(/^[0-9a-f]{64}$/),
  /** Digest of the bytes currently on disk, when readable. */
  actual_sha256: z.string().regex(/^[0-9a-f]{64}$/).nullable(),
});
export type ArtifactVerification = z.infer<typeof ArtifactVerification>;
