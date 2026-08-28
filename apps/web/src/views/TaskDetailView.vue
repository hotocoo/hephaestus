<script setup lang="ts">
/**
 * One task end to end: fields, current plan, live workflow state with
 * its gates, and the run's event history. This is where a human makes
 * plan-approval decisions when the run parks at awaiting_approval.
 */
import { computed } from "vue";
import { useRoute } from "vue-router";
import { PhCaretLeft, PhCaretRight, PhLinkBreak } from "@phosphor-icons/vue";
import type { Gate, Plan, Run } from "@hephaestus/sdk";
import { useControlPlane, notFoundToNull } from "@/api/client";
import { useAsyncResource } from "@/composables/useAsyncResource";
import { useCatalog } from "@/composables/useCatalog";
import { usePolling } from "@/composables/usePolling";
import { loadConfig } from "@/config";
import { formatDateTime } from "@/lib/format";
import { priorityTone, riskTone } from "@/lib/workflow";
import EventList from "@/components/EventList.vue";
import GatePanel from "@/components/GatePanel.vue";
import PlanView from "@/components/PlanView.vue";
import ResourceState from "@/components/ResourceState.vue";
import StateTimeline from "@/components/StateTimeline.vue";
import StatusPill from "@/components/StatusPill.vue";

const route = useRoute();
const client = useControlPlane();
const { projectName: catalogProjectName, repositoryName: catalogRepositoryName } = useCatalog();

const taskId = computed(() => {
  const raw = route.params.taskId;
  return typeof raw === "string" ? raw : "";
});

const task = useAsyncResource(() => client.getTask(taskId.value), {
  watch: taskId,
});
/** No plan yet is an empty state, not a failure. */
const plan = useAsyncResource(
  () => client.getTaskPlan(taskId.value).catch<Plan | null>(notFoundToNull),
  { watch: taskId },
);
/** A task can exist before its run is bootstrapped; that renders as "no run". */
const run = useAsyncResource(
  () => client.getTaskRun(taskId.value).catch<Run | null>(notFoundToNull),
  { watch: taskId },
);

const events = useAsyncResource(async () => {
  const id = runIdOrEmpty();
  if (id === "") return [];
  return client.listRunEvents(id, { limit: 100 });
}, { watch: [run.data] });
function runIdOrEmpty(): string {
  return run.data.value?.id ?? "";
}

/** Gates appear only once the pipeline opens them; absence is normal. */
const approvalGate = useAsyncResource(
  (): Promise<Gate | null> => {
    const id = runIdOrEmpty();
    if (id === "") return Promise.resolve(null);
    return client.getApprovalGate(id).catch<Gate | null>(notFoundToNull);
  },
  { watch: [run.data] },
);

const mergeGate = useAsyncResource(
  (): Promise<Gate | null> => {
    const id = runIdOrEmpty();
    if (id === "") return Promise.resolve(null);
    return client.getMergeGate(id).catch<Gate | null>(notFoundToNull);
  },
  { watch: [run.data] },
);

function reloadLive(): void {
  run.reload();
  if (run.data.value !== null) events.reload();
}
usePolling(reloadLive, loadConfig().pollSeconds);

function reloadAfterDecision(): void {
  reloadLive();
  void approvalGate.reload();
  void mergeGate.reload();
}

const submittedBanner = computed(() => route.query.submitted === "1");

/** Render model so the template never casts server data. */
const runView = computed(() => {
  const current = run.data.value;
  if (current === null) return null;
  return {
    id: current.id,
    state: current.state,
    since: formatDateTime(current.last_transition_at),
  };
});

const currentPlan = computed(() =>
  !plan.loading.value && !plan.error.value ? plan.data.value : null,
);

/** Labels arrive as stored JSON; tolerate anything without crashing. */
function readLabels(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((label): label is string => typeof label === "string");
}
</script>

<template>
  <header class="view-header">
    <div class="view-header__titles">
      <router-link to="/tasks" class="cell-sub" style="display:inline-flex; align-items:center; gap:4px">
        <PhCaretLeft :size="12" aria-hidden="true" /> all tasks
      </router-link>
      <h1 style="overflow-wrap:anywhere">{{ task.data.value?.title ?? "Task" }}</h1>
      <p v-if="task.data.value" class="view-header__sub mono">
        {{ task.data.value.id }}
      </p>
    </div>
    <button
      class="btn btn--ghost"
      :disabled="task.loading.value"
      @click="reloadLive(); task.reload(); plan.reload()"
    >
      refresh
    </button>
  </header>

  <div
    v-if="submittedBanner"
    class="notice notice--empty"
    role="status"
    style="margin-bottom:16px"
  >
    task accepted - analysis starts when a worker picks it up
  </div>

  <ResourceState
    :loading="task.loading.value"
    :error="task.error.value"
    :empty="false"
  />
  <template v-if="!task.loading.value && !task.error.value && task.data.value !== null">
    <div class="panel-grid">
      <section class="panel" aria-label="workflow state">
        <div class="panel__head">
          <h2>Workflow</h2>
          <router-link
            v-if="runView !== null"
            :to="'/runs/' + runView.id"
            class="cell-sub"
            style="display:inline-flex; align-items:center; gap:4px"
          >
            run view <PhCaretRight :size="11" aria-hidden="true" />
          </router-link>
        </div>
        <div class="panel__body">
          <template v-if="runView !== null">
            <StateTimeline :state="runView.state" :since="runView.since" />
          </template>
          <p v-else-if="!run.loading.value" class="cell-sub">no run bootstrapped yet</p>
          <ResourceState
            v-else
            :loading="run.loading.value"
            :error="null"
            :empty="false"
          />
        </div>
      </section>

      <section class="panel" aria-label="task details">
        <div class="panel__head"><h2>Details</h2></div>
        <dl class="kv">
          <dt>Project</dt>
          <dd>{{ catalogProjectName.get(task.data.value.project_id) ?? task.data.value.project_id.slice(0, 8) }}</dd>
          <dt>Repository</dt>
          <dd>{{ catalogRepositoryName.get(task.data.value.repository_id) ?? task.data.value.repository_id.slice(0, 8) }}</dd>
          <dt>Priority</dt>
          <dd><StatusPill :value="task.data.value.priority" :tone="priorityTone(task.data.value.priority)" /></dd>
          <dt>Risk</dt>
          <dd><StatusPill :value="task.data.value.risk" :tone="riskTone(task.data.value.risk)" /></dd>
          <dt>Labels</dt>
          <dd>
            <span v-if="readLabels(task.data.value.labels).length === 0" class="cell-sub">none</span>
            <span v-else class="chips">
              <span v-for="label in readLabels(task.data.value.labels)" :key="label" class="pill pill--outline">{{ label }}</span>
            </span>
          </dd>
          <dt>Created</dt>
          <dd>{{ formatDateTime(task.data.value.created_at) }}</dd>
          <dt>Updated</dt>
          <dd>{{ formatDateTime(task.data.value.updated_at) }}</dd>
        </dl>
        <p class="cell-sub" style="margin-top:14px; white-space:pre-wrap; max-width:70ch">{{ task.data.value.description }}</p>
      </section>
    </div>

    <section class="panel" aria-label="plan" style="margin-top:16px">
      <div class="panel__head"><h2>Plan</h2></div>
      <ResourceState
        :loading="plan.loading.value"
        :error="plan.error.value"
        :empty="false"
      />
      <PlanView v-if="!plan.loading.value && !plan.error.value" :plan="currentPlan" />
    </section>

    <div class="panel-grid" style="margin-top:16px">
      <section class="panel" aria-label="plan approval gate">
        <div class="panel__head"><h2>Plan approval</h2></div>
        <GatePanel
          v-if="runView !== null"
          kind="approval"
          :key="'approval-' + runView.id"
          :run-id="runView.id"
          :gate="approvalGate.data.value ?? null"
          @decided="reloadAfterDecision"
        />
        <div v-else class="panel__body"><p class="cell-sub">waiting for the run…</p></div>
      </section>

      <section class="panel" aria-label="merge gate">
        <div class="panel__head"><h2>Merge</h2></div>
        <GatePanel
          v-if="runView !== null"
          kind="merge"
          :key="'merge-' + runView.id"
          :run-id="runView.id"
          :gate="mergeGate.data.value ?? null"
          @decided="reloadAfterDecision"
        />
        <div v-else class="panel__body"><p class="cell-sub">waiting for the run…</p></div>
      </section>
    </div>

    <section class="panel" aria-label="events" style="margin-top:16px">
      <div class="panel__head">
        <h2><PhLinkBreak :size="13" aria-hidden="true" /> Events</h2>
        <button class="btn btn--ghost" :disabled="events.loading.value" @click="events.reload()">
          refresh
        </button>
      </div>
      <EventList :resource="events" />
    </section>
  </template>
</template>