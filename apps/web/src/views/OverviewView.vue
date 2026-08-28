<script setup lang="ts">
/**
 * Control-plane overview: catalog size, where recent work stands on
 * the workflow spine, and the newest tasks. Everything renders from
 * list endpoints; counts are derived client-side from real rows,
 * never invented. Run states are fetched only for the recent page -
 * the overview never fans out over the whole task list.
 */
import { computed } from "vue";
import { PhFlame, PhFolderOpen } from "@phosphor-icons/vue";
import type { Run, Task } from "@hephaestus/sdk";
import { useControlPlane, notFoundToNull } from "@/api/client";
import { useAsyncResource } from "@/composables/useAsyncResource";
import { useCatalog } from "@/composables/useCatalog";
import { usePolling } from "@/composables/usePolling";
import { loadConfig } from "@/config";
import { formatAge } from "@/lib/format";
import { priorityTone, riskTone, stateTone, WORKFLOW_STATES } from "@/lib/workflow";
import ResourceState from "@/components/ResourceState.vue";
import StatusPill from "@/components/StatusPill.vue";

const client = useControlPlane();
const { projectName, projectCount, repositoryCount, error: catalogError, reload: reloadCatalog } =
  useCatalog();

const tasks = useAsyncResource(() => client.listTasks({ limit: 200 }));

const RECENT_COUNT = 8;
const recentTasks = computed<Task[]>(() =>
  [...(tasks.data.value ?? [])]
    .sort((a, b) => b.created_at.localeCompare(a.created_at))
    .slice(0, RECENT_COUNT),
);

/**
 * Current run per recent task; a missing row (run not bootstrapped
 * yet) simply renders no state pill. Only a 404 counts as "no run" -
 * any other failure surfaces instead of masquerading as absence.
 */
const recentRuns = useAsyncResource(async () => {
  const current = recentTasks.value;
  const runs = await Promise.all(
    current.map((task) => client.getTaskRun(task.id).catch<Run | null>(notFoundToNull)),
  );
  const byTask = new Map<string, Run>();
  for (const [index, run] of runs.entries()) {
    const task = current[index];
    if (run !== null && task !== undefined) byTask.set(task.id, run);
  }
  return byTask;
}, { watch: recentTasks });

/** Table rows pair each recent task with its current run. */
const recentRows = computed(() =>
  recentTasks.value.map((task) => ({
    task,
    run: recentRuns.data.value?.get(task.id) ?? null,
  })),
);

/** Recent work grouped along the lifecycle spine. */
const stateCounts = computed(() => {
  const counts = new Map<string, number>();
  for (const run of recentRuns.data.value?.values() ?? []) {
    counts.set(run.state, (counts.get(run.state) ?? 0) + 1);
  }
  return WORKFLOW_STATES.map((state) => ({
    name: state as string,
    count: counts.get(state) ?? 0,
    tone: stateTone(state),
  })).filter((entry) => entry.count > 0);
});

function reloadAll(): void {
  tasks.reload();
  void reloadCatalog();
}
usePolling(reloadAll, loadConfig().pollSeconds);
</script>

<template>
  <header class="view-header">
    <div class="view-header__titles">
      <h1>Overview</h1>
      <p class="view-header__sub">
        AI proposes and executes engineering work; deterministic systems verify it.
      </p>
    </div>
    <button class="btn btn--ghost" :disabled="tasks.loading.value" @click="reloadAll">
      refresh
    </button>
  </header>

  <section aria-label="organization" class="statgrid" style="margin-bottom:16px">
    <div class="stat">
      <span class="stat__label">Projects</span>
      <div class="stat__value">{{ projectCount }}</div>
    </div>
    <div class="stat">
      <span class="stat__label">Repositories</span>
      <div class="stat__value">{{ repositoryCount }}</div>
    </div>
    <div class="stat" title="tasks on the most recent page (up to 200)">
      <span class="stat__label">Tasks</span>
      <div class="stat__value">{{ tasks.data.value?.length ?? "—" }}</div>
    </div>
  </section>

  <div
    v-if="catalogError"
    class="notice notice--error"
    role="alert"
    style="margin-bottom:16px"
  >
    catalog unavailable ({{ catalogError.code }}): {{ catalogError.message }}
  </div>

  <div class="panel-grid" style="margin-bottom:16px">
    <section class="panel" aria-label="workflow states of recent tasks">
      <div class="panel__head">
        <h2><PhFlame :size="14" aria-hidden="true" /> States · recent</h2>
      </div>
      <div class="panel__body">
        <p v-if="stateCounts.length === 0" class="cell-sub">
          nothing in flight on this page yet
        </p>
        <dl v-else class="kv">
          <template v-for="state in stateCounts" :key="state.name">
            <dt style="display:flex; align-items:center; gap:8px">
              <StatusPill :value="state.name" />
            </dt>
            <dd class="num">{{ state.count }}</dd>
          </template>
        </dl>
      </div>
    </section>

    <section class="panel" aria-label="projects">
      <div class="panel__head">
        <h2><PhFolderOpen :size="14" aria-hidden="true" /> Projects</h2>
      </div>
      <ul style="margin:0; padding:8px 0; list-style:none">
        <li v-if="projectCount === 0" class="cell-sub" style="padding:6px 16px">
          none provisioned
        </li>
        <li
          v-for="[id, name] in projectName"
          :key="id"
          style="padding:6px 16px; display:flex; justify-content:space-between; gap:10px"
        >
          <span>{{ name }}</span>
          <code class="cell-sub">{{ id.slice(0, 8) }}</code>
        </li>
      </ul>
    </section>
  </div>

  <section class="panel" aria-label="recent tasks">
    <div class="panel__head"><h2>Recent tasks</h2></div>
    <ResourceState
      :loading="tasks.loading.value"
      :error="tasks.error.value"
      :empty="(tasks.data.value ?? []).length === 0"
      empty-text="no tasks yet - submit the first one from the Tasks view"
    />
    <div
      v-if="!tasks.loading.value && !tasks.error.value"
      class="table-wrap"
    >
      <table class="table">
        <thead>
          <tr>
            <th>Title</th>
            <th>State</th>
            <th>Priority</th>
            <th>Risk</th>
            <th>Created</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="row in recentRows"
            :key="row.task.id"
            class="is-clickable"
            @click="$router.push('/tasks/' + row.task.id)"
          >
            <td>
              <router-link :to="'/tasks/' + row.task.id" class="cell-title row-link">
                {{ row.task.title }}
              </router-link>
              <div class="cell-sub mono">{{ row.task.id.slice(0, 8) }}</div>
            </td>
            <td>
              <StatusPill v-if="row.run !== null" :value="row.run.state" />
              <span v-else class="cell-sub">no run</span>
            </td>
            <td><StatusPill :value="row.task.priority" :tone="priorityTone(row.task.priority)" /></td>
            <td><StatusPill :value="row.task.risk" :tone="riskTone(row.task.risk)" /></td>
            <td class="num" :title="row.task.created_at">{{ formatAge(row.task.created_at) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>