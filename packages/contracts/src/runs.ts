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
