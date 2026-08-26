/**
 * Workflow state vocabulary shared by every view.
 *
 * The state names and lifecycle order mirror hephaestus-core's
 * WorkflowState (crates/core/src/state.rs); the server is the sole
 * authority on what state a run is actually in, this module only
 * renders names onto the fixed lifecycle spine.
 *
 * @module
 */

/** States in lifecycle order, terminal ones last. */
export const WORKFLOW_STATES = [
  "created",
  "analyzing",
  "planning",
  "awaiting_approval",
  "implementing",
  "verifying",
  "reviewing",
  "awaiting_merge",
  "building",
  "deploying",
  "verifying_deployment",
  "completed",
  "failed",
  "cancelled",
] as const;

export type WorkflowState = (typeof WORKFLOW_STATES)[number];

const TERMINAL_OK = new Set<string>(["completed"]);
const TERMINAL_BAD = new Set<string>(["failed", "cancelled"]);

/** Human label for a state name; unknown states render verbatim. */
export function stateLabel(state: string): string {
  return state.replaceAll("_", " ");
}

export type StateTone = "idle" | "busy" | "ok" | "bad";

/** Visual tone of a state: parked, moving, done or dead. */
export function stateTone(state: string): StateTone {
  if (TERMINAL_OK.has(state)) return "ok";
  if (TERMINAL_BAD.has(state)) return "bad";
  if (state === "created" || state === "awaiting_approval" || state === "awaiting_merge") {
    return "idle";
  }
  return "busy";
}

/** Whether a run in this state can still move. */
export function isTerminalState(state: string): boolean {
  return TERMINAL_OK.has(state) || TERMINAL_BAD.has(state);
}

function indexOfState(state: string): number {
  const index = WORKFLOW_STATES.indexOf(state as WorkflowState);
  return index === -1 ? WORKFLOW_STATES.length : index;
}

/**
 * Progress of a run along the happy-path spine.
 *
 * Terminal failure/cancellation keeps the distance it had reached so
 * a dead run still shows how far it got; unknown states (a newer
 * server talking to an older UI) render as 100% with the raw label,
 * which is honest rather than invented.
 */
export function stateProgress(state: string): {
  reachedIndex: number;
  percent: number;
  known: boolean;
} {
  const known = WORKFLOW_STATES.includes(state as WorkflowState);
  const reachedIndex = known ? indexOfState(state) : WORKFLOW_STATES.length - 1;
  const percent = Math.round((reachedIndex / (WORKFLOW_STATES.length - 1)) * 100);
  return { reachedIndex, percent, known };
}

/** Priority display order: most urgent first. */
export const PRIORITY_ORDER = ["critical", "high", "medium", "low"] as const;

export type Priority = (typeof PRIORITY_ORDER)[number];

/** Sort key honoring the priority order; unknown names sort last. */
export function priorityRank(priority: string): number {
  const index = PRIORITY_ORDER.indexOf(priority as Priority);
  return index === -1 ? PRIORITY_ORDER.length : index;
}

export type ClassificationTone = "idle" | "warn" | "bad" | "info";

/** Pill tone for priorities: urgent reads hot, low reads quiet. */
export function priorityTone(priority: string): ClassificationTone {
  if (priority === "critical" || priority === "high") return "bad";
  if (priority === "medium") return "info";
  return "idle";
}

/** Pill tone for risk levels: high risk always reads hot. */
export function riskTone(risk: string): ClassificationTone {
  if (risk === "critical") return "bad";
  if (risk === "high") return "warn";
  return "info";
}
