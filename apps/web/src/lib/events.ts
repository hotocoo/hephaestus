/**
 * Event interpretation for views.
 *
 * Events are append-only history with provenance classifications.
 * Views may use well-known payload shapes to enrich rendering but must
 * degrade to raw-text rendering when shapes do not match; a mismatched
 * payload is a display problem, never a crash and never an instruction.
 *
 * @module
 */

export interface WorkflowTransitionView {
  from: string | null;
  to: string | null;
  trigger: string | null;
  actor: string | null;
}

type PayloadRecord = Record<string, unknown>;

function isRecord(value: unknown): value is PayloadRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asStringOrNull(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

/**
 * Recognize workflow_state_changed payloads. Anything else returns
 * null and the view falls back to generic rendering.
 */
export function workflowTransition(payload: unknown): WorkflowTransitionView | null {
  if (!isRecord(payload)) return null;
  if (payload["type"] !== "workflow_state_changed") return null;
  const data = payload["data"];
  if (!isRecord(data)) return null;
  return {
    from: asStringOrNull(data["from"]),
    to: asStringOrNull(data["to"]),
    trigger: asStringOrNull(data["trigger"]),
    actor: asStringOrNull(data["actor"]),
  };
}
