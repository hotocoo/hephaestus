import { z } from "zod";
import { IsoDateTime, Uuid } from "./helpers.ts";

/** One executable step of a plan. */
export const PlanStep = z.strictObject({
  id: Uuid,
  plan_id: Uuid,
  /** 1-based order of execution; contiguous across a plan. */
  position: z.number().int().min(1),
  action: z.string(),
  verification: z.string(),
  risks: z.array(z.string()),
});
export type PlanStep = z.infer<typeof PlanStep>;

/** Rollback / deployment / verification strategy summary. */
export const StrategyNotes = z.strictObject({
  rollback: z.string().nullable(),
  deployment: z.string().nullable(),
  verification: z.array(z.string()),
});
export type StrategyNotes = z.infer<typeof StrategyNotes>;

/** An implementation plan (the versioned domain document). */
export const Plan = z.strictObject({
  id: Uuid,
  task_id: Uuid,
  objective: z.string(),
  steps: z.array(PlanStep).min(1),
  affected_components: z.array(z.string()),
  affected_symbols: z.array(z.string()),
  strategy: StrategyNotes,
  created_at: IsoDateTime,
  prompt_version: z.string().nullable(),
});
export type Plan = z.infer<typeof Plan>;
