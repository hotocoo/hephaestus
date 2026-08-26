import { z } from "zod";

/**
 * Query parameters shared by list endpoints. Values arrive as URL
 * strings, so coercion is part of the contract.
 */
export const PageQuery = z.object({
  limit: z.coerce.number().int().positive().max(10_000).optional(),
  offset: z.coerce.number().int().nonnegative().optional(),
});
export type PageQuery = z.infer<typeof PageQuery>;
