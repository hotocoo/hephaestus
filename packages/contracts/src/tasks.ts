import { z } from "zod";
import { IsoDateTime, Uuid } from "./helpers.ts";

const Priority = z.enum(["critical", "high", "medium", "low"]);
const RiskLevel = z.enum(["low", "medium", "high", "critical"]);

/** A task as stored. */
export const Task = z.strictObject({
  id: Uuid,
  organization_id: Uuid,
  project_id: Uuid,
  repository_id: Uuid,
  title: z.string(),
  description: z.string(),
  priority: Priority,
  risk: RiskLevel,
  /** Normalized (trimmed, lowercased, deduplicated) label array. */
  labels: z.array(z.string()),
  created_at: IsoDateTime,
  updated_at: IsoDateTime,
});
export type Task = z.infer<typeof Task>;

/**
 * Task intake body. The server ignores unknown fields here, so the
 * mirror is deliberately open; response mirrors are strict instead.
 */
export const CreateTaskInput = z.object({
  title: z.string().min(1).max(512),
  description: z.string(),
  project_id: Uuid,
  repository_id: Uuid,
  priority: Priority.nullish(),
  risk: RiskLevel.nullish(),
  labels: z.array(z.string()).nullish(),
});
export type CreateTaskInput = z.infer<typeof CreateTaskInput>;

/** Receipt of an accepted (or deduplicated) task submission. */
export const IntakeReceipt = z.strictObject({
  task_id: Uuid,
  run_id: Uuid,
  deduplicated: z.boolean(),
});
export type IntakeReceipt = z.infer<typeof IntakeReceipt>;
