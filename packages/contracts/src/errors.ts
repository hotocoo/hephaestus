import { z } from "zod";

/**
 * Stable public error codes (hephaestus_core::Error::code). Renaming
 * any entry is a breaking change by definition; conformance tests
 * pin live responses against this list.
 */
export const ERROR_CODES = [
  "VALIDATION_FAILED",
  "NOT_FOUND",
  "CONFLICT",
  "FORBIDDEN",
  "UNAUTHENTICATED",
  "WORKFLOW_ILLEGAL_TRANSITION",
  "VERIFICATION_FAILED",
  "CONFIG_INVALID",
  "STORAGE_ERROR",
  "EXTERNAL_ERROR",
  "BUDGET_EXHAUSTED",
] as const;

/** Every failure response body shares this shape. */
export const ErrorBody = z.strictObject({
  code: z.enum(ERROR_CODES),
  message: z.string(),
  /** Offending input field; validation failures only. */
  field: z.string().optional(),
});
export type ErrorBody = z.infer<typeof ErrorBody>;
