<script setup lang="ts">
/**
 * Task list and intake.
 *
 * The table renders every task of the organization with its current
 * run state (fetched for the visible page only); the intake form sits
 * above it and links straight to the created task's detail page.
 */
import { computed } from "vue";
import type { Run } from "@hephaestus/sdk";
import { useControlPlane } from "@/api/client";
import { useAsyncResource } from "@/composables/useAsyncResource";
import { useCatalog } from "@/composables/useCatalog";
import { usePolling } from "@/composables/usePolling";
import { loadConfig } from "@/config";
import { formatAge } from "@/lib/format";
import { priorityRank, priorityTone, riskTone } from "@/lib/workflow";
import ResourceState from "@/components/ResourceState.vue";
import StatusPill from "@/components/StatusPill.vue";
import TaskForm from "@/components/TaskForm.vue";

const client = useControlPlane();
const { projectName: catalogProjectName } = useCatalog();

const tasks = useAsyncResource(() => client.listTasks({ limit: 100 }));
usePolling(tasks.reload, loadConfig().pollSeconds);

const runs = useAsyncResource(async () => {
  const current = tasks.data.value ?? [];
  const fetched = await Promise.all(
    current.map((task) => client.getTaskRun(task.id).catch(() => null)),
  );
  const byTask = new Map<string, Run>();
  for (const [index, run] of fetched.entries()) {
    const task = current[index];
    if (run !== null && task !== undefined) byTask.set(task.id, run);
  }
  return byTask;
}, { watch: tasks.data });

interface Row {
  id: string;
  title: string;
  projectId: string;
  projectName: string | null;
  state: string | null;
  priority: string;
  risk: string;
  labels: string[];
  createdAt: string;
}

const rows = computed<Row[]>(() =>
  [...(tasks.data.value ?? [])]
    .sort(
      (a, b) =>
        priorityRank(a.priority) - priorityRank(b.priority) ||
        b.created_at.localeCompare(a.created_at),
    )
    .map((task) => ({
      id: task.id,
      title: task.title,
      projectId: task.project_id,
      projectName: catalogProjectName.value.get(task.project_id) ?? task.project_id.slice(0, 8),
      state: runs.data.value?.get(task.id)?.state ?? null,
      priority: task.priority,
      risk: task.risk,
      labels: readLabels(task.labels),
      createdAt: task.created_at,
    })),
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
      <h1>Tasks</h1>
      <p class="view-header__sub">Engineering work enters the pipeline here.</p>
    </div>
    <button
      class="btn btn--ghost"
      :disabled="tasks.loading.value"
      @click="tasks.reload(); runs.reload()"
    >
      refresh
    </button>
  </header>

  <section class="panel" aria-label="submit a task" style="margin-bottom:16px">
    <div class="panel__head"><h2>Submit work</h2></div>
    <TaskForm />
  </section>

  <section class="panel" aria-label="task list">
    <div class="panel__head">
      <h2>All tasks</h2>
      <span class="cell-sub num">{{ rows.length }}</span>
    </div>
    <ResourceState
      :loading="tasks.loading.value"
      :error="tasks.error.value"
      :empty="rows.length === 0"
      empty-text="no tasks yet - submit the first one above"
    />
    <div
      v-if="!tasks.loading.value && !tasks.error.value"
      class="table-wrap"
    >
      <table class="table">
        <thead>
          <tr>
            <th>Title</th>
            <th>Project</th>
            <th>State</th>
            <th>Priority</th>
            <th>Risk</th>
            <th>Labels</th>
            <th>Created</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="row in rows"
            :key="row.id"
            class="is-clickable"
            @click="$router.push('/tasks/' + row.id)"
          >
            <td>
              <div class="cell-title">{{ row.title }}</div>
              <div class="cell-sub mono">{{ row.id.slice(0, 8) }}</div>
            </td>
            <td>{{ row.projectName }}</td>
            <td>
              <StatusPill v-if="row.state !== null" :value="row.state" />
              <span v-else class="cell-sub">…</span>
            </td>
            <td><StatusPill :value="row.priority" :tone="priorityTone(row.priority)" /></td>
            <td><StatusPill :value="row.risk" :tone="riskTone(row.risk)" /></td>
            <td>
              <div class="chips">
                <span v-for="label in row.labels.slice(0, 3)" :key="label" class="pill pill--outline">
                  {{ label }}
                </span>
              </div>
            </td>
            <td class="num" :title="row.createdAt">{{ formatAge(row.createdAt) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>