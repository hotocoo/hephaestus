import { describe, expect, it } from "vitest";
import { z } from "zod";
import {
  ApprovalOutcome,
  Deployment,
  ErrorBody,
  Event,
  IsoDateTime,
  MergeOutcome,
  PageQuery,
  Plan,
  Run,
  Task,
  Uuid,
} from "../src/index.ts";

const uuid = () => "018f6d2a-7c1f-7000-8000-000000000000";

describe("wire mirrors", () => {
  it("parses a task exactly as serde renders it", () => {
    const task = Task.parse({
      id: uuid(),
      organization_id: uuid(),
      project_id: uuid(),
      repository_id: uuid(),
      title: "Fix upload guard",
      description: "500s appear intermittently",
      priority: "high",
      risk: "medium",
      labels: ["upload"],
      created_at: "2026-08-25T09:30:00.123456789Z",
      updated_at: "2026-08-25T09:30:00Z",
    });
    expect(task.priority).toBe("high");
  });

  it("rejects unknown fields on strict response mirrors", () => {
    const parse = () =>
      Task.parse({
        id: uuid(),
        organization_id: uuid(),
        project_id: uuid(),
        repository_id: uuid(),
        title: "t",
        description: "d",
        priority: "high",
        risk: "low",
        labels: [],
        created_at: "2026-08-25T09:30:00Z",
        updated_at: "2026-08-25T09:30:00Z",
        surprise: true,
      });
    expect(parse).toThrow(z.ZodError);
  });

  it("accepts nanosecond timestamps but not naive dates", () => {
    expect(IsoDateTime.safeParse("2026-08-25T09:30:00.123456789Z").success).toBe(true);
    expect(IsoDateTime.safeParse("2026-08-25 09:30:00").success).toBe(false);
    expect(IsoDateTime.safeParse("2026-08-25T09:30:00+02:00").success).toBe(false);
  });

  it("validates error bodies against the stable code list", () => {
    expect(ErrorBody.safeParse({ code: "NOT_FOUND", message: "task not found" }).success).toBe(true);
    expect(
      ErrorBody.safeParse({ code: "VALIDATION_FAILED", message: "x", field: "title" })
        .success,
    ).toBe(true);
    expect(ErrorBody.safeParse({ code: "SOMETHING_ELSE" }).success).toBe(false);
  });

  it("discriminates gate outcomes on the outcome tag", () => {
    expect(
      ApprovalOutcome.parse({
        outcome: "approved",
        execution_id: uuid(),
        enqueued_step: null,
      }),
    ).toMatchObject({ outcome: "approved" });
    expect(
      ApprovalOutcome.safeParse({ outcome: "approved", execution_id: "nope", enqueued_step: null })
        .success,
    ).toBe(false);
    expect(MergeOutcome.parse({ outcome: "already_merged" })).toMatchObject({
      outcome: "already_merged",
    });
  });

  it("mirrors runs and events including nullable leases", () => {
    const run = Run.parse({
      id: uuid(),
      task_id: uuid(),
      organization_id: uuid(),
      state: "created",
      attempt: 1,
      correlation_id: uuid(),
      lease_owner: null,
      lease_expires_at: null,
      last_transition_at: "2026-08-25T09:30:00Z",
    });
    expect(run.state).toBe("created");

    const event = Event.parse({
      id: uuid(),
      aggregate: "workflow_run",
      aggregate_id: uuid(),
      provenance: "internal",
      payload: { kind: "anything" },
      occurred_at: "2026-08-25T09:30:00Z",
    });
    expect(event.aggregate).toBe("workflow_run");
  });

  it("mirrors deployments with their status vocabulary", () => {
    const deployment = Deployment.parse({
      id: uuid(),
      task_id: uuid(),
      run_id: uuid(),
      build_id: uuid(),
      target: "staging",
      status: "failed",
      failure_reason: "heph-deploy failed (exit_code=Some(3)): boom",
      created_at: "2026-08-25T09:30:00Z",
    });
    expect(deployment.status).toBe("failed");

    // Only the three documented statuses parse.
    expect(Deployment.safeParse({ ...deployment, status: "paused" }).success).toBe(false);
    // Unknown fields fail, like every strict mirror.
    expect(
      Deployment.safeParse({ ...deployment, verified_at: "2026-08-25T10:00:00Z" }).success,
    ).toBe(false);
  });

  it("keeps plan invariants: at least one contiguous step", () => {
    const base = {
      id: uuid(),
      task_id: uuid(),
      objective: "Fix it",
      affected_components: [],
      affected_symbols: [],
      strategy: { rollback: null, deployment: null, verification: ["unit"] },
      created_at: "2026-08-25T09:30:00Z",
      prompt_version: null,
    };
    const step = (position: number) => ({
      id: uuid(),
      plan_id: base.id,
      position,
      action: "Add guard in src/upload.rs",
      verification: "unit:upload_size_guard",
      risks: [],
    });
    expect(Plan.safeParse({ ...base, steps: [] }).success).toBe(false);
    expect(Plan.parse({ ...base, steps: [step(1)] }).steps).toHaveLength(1);
  });

  it("coerces pagination queries from URL strings", () => {
    expect(PageQuery.parse({ limit: "10", offset: "20" })).toEqual({ limit: 10, offset: 20 });
    expect(PageQuery.safeParse({ limit: "-3" }).success).toBe(false);
  });

  it("treats identifiers as UUID strings only", () => {
    expect(Uuid.safeParse(uuid()).success).toBe(true);
    expect(Uuid.safeParse("not-a-uuid").success).toBe(false);
  });
});
