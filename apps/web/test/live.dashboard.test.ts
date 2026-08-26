/**
 * Dashboard data layer against the real control plane.
 *
 * Spawns the actual hephaestus-server binary on real PostgreSQL
 * (the same harness the contracts conformance suite uses) and drives
 * the flows the dashboard depends on: idempotent intake, the
 * task->run lookup that powers every live view, and one mounted
 * shell rendering real HTTP responses inside jsdom.
 *
 * Prerequisites:
 * - HEPHAESTUS_TEST_DATABASE_URL set to a disposable PostgreSQL database.
 * - A built server binary: cargo build -p hephaestus-api --bin hephaestus-server.
 */

import { describe, expect, it } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createMemoryHistory, createRouter } from "vue-router";
import { HephaestusClient } from "@hephaestus/sdk";
import { startLiveServer } from "@hephaestus/contracts/testing";
import App from "../src/App.vue";
import TaskDetailView from "../src/views/TaskDetailView.vue";

/**
 * Real network round-trips do not settle within one microtask flush;
 * poll the rendered text for a bounded while instead.
 */
async function untilText(
  wrapper: { text(): string },
  needle: string,
  timeoutMs = 10_000,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    await flushPromises();
    if (wrapper.text().includes(needle)) return true;
    if (Date.now() > deadline) return false;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

describe("dashboard over the live control plane", () => {
  it("intakes work, resolves the run by task, and renders the detail view", async () => {
    const server = await startLiveServer();
    try {
      const client = new HephaestusClient({
        baseUrl: server.baseUrl,
        token: server.token,
      });

      // Intake is idempotent over the wire.
      const input = {
        title: "Dashboard live intake",
        description: "Submitted by the dashboard integration suite.",
        project_id: server.tenant.projectId,
        repository_id: server.tenant.repositoryId,
      };
      const first = await client.submitTask(input, { idempotencyKey: "live-key-1" });
      expect(first.deduplicated).toBe(false);
      const replay = await client.submitTask(input, { idempotencyKey: "live-key-1" });
      expect(replay.deduplicated).toBe(true);
      expect(replay.task_id).toBe(first.task_id);

      // The new lookup: task -> current run, tenant-scoped.
      const run = await client.getTaskRun(first.task_id);
      expect(run.id).toBe(first.run_id);
      expect(run.state).toBe("created");

      // The catalog reflects the seeded tenant.
      const projects = await client.listProjects();
      expect(projects.some((p) => p.id === server.tenant.projectId)).toBe(true);

      // One mounted shell, real responses all the way down.
      const router = createRouter({
        history: createMemoryHistory(),
        routes: [
          { path: "/", component: { template: "<div />" } },
          { path: "/tasks", component: { template: "<div />" } },
          { path: "/tasks/:taskId", component: TaskDetailView },
        ],
      });
      await router.push("/tasks/" + first.task_id);
      await router.isReady();
      const wrapper = mount(App, {
        props: { client },
        global: { plugins: [router] },
      });
      await flushPromises();

      expect(await untilText(wrapper, "api healthy")).toBe(true);
      expect(await untilText(wrapper, "Dashboard live intake")).toBe(true);
      expect(await untilText(wrapper, "created")).toBe(true);
      wrapper.unmount();
    } finally {
      await server.stop();
    }
  }, 120_000);
});
