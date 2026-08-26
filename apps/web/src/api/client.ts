/**
 * Access to the control plane for Vue components.
 *
 * The typed client comes from @hephaestus/sdk, whose declarations are
 * generated from the OpenAPI document the server itself serves
 * (ADR-011). The dashboard consumes it through an injection key so
 * tests can mount real components against an in-memory double without
 * patching modules; production wiring provides the real client exactly
 * once, in main.ts.
 *
 * @module
 */

import { inject, provide } from "vue";
import type { InjectionKey } from "vue";
import { ApiError } from "@hephaestus/sdk";
import type {
  ApprovalDecisionInput,
  ApprovalDecisionOutput,
  CreateTaskInput,
  Event,
  Gate,
  IntakeReceipt,
  MergeDecisionInput,
  MergeDecisionOutput,
  PageQuery,
  Plan,
  Project,
  Repository,
  RepositoryQuery,
  Run,
  Schemas,
  Task,
} from "@hephaestus/sdk";

/**
 * Everything the dashboard may ask the control plane to do.
 *
 * Method shapes mirror the generated client verbatim (query objects
 * included) so the real HephaestusClient satisfies this interface
 * structurally and tests can substitute a double.
 */
export interface ControlPlane {
  getHealthz(): Promise<Schemas["StatusBody"]>;
  listProjects(query?: PageQuery): Promise<Project[]>;
  listRepositories(query?: RepositoryQuery): Promise<Repository[]>;
  listTasks(query?: PageQuery): Promise<Task[]>;
  getTask(taskId: string): Promise<Task>;
  getTaskPlan(taskId: string): Promise<Plan>;
  getTaskRun(taskId: string): Promise<Run>;
  getRun(runId: string): Promise<Run>;
  listRunEvents(runId: string, query?: PageQuery): Promise<Event[]>;
  getApprovalGate(runId: string): Promise<Gate>;
  decideApproval(runId: string, input: ApprovalDecisionInput): Promise<ApprovalDecisionOutput>;
  getMergeGate(runId: string): Promise<Gate>;
  decideMerge(runId: string, input: MergeDecisionInput): Promise<MergeDecisionOutput>;
  submitTask(input: CreateTaskInput, options?: { idempotencyKey?: string }): Promise<IntakeReceipt>;
}

/** Status probe shape, public and unauthenticated. */
export type StatusBody = Schemas["StatusBody"];

/**
 * Loader helper: a 404 becomes "absent" instead of a failure, so
 * views can distinguish "nothing there yet" from "the request broke".
 * Any other error rethrows untouched.
 */
export function notFoundToNull<T>(thrown: unknown): T | null {
  if (thrown instanceof ApiError && thrown.status === 404) return null;
  throw thrown;
}

const CONTROL_PLANE_KEY: InjectionKey<ControlPlane> = Symbol("control-plane");

/** Install the client for the whole component tree. */
export function provideControlPlane(client: ControlPlane): void {
  provide(CONTROL_PLANE_KEY, client);
}

/** The installed client; missing wiring is a programming error. */
export function useControlPlane(): ControlPlane {
  const client = inject(CONTROL_PLANE_KEY);
  if (client === undefined) {
    throw new Error(
      "no control plane provided - mount the app through createAppWithControlPlane or provide one in tests",
    );
  }
  return client;
}
