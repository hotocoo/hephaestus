import { z } from "zod";

/** Body of the unauthenticated liveness/readiness probes. */
export const Status = z.strictObject({
  status: z.string(),
});
export type Status = z.infer<typeof Status>;
