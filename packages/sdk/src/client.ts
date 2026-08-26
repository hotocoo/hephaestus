/**
 * Typed thin client for the Hephaestus control-plane API.
 *
 * Every request/response type derives from src/openapi.d.ts, which
 * is generated from the OpenAPI document the server itself serves -
 * never hand-maintained (ADR-002). The client holds no authority:
 * it forwards credentials it was constructed with, adds no policy,
 * and renders every failure through one typed error.
 */
import type { components } from "./openapi";

/** Schema-derived wire types. */
export type Schemas = components["schemas"];
export type Project = Schemas["ProjectResponse"];
export type Repository = Schemas["RepositoryResponse"];
export type Task = Schemas["TaskResponse"];
export type CreateTaskInput = Schemas["CreateTaskRequest"];
export type IntakeReceipt = Schemas["IntakeResponse"];
export type Run = Schemas["RunResponse"];
export type Deployment = Schemas["DeploymentResponse"];
export type Event = Schemas["EventResponse"];
export type Plan = Schemas["Plan"];
export type Gate = Schemas["GateResponse"];
export type ApprovalDecisionInput = Schemas["ApprovalDecisionRequest"];
export type ApprovalDecisionOutput = Schemas["ApprovalDecisionResponse"];
export type MergeDecisionInput = Schemas["MergeDecisionRequest"];
export type MergeDecisionOutput = Schemas["MergeDecisionResponse"];

/** Pagination query shared by list methods. */
export interface PageQuery {
  limit?: number;
  offset?: number;
}

export interface RepositoryQuery extends PageQuery {
  project_id?: string;
}

/** The one error type requests reject with. */
export class ApiError extends Error {
  readonly status: number;
  /** Stable public code from the shared error shape. */
  readonly code: string | undefined;
  /** Offending field, validation failures only. */
  readonly field: string | undefined;

  constructor(
    status: number,
    code: string | undefined,
    message: string,
    field?: string,
  ) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.field = field;
  }

  /** Build from a raw failure body, tolerating non-JSON payloads */
  static from(status: number, raw: string): ApiError {
    let code: string | undefined;
    let message: string | undefined;
    let field: string | undefined;
    try {
      const parsed = JSON.parse(raw) as {
        code?: unknown;
        message?: unknown;
        field?: unknown;
      };
      if (typeof parsed.code === "string") code = parsed.code;
      if (typeof parsed.message === "string") message = parsed.message;
      if (typeof parsed.field === "string") field = parsed.field;
    } catch {
      // Plain-text failures (e.g. body-size limit) land here.
    }
    return new ApiError(
      status,
      code,
      message ?? "request failed with HTTP " + String(status),
      field,
    );
  }
}

export interface HephaestusClientOptions {
  /** Base URL without trailing slash, e.g. http://127.0.0.1:7300 */
  baseUrl: string;
  /** Pre-provisioned bearer token bound to one organization. */
  token: string;
  /** Transport override for tests; global fetch by default. */
  fetchImpl?: typeof fetch;
}

function buildQuery(query: object): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined) params.set(key, String(value));
  }
  const encoded = params.toString();
  return encoded.length === 0 ? "" : "?" + encoded;
}

const idSegment = (id: string): string => encodeURIComponent(id);

/** Typed client over the served contract document. */
export class HephaestusClient {
  private readonly baseUrl: string;
  private readonly token: string;
  private readonly transport: typeof fetch;

  constructor(options: HephaestusClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.token = options.token;
    this.transport = options.fetchImpl ?? fetch;
  }

  private async request<T>(
    method: "GET" | "POST",
    path: string,
    init: {
      body?: unknown;
      headers?: Record<string, string>;
      /** Public routes opt out of the bearer header entirely. */
      auth?: boolean;
    } = {},
  ): Promise<T> {
    const headers: Record<string, string> = { ...init.headers };
    if (init.auth !== false) {
      headers.authorization = "Bearer " + this.token;
    }
    if (init.body !== undefined) {
      headers["content-type"] = "application/json";
    }
    const response = await this.transport(this.baseUrl + path, {
      method,
      headers,
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
    });
    const raw = await response.text();
    if (!response.ok) {
      throw ApiError.from(response.status, raw);
    }
    return (raw.length === 0 ? undefined : JSON.parse(raw)) as T;
  }

  /** Fetch the OpenAPI document itself (public, unauthenticated). */
  async getOpenApiDocument(): Promise<Record<string, unknown>> {
    return this.request("GET", "/api/v1/openapi.json", { auth: false });
  }

  async getHealthz(): Promise<Schemas["StatusBody"]> {
    return this.request("GET", "/healthz", { auth: false });
  }

  async getReadyz(): Promise<Schemas["StatusBody"]> {
    return this.request("GET", "/readyz", { auth: false });
  }

  async listProjects(query: PageQuery = {}): Promise<Project[]> {
    return this.request("GET", "/api/v1/projects" + buildQuery(query));
  }

  async listRepositories(query: RepositoryQuery = {}): Promise<Repository[]> {
    return this.request("GET", "/api/v1/repositories" + buildQuery(query));
  }

  async submitTask(
    input: CreateTaskInput,
    options: { idempotencyKey?: string } = {},
  ): Promise<IntakeReceipt> {
    const headers: Record<string, string> = {};
    if (options.idempotencyKey !== undefined) {
      headers["idempotency-key"] = options.idempotencyKey;
    }
    return this.request("POST", "/api/v1/tasks", { body: input, headers });
  }

  async listTasks(query: PageQuery = {}): Promise<Task[]> {
    return this.request("GET", "/api/v1/tasks" + buildQuery(query));
  }

  async getTask(taskId: string): Promise<Task> {
    return this.request("GET", "/api/v1/tasks/" + idSegment(taskId));
  }

  async getTaskPlan(taskId: string): Promise<Plan> {
    return this.request("GET", "/api/v1/tasks/" + idSegment(taskId) + "/plan");
  }

  /** Current workflow run of a task; the path from task to live state. */
  async getTaskRun(taskId: string): Promise<Run> {
    return this.request("GET", "/api/v1/tasks/" + idSegment(taskId) + "/run");
  }

  async getRun(runId: string): Promise<Run> {
    return this.request("GET", "/api/v1/runs/" + idSegment(runId));
  }

  /** The run's latest deployment; rejects with ApiError(404) when none. */
  async getRunDeployment(runId: string): Promise<Deployment> {
    return this.request("GET", "/api/v1/runs/" + idSegment(runId) + "/deployment");
  }

  async listRunEvents(runId: string, query: PageQuery = {}): Promise<Event[]> {
    return this.request("GET", "/api/v1/runs/" + idSegment(runId) + "/events" + buildQuery(query));
  }

  async getApprovalGate(runId: string): Promise<Gate> {
    return this.request("GET", "/api/v1/runs/" + idSegment(runId) + "/approval");
  }

  async decideApproval(
    runId: string,
    input: ApprovalDecisionInput,
  ): Promise<ApprovalDecisionOutput> {
    return this.request("POST", "/api/v1/runs/" + idSegment(runId) + "/approval", {
      body: input,
    });
  }

  async getMergeGate(runId: string): Promise<Gate> {
    return this.request("GET", "/api/v1/runs/" + idSegment(runId) + "/merge-gate");
  }

  async decideMerge(
    runId: string,
    input: MergeDecisionInput,
  ): Promise<MergeDecisionOutput> {
    return this.request("POST", "/api/v1/runs/" + idSegment(runId) + "/merge", {
      body: input,
    });
  }
}
