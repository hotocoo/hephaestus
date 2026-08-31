/**
 * In-memory control-plane double for component tests.
 *
 * Implements the same ControlPlane surface the app consumes and
 * mimics just enough server behavior - gate decisions flip recorded
 * state, unknown gates render as 404s, receipts deduplicate on the
 * idempotency key - to exercise real view logic without network.
 *
 * @module
 */

import { ApiError } from "@hephaestus/sdk";
import type {
  ApprovalDecisionInput,
  ApprovalDecisionOutput,
  Artifact,
  ArtifactVerification,
  CreateTaskInput,
  Deployment,
  Event,
  Gate,
  IntakeReceipt,
  MergeDecisionInput,
  MergeDecisionOutput,
  Plan,
  Project,
  Repository,
  Run,
  Task,
} from "@hephaestus/sdk";
import type { ControlPlane } from "@/api/client";

export const ORG_ID = "01900000-0000-7000-8000-000000000001";
export const PROJECT_ID = "01900000-0000-7000-8000-000000000002";
export const REPO_ID = "01900000-0000-7000-8000-000000000003";
export const TASK_ID = "01900000-0000-7000-8000-000000000004";
export const RUN_ID = "01900000-0000-7000-8000-000000000005";

const NOW = "2026-01-15T10:30:00Z";

export function sampleTask(overrides: Partial<Task> = {}): Task {
  return {
    id: TASK_ID,
    organization_id: ORG_ID,
    project_id: PROJECT_ID,
    repository_id: REPO_ID,
    title: "Fix the flaky upload",
    description: "Uploads below the size limit fail intermittently.",
    priority: "high",
    risk: "medium",
    labels: ["bug"],
    created_at: NOW,
    updated_at: NOW,
    ...overrides,
  };
}

export function sampleRun(overrides: Partial<Run> = {}): Run {
  return {
    id: RUN_ID,
    task_id: TASK_ID,
    organization_id: ORG_ID,
    state: "awaiting_approval",
    attempt: 1,
    correlation_id: "01900000-0000-7000-8000-000000000006",
    lease_owner: null,
    lease_expires_at: null,
    last_transition_at: NOW,
    ...overrides,
  };
}

export function sampleDeployment(overrides: Partial<Deployment> = {}): Deployment {
  return {
    id: "01900000-0000-7000-8000-00000000000c",
    task_id: TASK_ID,
    run_id: RUN_ID,
    build_id: "01900000-0000-7000-8000-00000000000d",
    target: "staging",
    status: "succeeded",
    failure_reason: null,
    created_at: NOW,
    ...overrides,
  };
}

const SAMPLE_SHA = "9".repeat(64);

export function sampleArtifact(overrides: Partial<Artifact> = {}): Artifact {
  return {
    id: "01900000-0000-7000-8000-00000000000e",
    task_id: TASK_ID,
    run_id: RUN_ID,
    build_id: "01900000-0000-7000-8000-00000000000d",
    path: "target/debug/forge-cli",
    sha256: SAMPLE_SHA,
    size_bytes: 1024,
    created_at: NOW,
    ...overrides,
  };
}

function samplePlan(): Plan {
  return {
    id: "01900000-0000-7000-8000-000000000007",
    task_id: TASK_ID,
    objective: "Make uploads deterministic.",
    steps: [
      {
        id: "01900000-0000-7000-8000-000000000008",
        plan_id: "01900000-0000-7000-8000-000000000007",
        position: 1,
        action: "Adjust upload size guard",
        verification: "unit:upload",
        risks: ["touches request validation"],
      },
    ],
    affected_components: ["src/upload.rs"],
    affected_symbols: ["upload_size_guard"],
    strategy: {
      rollback: "revert the guard change",
      deployment: null,
      verification: ["unit:upload"],
    },
    created_at: NOW,
    prompt_version: "v1",
  };
}

/** Recorded decision inputs, keyed by run, for assertions. */
export interface DecisionLog {
  approvals: { runId: string; input: ApprovalDecisionInput }[];
  merges: { runId: string; input: MergeDecisionInput }[];
  submissions: { input: CreateTaskInput; key?: string }[];
}

export class FakeControlPlane implements ControlPlane {
  readonly decisions: DecisionLog = { approvals: [], merges: [], submissions: [] };

  projects: Project[] = [
    { id: PROJECT_ID, organization_id: ORG_ID, name: "Forge Core", slug: "forge-core" },
  ];
  repositories: Repository[] = [
    {
      id: REPO_ID,
      organization_id: ORG_ID,
      project_id: PROJECT_ID,
      remote_url: "https://example.invalid/forge.git",
      default_branch: "main",
      display_name: "Forge Repository",
    },
  ];
  tasks: Task[] = [];
  runs = new Map<string, Run>([[TASK_ID, sampleRun()]]);
  plans = new Map<string, Plan>([[TASK_ID, samplePlan()]]);
  approvalGates = new Map<string, Gate>([
    [
      RUN_ID,
      {
        approval_id: "01900000-0000-7000-8000-000000000009",
        gate: "plan",
        required_role: "approver",
        decision: null,
      },
    ],
  ]);
  mergeGates = new Map<string, Gate>();
  eventsByRun = new Map<string, Event[]>([
    [
      RUN_ID,
      [
        {
          id: "01900000-0000-7000-8000-00000000000a",
          aggregate: "workflow_run",
          aggregate_id: RUN_ID,
          provenance: "computed",
          payload: {
            type: "workflow_state_changed",
            data: { from: "planning", to: "awaiting_approval", trigger: "submit_for_approval", actor: "planner" },
          },
          occurred_at: NOW,
        },
      ],
    ],
  ]);
  deploymentsByRun = new Map<string, Deployment>([[RUN_ID, sampleDeployment()]]);
  artifactsByRun = new Map<string, Artifact[]>([
    [RUN_ID, [sampleArtifact()]],
  ]);

  async getHealthz(): Promise<{ status: string }> {
    return { status: "ok" };
  }

  async listProjects(): Promise<Project[]> {
    return this.projects;
  }

  async listRepositories(): Promise<Repository[]> {
    return this.repositories;
  }

  async listTasks(): Promise<Task[]> {
    return this.tasks;
  }


  async getTask(taskId: string): Promise<Task> {
    const task = this.tasks.find((t) => t.id === taskId);
    if (task === undefined) throw notFound();
    return task;
  }

  async getTaskPlan(taskId: string): Promise<Plan> {
    const plan = this.plans.get(taskId);
    if (plan === undefined) throw notFound();
    return plan;
  }

  async getTaskRun(taskId: string): Promise<Run> {
    const run = this.runs.get(taskId);
    if (run === undefined) throw notFound();
    return run;
  }

  async getRun(runId: string): Promise<Run> {
    for (const run of this.runs.values()) {
      if (run.id === runId) return run;
    }
    throw notFound();
  }

  async getRunDeployment(runId: string): Promise<Deployment> {
    const deployment = this.deploymentsByRun.get(runId);
    if (deployment === undefined) throw notFound();
    return deployment;
  }

  async listRunEvents(runId: string): Promise<Event[]> {
    return this.eventsByRun.get(runId) ?? [];
  }

  async listRunArtifacts(runId: string): Promise<Artifact[]> {
    return this.artifactsByRun.get(runId) ?? [];
  }

  async verifyArtifact(
    runId: string,
    artifactId: string,
  ): Promise<ArtifactVerification> {
    const artifacts = this.artifactsByRun.get(runId) ?? [];
    const artifact = artifacts.find((a) => a.id === artifactId);
    if (artifact === undefined) throw notFound();
    return {
      artifact_id: artifact.id,
      status: "verified",
      expected_sha256: artifact.sha256,
      actual_sha256: artifact.sha256,
    };
  }

  async downloadArtifact(runId: string, artifactId: string): Promise<Blob> {
    const artifacts = this.artifactsByRun.get(runId) ?? [];
    const artifact = artifacts.find((a) => a.id === artifactId);
    if (artifact === undefined) throw notFound();
    return new Blob(["fake-artifact-bytes"], { type: "application/octet-stream" });
  }


  async getApprovalGate(runId: string): Promise<Gate> {
    const gate = this.approvalGates.get(runId);
    if (gate === undefined) throw notFound();
    return gate;
  }

  async decideApproval(runId: string, input: ApprovalDecisionInput): Promise<ApprovalDecisionOutput> {
    const gate = this.approvalGates.get(runId);
    if (gate === undefined) throw notFound();
    this.decisions.approvals.push({ runId, input });
    if (input.approved) {
      gate.decision = "approved";
      const run = [...this.runs.values()].find((r) => r.id === runId);
      if (run !== undefined) run.state = "implementing";
      return {
        outcome: "approved",
        execution_id: "01900000-0000-7000-8000-00000000000b",
        enqueued_step: "01900000-0000-7000-8000-000000000008",
      };
    }
    gate.decision = "rejected";
    return { outcome: "rejected", approval_id: gate.approval_id };
  }

  async getMergeGate(runId: string): Promise<Gate> {
    const gate = this.mergeGates.get(runId);
    if (gate === undefined) throw notFound();
    return gate;
  }

  async decideMerge(runId: string, input: MergeDecisionInput): Promise<MergeDecisionOutput> {
    this.decisions.merges.push({ runId, input });
    return { outcome: "merged" };
  }

  async submitTask(input: CreateTaskInput, options?: { idempotencyKey?: string }): Promise<IntakeReceipt> {
    this.decisions.submissions.push({ input, key: options?.idempotencyKey });
    const existing = this.tasks.find(
      (t) =>
        t.title === input.title &&
        t.project_id === input.project_id &&
        t.repository_id === input.repository_id,
    );
    if (existing !== undefined) {
      const run = this.runs.get(existing.id);
      return {
        task_id: existing.id,
        run_id: run?.id ?? RUN_ID,
        deduplicated: true,
      };
    }
    const task = sampleTask({
      id: nextId(this.tasks.length + 1),
      title: input.title,
      description: input.description,
      project_id: input.project_id,
      repository_id: input.repository_id,
      priority: input.priority ?? "medium",
      risk: input.risk ?? "medium",
      labels: input.labels ?? [],
    });
    this.tasks.push(task);
    const runId = nextId(100 + this.tasks.length);
    this.runs.set(task.id, sampleRun({ id: runId, task_id: task.id, state: "created" }));
    return { task_id: task.id, run_id: runId, deduplicated: false };
  }
}

let counter = 0x10;

/** Distinct UUIDv7-shaped ids per fake entity. */
export function nextId(n: number): string {
  counter += n;
  return "01900000-0000-7000-8000-" + String(counter).padStart(12, "0");
}

function notFound(): ApiError {
  return new ApiError(404, "NOT_FOUND", "no such row");
}
