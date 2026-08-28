import { describe, expect, it } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createMemoryHistory, createRouter } from "vue-router";
import App from "../src/App.vue";
import RunDetailView from "../src/views/RunDetailView.vue";
import SetupView from "../src/views/SetupView.vue";
import TaskDetailView from "../src/views/TaskDetailView.vue";
import {
  FakeControlPlane,
  PROJECT_ID,
  REPO_ID,
  RUN_ID,
  TASK_ID,
  sampleDeployment,
  sampleRun,
  sampleTask,
} from "./fakes";

/**
 * Mount the real shell against the in-memory double, with the real
 * task-detail view wired under its route.
 */
async function mountApp(client: FakeControlPlane) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: "/", name: "overview", component: { template: "<div />" } },
      { path: "/tasks", name: "tasks", component: { template: "<div />" } },
      {
        path: "/tasks/:taskId",
        name: "task-detail",
        component: TaskDetailView,
      },
      { path: "/runs/:runId", name: "run-detail", component: { template: "<div />" } },
    ],
  });
  await router.push("/tasks/" + TASK_ID);
  await router.isReady();
  const wrapper = mount(App, {
    props: { client },
    global: { plugins: [router] },
  });
  await flushPromises();
  return wrapper;
}

/** Mount the real shell on the run-detail route (App owns the client). */
async function mountRunDetail(client: FakeControlPlane) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: "/", name: "overview", component: { template: "<div />" } },
      { path: "/tasks", name: "tasks", component: { template: "<div />" } },
      {
        path: "/tasks/:taskId",
        name: "task-detail",
        component: TaskDetailView,
      },
      {
        path: "/runs/:runId",
        name: "run-detail",
        component: RunDetailView,
      },
    ],
  });
  await router.push("/runs/" + RUN_ID);
  await router.isReady();
  const wrapper = mount(App, {
    props: { client },
    global: { plugins: [router] },
  });
  await flushPromises();
  return wrapper;
}
describe("setup gate", () => {
  it("renders operator instructions instead of an app without credentials", () => {
    const wrapper = mount(SetupView);
    expect(wrapper.text()).toContain("no API credentials");
    expect(wrapper.text()).toContain("token:");
    wrapper.unmount();
  });
});
describe("task detail flow", () => {
  it("renders task fields, plan and open approval gate", async () => {
    const client = new FakeControlPlane();
    client.tasks.push(sampleTask());
    const wrapper = await mountApp(client);

    const text = wrapper.text();
    expect(text).toContain("Fix the flaky upload");
    expect(text).toContain("Forge Core");
    expect(text).toContain("Make uploads deterministic.");
    expect(text).toContain("Adjust upload size guard");
    expect(text).toContain("Approve plan");
    expect(text).toContain("computed");
    expect(text).toContain("awaiting_approval");

    // The workflow panel links to the run's own view - the run detail
    // page is reachable from the task that owns it, not orphaned.
    const runLink = wrapper.findAll("a").find((a) => a.text().includes("run view"));
    expect(runLink?.attributes("href")).toContain("/runs/");

    wrapper.unmount();
  });
  it("forwards an approval decision through the client and refreshes", async () => {
    const client = new FakeControlPlane();
    client.tasks.push(sampleTask());
    const wrapper = await mountApp(client);

    const approve = wrapper.find("button.btn--primary");
    expect(approve.text()).toContain("Approve plan");
    await approve.trigger("click");
    await flushPromises();

    expect(client.decisions.approvals).toHaveLength(1);
    expect(client.decisions.approvals[0]?.input.approved).toBe(true);
    expect(wrapper.text()).toContain("decided: approved");
    expect(wrapper.text()).toContain("implementing");
    wrapper.unmount();
  });

  it("renders the no-plan empty state for tasks still analyzing", async () => {
    const client = new FakeControlPlane();
    client.tasks.push(sampleTask());
    client.plans.delete(TASK_ID);
    client.runs.set(TASK_ID, sampleRun({ state: "analyzing" }));
    client.approvalGates.clear();
    const wrapper = await mountApp(client);

    expect(wrapper.text()).toContain("no plan generated yet");
    expect(wrapper.text()).not.toContain("Approve plan");
    wrapper.unmount();
  });
});
describe("intake through the app", () => {
  it("records submissions with an idempotency key", async () => {
    const client = new FakeControlPlane();
    const input = {
      title: "Wire the run lookup",
      description: "d",
      project_id: PROJECT_ID,
      repository_id: REPO_ID,
    };
    const receipt = await client.submitTask(input, { idempotencyKey: "key-123" });
    expect(receipt.deduplicated).toBe(false);
    expect(client.decisions.submissions[0]?.key).toBe("key-123");

    const replay = await client.submitTask(input, { idempotencyKey: "key-123" });
    expect(replay.deduplicated).toBe(true);
    expect(replay.task_id).toBe(receipt.task_id);
  });
});

describe("run detail deployment panel", () => {
  it("renders a recorded deployment as data and hides absence", async () => {
    const client = new FakeControlPlane();
    // The fake pre-seeds one succeeded staging deployment for RUN_ID.
    const wrapper = await mountRunDetail(client);
    const text = wrapper.text();
    expect(text).toContain("Deployment");
    expect(text).toContain("staging");
    expect(text).toContain("succeeded");
    wrapper.unmount();

    // A run that never deployed shows no panel and no fabricated data.
    const quiet = new FakeControlPlane();
    quiet.deploymentsByRun.delete(RUN_ID);
    const emptyWrapper = await mountRunDetail(quiet);
    expect(emptyWrapper.text()).not.toContain("staging");
    emptyWrapper.unmount();

    // Failures surface their persisted reason verbatim.
    const failed = new FakeControlPlane();
    failed.deploymentsByRun.set(
      RUN_ID,
      sampleDeployment({ status: "failed", failure_reason: "heph-deploy failed (exit_code=Some(3))" }),
    );
    const failedWrapper = await mountRunDetail(failed);
    expect(failedWrapper.text()).toContain("failed");
    expect(failedWrapper.text()).toContain("heph-deploy failed (exit_code=Some(3))");
    failedWrapper.unmount();

    void sampleRun;
  });
});
