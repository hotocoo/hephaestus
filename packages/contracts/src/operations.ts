/**
 * The operation inventory this package mirrors, pinned one-to-one
 * against \`hephaestus_api::openapi::OPERATIONS\`. Conformance tests
 * fetch the served OpenAPI document and require exact equality with
 * this list - method, path and operationId - so the Rust contract and
 * the TypeScript mirror cannot drift apart silently.
 */
export interface OperationDescriptor {
  method: "GET" | "POST";
  path: string;
  operationId: string;
}

export const OPERATIONS: readonly OperationDescriptor[] = [
  { method: "GET", path: "/healthz", operationId: "getHealthz" },
  { method: "GET", path: "/readyz", operationId: "getReadyz" },
  { method: "GET", path: "/api/v1/openapi.json", operationId: "getOpenapiDocument" },
  { method: "POST", path: "/api/v1/tasks", operationId: "submitTask" },
  { method: "GET", path: "/api/v1/tasks", operationId: "listTasks" },
  { method: "GET", path: "/api/v1/tasks/{task_id}", operationId: "getTask" },
  { method: "GET", path: "/api/v1/tasks/{task_id}/plan", operationId: "getTaskPlan" },
  { method: "GET", path: "/api/v1/tasks/{task_id}/run", operationId: "getTaskRun" },
  { method: "GET", path: "/api/v1/projects", operationId: "listProjects" },
  { method: "GET", path: "/api/v1/repositories", operationId: "listRepositories" },
  { method: "GET", path: "/api/v1/runs/{run_id}", operationId: "getRun" },
  { method: "GET", path: "/api/v1/runs/{run_id}/deployment", operationId: "getRunDeployment" },
  { method: "GET", path: "/api/v1/runs/{run_id}/events", operationId: "listRunEvents" },
  { method: "GET", path: "/api/v1/runs/{run_id}/approval", operationId: "getApprovalGate" },
  { method: "POST", path: "/api/v1/runs/{run_id}/approval", operationId: "decideApproval" },
  { method: "GET", path: "/api/v1/runs/{run_id}/merge-gate", operationId: "getMergeGate" },
  { method: "POST", path: "/api/v1/runs/{run_id}/merge", operationId: "decideMerge" },
];
