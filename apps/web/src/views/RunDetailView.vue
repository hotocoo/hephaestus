<script setup lang="ts">
/**
 * One workflow run: position on the lifecycle spine, both human
 * gates, and the append-only event history. Reached directly (from a
 * receipt link) or from a task page.
 */
import { computed } from "vue";
import { useRoute } from "vue-router";
import { PhCaretLeft } from "@phosphor-icons/vue";
import type { Deployment, Gate } from "@hephaestus/sdk";
import { useControlPlane, notFoundToNull } from "@/api/client";
import { useAsyncResource } from "@/composables/useAsyncResource";
import { usePolling } from "@/composables/usePolling";
import { loadConfig } from "@/config";
import { formatDateTime } from "@/lib/format";
import EventList from "@/components/EventList.vue";
import GatePanel from "@/components/GatePanel.vue";
import ResourceState from "@/components/ResourceState.vue";
import StateTimeline from "@/components/StateTimeline.vue";

const route = useRoute();
const client = useControlPlane();

const runId = computed(() => {
  const raw = route.params.runId;
  return typeof raw === "string" ? raw : "";
});

const run = useAsyncResource(() => client.getRun(runId.value), { watch: runId });

const events = useAsyncResource(() => client.listRunEvents(runId.value, { limit: 100 }), {
  watch: runId,
});

/** Gates appear only once the pipeline opens them; absence is normal. */
const approvalGate = useAsyncResource(
  (): Promise<Gate | null> =>
    client.getApprovalGate(runId.value).catch<Gate | null>(notFoundToNull),
  { watch: [run.data] },
);
const mergeGate = useAsyncResource(
  (): Promise<Gate | null> =>
    client.getMergeGate(runId.value).catch<Gate | null>(notFoundToNull),
  { watch: [run.data] },
);

/** Most runs never deploy; absence renders as no panel at all. */
const deployment = useAsyncResource(
  (): Promise<Deployment | null> =>
    client.getRunDeployment(runId.value).catch<Deployment | null>(notFoundToNull),
  { watch: [run.data] },
);

function reloadLive(): void {
  run.reload();
  events.reload();
}
usePolling(reloadLive, loadConfig().pollSeconds);

function reloadAfterDecision(): void {
  reloadLive();
  void approvalGate.reload();
  void mergeGate.reload();
}

/** Render model for the deployment panel; null hides the panel. */
const deploymentView = computed(() => {
  const current = deployment.data.value;
  if (current === null) return null;
  return {
    target: current.target,
    status: current.status,
    reason: current.failure_reason,
    created: formatDateTime(current.created_at),
  };
});

/** Render model so the template never casts server data. */
const view = computed(() => {
  const current = run.data.value;
  if (current === null) return null;
  return {
    taskId: current.task_id,
    state: current.state,
    since: formatDateTime(current.last_transition_at),
    attempt: current.attempt,
    correlation: current.correlation_id.slice(0, 8),
  };
});
</script>

<template>
  <header class="view-header">
    <div class="view-header__titles">
      <router-link
        v-if="view !== null"
        :to="'/tasks/' + view.taskId"
        class="cell-sub"
        style="display:inline-flex; align-items:center; gap:4px"
      >
        <PhCaretLeft :size="12" aria-hidden="true" /> parent task
      </router-link>
      <h1>Run</h1>
      <p v-if="view !== null" class="view-header__sub mono">
        {{ runId }}
      </p>
    </div>
    <button class="btn btn--ghost" :disabled="run.loading.value" @click="reloadLive">
      refresh
    </button>
  </header>

  <ResourceState :loading="run.loading.value" :error="run.error.value" :empty="false" />

  <template v-if="view !== null">
    <div class="panel-grid">
      <section class="panel" aria-label="workflow state">
        <div class="panel__head"><h2>Workflow</h2></div>
        <div class="panel__body">
          <StateTimeline :state="view.state" :since="view.since" />
          <dl class="kv" style="margin-top:14px">
            <dt>Attempt</dt>
            <dd class="num">{{ view.attempt }}</dd>
            <dt>Correlation</dt>
            <dd><code class="mono">{{ view.correlation }}</code></dd>
          </dl>
        </div>
      </section>

      <div>
        <section class="panel" aria-label="plan approval gate">
          <div class="panel__head"><h2>Plan approval</h2></div>
          <GatePanel
            kind="approval"
            :key="'approval-' + runId"
            :run-id="runId"
            :gate="approvalGate.data.value ?? null"
            @decided="reloadAfterDecision"
          />
        </section>
        <section class="panel" aria-label="merge gate">
          <div class="panel__head"><h2>Merge</h2></div>
          <GatePanel
            kind="merge"
            :key="'merge-' + runId"
            :run-id="runId"
            :gate="mergeGate.data.value ?? null"
            @decided="reloadAfterDecision"
          />
        </section>
        <section v-if="deploymentView !== null" class="panel" aria-label="deployment">
          <div class="panel__head"><h2>Deployment</h2></div>
          <div class="panel__body">
            <dl class="kv">
              <dt>Target</dt>
              <dd><code class="mono">{{ deploymentView.target }}</code></dd>
              <dt>Status</dt>
              <dd>{{ deploymentView.status }}</dd>
              <dt>Started</dt>
              <dd>{{ deploymentView.created }}</dd>
            </dl>
            <p
              v-if="deploymentView.reason !== null"
              class="cell-sub mono"
              style="margin-top:10px; overflow-wrap:anywhere"
            >
              {{ deploymentView.reason }}
            </p>
          </div>
        </section>
      </div>
    </div>

    <section class="panel" aria-label="events" style="margin-top:16px">
      <div class="panel__head"><h2>Events</h2></div>
      <EventList :resource="events" />
    </section>
  </template>
</template>