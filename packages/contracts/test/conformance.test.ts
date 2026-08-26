/**
 * Live conformance: every assertion below runs against a real
 * hephaestus-server process over real HTTP and PostgreSQL. The zod
 * mirrors are only trustworthy if the server's actual bytes parse
 * against them - that is what this suite proves (ADR-002, ADR-011).
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { randomUUID } from "node:crypto";
import { z } from "zod";
import { startLiveServer, type LiveServer } from "../src/testing/server.ts";
import {
  ErrorBody,
  Event,
  IntakeReceipt,
  OPERATIONS,
  Project,
  Repository,
  Run,
  Status,
  Task,
} from "../src/index.ts";

let server: LiveServer;

interface Call {
  status: number;
  body: unknown;
}

async function call(
  method: string,
  path: string,
  options: { token?: string; body?: unknown; headers?: Record<string, string> } = {},
): Promise<Call> {
  const headers: Record<string, string> = { ...options.headers };
  if (options.token !== undefined) {
    headers.authorization = "Bearer " + options.token;
  }
  if (options.body !== undefined) {
    headers["content-type"] = "application/json";
  }
  const response = await fetch(server.baseUrl + path, {
    method,
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
  const text = await response.text();
  return {
    status: response.status,
    body: text.length === 0 ? null : JSON.parse(text),
  };
}

const authed = (path: string) => call("GET", path, { token: server.token });

beforeAll(async () => {
  server = await startLiveServer();
});

afterAll(async () => {
  await server.stop();
});

describe("live conformance against hephaestus-server", () => {
  it("serves probes without auth in the documented shape", async () => {
    const live = await call("GET", "/healthz");
    expect(live.status).toBe(200);
    expect(Status.parse(live.body)).toMatchObject({ status: "ok" });

    const ready = await call("GET", "/readyz");
    expect(ready.status).toBe(200);
    expect(Status.parse(ready.body)).toMatchObject({ status: "ready" });
  });

  it("serves a document whose operations equal the mirrored inventory", async () => {
    const doc = await call("GET", "/api/v1/openapi.json");
    expect(doc.status).toBe(200);
    const document = doc.body as { openapi: string; paths: Record<string, Record<string, { operationId: string }>> };
    expect(document.openapi).toBe("3.1.0");

    const served: string[] = [];
    for (const [path, item] of Object.entries(document.paths)) {
      for (const [method, op] of Object.entries(item)) {
        if (!["get", "post", "put", "patch", "delete"].includes(method)) continue;
        served.push(method.toUpperCase() + " " + path + " " + op.operationId);
      }
    }
    served.sort((a, b) => a.localeCompare(b));
    const mirrored = OPERATIONS.map(
      (o) => o.method + " " + o.path + " " + o.operationId,
    ).sort((a, b) => a.localeCompare(b));
    expect(served).toEqual(mirrored);
  });

  it("rejects unauthenticated calls with the shared error shape", async () => {
    const none = await call("GET", "/api/v1/tasks");
    expect(none.status).toBe(401);
    expect(ErrorBody.parse(none.body)).toMatchObject({ code: "UNAUTHENTICATED" });

    const wrong = await call("GET", "/api/v1/tasks", { token: "not-a-real-token-000" });
    expect(wrong.status).toBe(401);
    expect(ErrorBody.parse(wrong.body)).toMatchObject({ code: "UNAUTHENTICATED" });
  });

  it("lists seeded catalog rows that parse against the mirrors", async () => {
    const projects = await authed("/api/v1/projects");
    expect(projects.status).toBe(200);
    const parsedProjects = z.array(Project).parse(projects.body);
    expect(parsedProjects.length).toBeGreaterThanOrEqual(1);

    const repositories = await authed("/api/v1/repositories?project_id=" + server.tenant.projectId);
    expect(repositories.status).toBe(200);
    const parsedRepos = z.array(Repository).parse(repositories.body);
    expect(parsedRepos.length).toBeGreaterThanOrEqual(1);
    for (const repo of parsedRepos) {
      expect(repo.project_id).toBe(server.tenant.projectId);
    }
  });

  it("accepts intake idempotently and parses receipts", async () => {
    const input = {
      title: "Conformance upload guard",
      description: "Verify the wire contract end to end.",
      project_id: server.tenant.projectId,
      repository_id: server.tenant.repositoryId,
      priority: "high",
      risk: "low",
      labels: ["conformance"],
    };
    const key = "conf-" + randomUUID();

    const first = await call("POST", "/api/v1/tasks", {
      token: server.token,
      body: input,
      headers: { "idempotency-key": key },
    });
    expect(first.status).toBe(201);
    const receipt = IntakeReceipt.parse(first.body);
    expect(receipt.deduplicated).toBe(false);

    const replay = await call("POST", "/api/v1/tasks", {
      token: server.token,
      body: input,
      headers: { "idempotency-key": key },
    });
    expect(replay.status).toBe(201);
    const replayReceipt = IntakeReceipt.parse(replay.body);
    expect(replayReceipt.deduplicated).toBe(true);
    expect(replayReceipt.task_id).toBe(receipt.task_id);
    expect(replayReceipt.run_id).toBe(receipt.run_id);

    // Remember one run for later scenarios.
    context.runId = receipt.run_id;
    context.taskId = receipt.task_id;
  });

  it("names the offending field on validation failures", async () => {
    const bad = await call("POST", "/api/v1/tasks", {
      token: server.token,
      body: {
        title: "",
        description: "d",
        project_id: server.tenant.projectId,
        repository_id: server.tenant.repositoryId,
        priority: "urgent",
      },
    });
    expect(bad.status).toBe(422);
    const err = ErrorBody.parse(bad.body);
    expect(err.code).toBe("VALIDATION_FAILED");
    expect(err.field === "priority" || err.field === "title").toBe(true);
  });

  it("reads back task, run and events in the documented shapes", async () => {
    const task = await authed("/api/v1/tasks/" + context.taskId);
    expect(task.status).toBe(200);
    expect(Task.parse(task.body)).toMatchObject({ title: "Conformance upload guard" });

    const missing = await authed("/api/v1/tasks/" + randomUUID());
    expect(missing.status).toBe(404);
    expect(ErrorBody.parse(missing.body)).toMatchObject({ code: "NOT_FOUND" });

    // No planning has run yet, so no plan exists.
    const plan = await authed("/api/v1/tasks/" + context.taskId + "/plan");
    expect(plan.status).toBe(404);
    expect(ErrorBody.parse(plan.body)).toMatchObject({ code: "NOT_FOUND" });

    const run = await authed("/api/v1/runs/" + context.runId);
    expect(run.status).toBe(200);
    expect(Run.parse(run.body)).toMatchObject({ state: "created" });

    const events = await authed("/api/v1/runs/" + context.runId + "/events");
    expect(events.status).toBe(200);
    expect(Array.isArray(events.body)).toBe(true);
    z.array(Event).parse(events.body);
  });

  it("reports absent gates honestly and refuses premature merges", async () => {
    const gate = await authed("/api/v1/runs/" + context.runId + "/approval");
    expect(gate.status).toBe(404);
    expect(ErrorBody.parse(gate.body)).toMatchObject({ code: "NOT_FOUND" });

    const mergeGate = await authed("/api/v1/runs/" + context.runId + "/merge-gate");
    expect(mergeGate.status).toBe(404);

    const merge = await call("POST", "/api/v1/runs/" + context.runId + "/merge", {
      token: server.token,
      body: { reason: "too early", external_ref: null },
    });
    expect(merge.status).toBe(409);
    expect(ErrorBody.parse(merge.body)).toMatchObject({ code: "CONFLICT" });
  });
});

// Scenario order matters (intake feeds reads feed gates), so the
// suite shares one server and passes identifiers through this object.
const context: { runId?: string; taskId?: string } = {};
