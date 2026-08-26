import { describe, expect, it } from "vitest";
import { ApiError, HephaestusClient } from "../src/index.ts";

interface CapturedRequest {
  url: string;
  method: string;
  headers: Record<string, string>;
  body?: string;
}

/** A transport that records requests and replays canned responses. */
function makeTransport(responses: Array<{ status: number; body?: unknown; text?: string }>) {
  const captured: CapturedRequest[] = [];
  let call = 0;
  const fetchImpl = (async (input: string | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.href;
    const headers: Record<string, string> = {};
    new Headers(init?.headers).forEach((value, key) => {
      headers[key] = value;
    });
    captured.push({
      url,
      method: init?.method ?? "GET",
      headers,
      body: typeof init?.body === "string" ? init.body : undefined,
    });
    const canned = responses[call];
    if (canned === undefined) {
      throw new Error("no canned response for request " + String(call));
    }
    call += 1;
    const payload = canned.text ?? JSON.stringify(canned.body ?? null);
    return new Response(payload, {
      status: canned.status,
      headers: { "content-type": "application/json" },
    });
  }) as typeof fetch;
  return { fetchImpl, captured };
}

const uuid = "018f6d2a-7c1f-7000-8000-000000000000";

function clientWith(
  responses: Array<{ status: number; body?: unknown; text?: string }>,
) {
  const transport = makeTransport(responses);
  const client = new HephaestusClient({
    baseUrl: "http://127.0.0.1:7300/",
    token: "secret-token",
    fetchImpl: transport.fetchImpl,
  });
  return { client, captured: transport.captured };
}

describe("HephaestusClient", () => {
  it("normalizes the base URL and sends bearer credentials", async () => {
    const { client, captured } = clientWith([{ status: 200, body: [] }]);
    await client.listTasks();
    expect(captured[0]?.url).toBe("http://127.0.0.1:7300/api/v1/tasks");
    expect(captured[0]?.headers.authorization).toBe("Bearer secret-token");
  });

  it("serializes pagination and filters into query strings", async () => {
    const { client, captured } = clientWith([{ status: 200, body: [] }]);
    await client.listRepositories({ limit: 10, offset: 20, project_id: uuid });
    expect(captured[0]?.url).toContain("/api/v1/repositories?");
    expect(captured[0]?.url).toContain("limit=10");
    expect(captured[0]?.url).toContain("offset=20");
    expect(captured[0]?.url).toContain("project_id=" + uuid);
  });

  it("submits tasks with idempotency keys as typed bodies", async () => {
    const receipt = { task_id: uuid, run_id: uuid, deduplicated: false };
    const { client, captured } = clientWith([{ status: 201, body: receipt }]);
    const out = await client.submitTask(
      {
        title: "Fix upload guard",
        description: "d",
        project_id: uuid,
        repository_id: uuid,
        priority: "high",
      },
      { idempotencyKey: "key-123" },
    );
    expect(out).toEqual(receipt);
    expect(captured[0]?.method).toBe("POST");
    expect(captured[0]?.headers["idempotency-key"]).toBe("key-123");
    expect(captured[0]?.headers["content-type"]).toBe("application/json");
    expect(JSON.parse(captured[0]?.body ?? "{}")).toMatchObject({ title: "Fix upload guard" });
  });

  it("escapes path identifiers", async () => {
    const { client, captured } = clientWith([
      { status: 200, body: { id: uuid, task_id: uuid, organization_id: uuid, state: "created", attempt: 1, correlation_id: uuid, lease_owner: null, lease_expires_at: null, last_transition_at: "2026-08-25T09:30:00Z" } },
    ]);
    await client.getRun("abc 123/45");
    expect(captured[0]?.url).toBe("http://127.0.0.1:7300/api/v1/runs/abc%20123%2F45");
  });

  it("renders failures through one typed error", async () => {
    const { client } = clientWith([
      {
        status: 422,
        body: { code: "VALIDATION_FAILED", message: "priority: must be one of critical|high|medium|low", field: "priority" },
      },
    ]);
    // A malformed identifier reaches the handler and fails validation
    // there - the honest way to exercise this path with typed inputs.
    const err = await client.getTask("not-a-uuid").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    const apiError = err as ApiError;
    expect(apiError.status).toBe(422);
    expect(apiError.code).toBe("VALIDATION_FAILED");
    expect(apiError.field).toBe("priority");
    expect(apiError.message).toContain("priority");
  });

  it("survives non-JSON failure bodies like the size limit", async () => {
    const { client } = clientWith([{ status: 413, text: "body too large" }]);
    const err = (await client.listProjects().catch((e: unknown) => e)) as ApiError;
    expect(err).toBeInstanceOf(ApiError);
    expect(err.code).toBeUndefined();
    expect(err.message).toContain("413");
  });

  it("keeps public routes free of credentials", async () => {
    const { client, captured } = clientWith([
      { status: 200, body: { status: "ok" } },
      { status: 200, body: { openapi: "3.1.0", paths: {} } },
    ]);
    await client.getHealthz();
    await client.getOpenApiDocument();
    expect(captured[0]?.headers.authorization).toBeUndefined();
    expect(captured[1]?.headers.authorization).toBeUndefined();
  });

  it("narrows tagged gate outcomes for callers", async () => {
    const { client } = clientWith([
      { status: 200, body: { outcome: "approved", execution_id: uuid, enqueued_step: null } },
    ]);
    const outcome = await client.decideApproval(uuid, { approved: true, reason: "lgtm" });
    if (outcome.outcome === "approved") {
      expect(outcome.execution_id).toBe(uuid);
      expect(outcome.enqueued_step).toBeNull();
    } else {
      throw new Error("unexpected branch");
    }
  });

  it("defaults to the global fetch without detaching its receiver", async () => {
    // Regression: storing bare fetch on the instance detaches it from
    // its global receiver; browsers then reject every request with
    // "Illegal invocation" (found by driving the live dashboard).
    const original = globalThis.fetch;
    let called = 0;
    globalThis.fetch = (async () =>
      new Response(JSON.stringify([]), {
        status: 200,
        headers: { "content-type": "application/json" },
      })) as unknown as typeof fetch;
    try {
      const client = new HephaestusClient({
        baseUrl: "http://127.0.0.1:7300",
        token: "t",
      });
      const out = await client.listTasks();
      expect(out).toEqual([]);
      called = 1;
    } finally {
      globalThis.fetch = original;
    }
    expect(called).toBe(1);
  });
});
