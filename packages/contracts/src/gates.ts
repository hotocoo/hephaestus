import { z } from "zod";
import { Uuid } from "./helpers.ts";

/** A human decision point as stored. */
export const Gate = z.strictObject({
  approval_id: Uuid,
  gate: z.enum(["plan", "merge"]),
  required_role: z.string(),
  /** Recorded decision name when made. */
  decision: z.string().nullable(),
});
export type Gate = z.infer<typeof Gate>;

/** Plan-approval decision body; the approver is the authenticated principal. */
export const ApprovalDecisionInput = z.object({
  approved: z.boolean(),
  reason: z.string().nullish(),
});
export type ApprovalDecisionInput = z.infer<typeof ApprovalDecisionInput>;

/** Outcome of applying a plan-approval decision (tagged union). */
export const ApprovalOutcome = z.discriminatedUnion("outcome", [
  z.strictObject({
    outcome: z.literal("approved"),
    execution_id: Uuid,
    enqueued_step: Uuid.nullable(),
  }),
  z.strictObject({
    outcome: z.literal("rejected"),
    approval_id: Uuid,
  }),
]);
export type ApprovalOutcome = z.infer<typeof ApprovalOutcome>;

/**
 * Merge decision body. The merger is the authenticated principal and
 * is never accepted from the body.
 */
export const MergeDecisionInput = z.object({
  reason: z.string().nullish(),
  external_ref: z.string().nullish(),
});
export type MergeDecisionInput = z.infer<typeof MergeDecisionInput>;

/** Outcome of applying a merge decision (tagged union). */
export const MergeOutcome = z.discriminatedUnion("outcome", [
  z.strictObject({ outcome: z.literal("merged") }),
  z.strictObject({ outcome: z.literal("already_merged") }),
]);
export type MergeOutcome = z.infer<typeof MergeOutcome>;
