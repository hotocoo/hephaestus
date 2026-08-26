<script setup lang="ts">
/**
 * Task intake.
 *
 * Mirrors the wire contract's CreateTaskRequest exactly: title,
 * description, project and repository are required; priority, risk
 * and labels are optional. Every submission carries an idempotency
 * key so a retry can never double-create work - the key is generated
 * here, shown to the operator, and travels with the receipt link.
 */
import { computed, onMounted, ref } from "vue";
import { useRouter } from "vue-router";
import { PhPaperPlaneTilt } from "@phosphor-icons/vue";
import type { CreateTaskInput, Project, Repository } from "@hephaestus/sdk";
import { useControlPlane } from "@/api/client";
import { toResourceError } from "@/composables/useAsyncResource";
import type { ResourceError } from "@/composables/useAsyncResource";

const client = useControlPlane();
const router = useRouter();

const projects = ref<Project[]>([]);
const repositories = ref<Repository[]>([]);
const loadError = ref<ResourceError | null>(null);
const loadingCatalog = ref(true);

onMounted(async () => {
  try {
    const [projectRows, repositoryRows] = await Promise.all([
      client.listProjects({ limit: 200 }),
      client.listRepositories({ limit: 200 }),
    ]);
    // The API scopes both lists by the token's organization; the form
    // only offers repositories that actually belong to the project.
    projects.value = projectRows;
    repositories.value = repositoryRows;
  } catch (thrown) {
    loadError.value = toResourceError(thrown);
  } finally {
    loadingCatalog.value = false;
  }
});

const title = ref("");
const description = ref("");
const projectId = ref<string>("");
const repositoryId = ref<string>("");
const priority = ref<string>("");
const risk = ref("");
const labelsText = ref("");

const submitting = ref(false);
const submitError = ref<ResourceError | null>(null);

/** Web Crypto is available in every browser context this app runs in. */
function freshKey(): string {
  return globalThis.crypto.randomUUID();
}

const idempotencyKey = ref(freshKey());
const regenerateKey = (): void => {
  idempotencyKey.value = freshKey();
};

/** Repositories of the selected project only. */
const projectRepositories = computed(() =>
  repositories.value.filter((r) => r.project_id === projectId.value),
);

const valid = computed(
  () =>
    title.value.trim().length > 0 &&
    description.value.trim().length > 0 &&
    projectId.value !== "" &&
    repositoryId.value !== "",
);

type PriorityValue = NonNullable<CreateTaskInput["priority"]>;
type RiskValue = NonNullable<CreateTaskInput["risk"]>;

const PRIORITIES: readonly PriorityValue[] = ["critical", "high", "medium", "low"];
const RISKS: readonly RiskValue[] = ["low", "medium", "high", "critical"];

/** The select binds strings; only known names travel to the API. */
function asPriority(value: string): PriorityValue | undefined {
  return PRIORITIES.find((p) => p === value);
}

function asRisk(value: string): RiskValue | undefined {
  return RISKS.find((r) => r === value);
}

function parseLabels(): string[] | undefined {
  const labels = labelsText.value
    .split(",")
    .map((label) => label.trim().toLowerCase())
    .filter((label) => label.length > 0);
  return labels.length > 0 ? Array.from(new Set(labels)) : undefined;
}

async function submit(): Promise<void> {
  if (!valid.value || submitting.value) return;
  submitting.value = true;
  submitError.value = null;
  try {
    const receipt = await client.submitTask(
      {
        title: title.value.trim(),
        description: description.value,
        project_id: projectId.value,
        repository_id: repositoryId.value,
        priority: asPriority(priority.value),
        risk: asRisk(risk.value),
        labels: parseLabels(),
      },
      { idempotencyKey: idempotencyKey.value },
    );
    await router.push("/tasks/" + receipt.task_id + "?submitted=1");
  } catch (thrown) {
    submitError.value = toResourceError(thrown);
    // A failed submission may or may not have landed server-side;
    // never reuse the same idempotency key for edited content.
    regenerateKey();
  } finally {
    submitting.value = false;
  }
}
</script>

<template>
  <div class="panel__body">
    <p v-if="loadingCatalog" class="loading-row" role="status">
      <span class="spinner" aria-hidden="true"></span> loading catalog…
    </p>
    <div v-else-if="loadError" class="notice notice--error" role="alert">
      catalog unavailable<template v-if="loadError.code"> ({{ loadError.code }})</template>: {{ loadError.message }}
    </div>
    <form v-else class="form-grid" @submit.prevent="submit">
      <div class="field field--wide">
        <label for="task-title">Title</label>
        <input
          id="task-title"
          v-model="title"
          class="input"
          type="text"
          maxlength="512"
          placeholder="what should be done"
          required
        />
      </div>

      <div class="field field--wide">
        <label for="task-description">Description</label>
        <textarea
          id="task-description"
          v-model="description"
          class="textarea"
          placeholder="context, constraints, acceptance criteria"
          required
        ></textarea>
      </div>

      <div class="field">
        <label for="task-project">Project</label>
        <select id="task-project" v-model="projectId" class="select" required>
          <option value="" disabled>choose…</option>
          <option v-for="project in projects" :key="project.id" :value="project.id">
            {{ project.name }}
          </option>
        </select>
      </div>

      <div class="field">
        <label for="task-repository">Repository</label>
        <select
          id="task-repository"
          v-model="repositoryId"
          class="select"
          :disabled="projectId === ''"
          required
        >
          <option value="" disabled>choose…</option>
          <option v-for="repo in projectRepositories" :key="repo.id" :value="repo.id">
            {{ repo.display_name }} ({{ repo.default_branch }})
          </option>
        </select>
      </div>

      <div class="field">
        <label for="task-priority">Priority</label>
        <select id="task-priority" v-model="priority" class="select">
          <option value="">medium (default)</option>
          <option value="low">low</option>
          <option value="high">high</option>
          <option value="critical">critical</option>
        </select>
      </div>

      <div class="field">
        <label for="task-risk">Risk</label>
        <select id="task-risk" v-model="risk" class="select">
          <option value="">medium (default)</option>
          <option value="low">low</option>
          <option value="high">high</option>
          <option value="critical">critical</option>
        </select>
      </div>

      <div class="field field--wide">
        <label for="task-labels">Labels (comma-separated)</label>
        <input
          id="task-labels"
          v-model="labelsText"
          class="input"
          type="text"
          placeholder="bug, upload"
        />
      </div>

      <div class="field field--wide">
        <label for="task-idem">Idempotency key</label>
        <div style="display:flex; gap:8px">
          <input id="task-idem" class="input mono" type="text" :value="idempotencyKey" readonly />
          <button class="btn btn--ghost" type="button" title="new key" @click="regenerateKey">
            ↻
          </button>
        </div>
        <span class="cell-sub">reusing this form after an error generates a fresh key automatically</span>
      </div>

      <div class="field--wide" style="display:flex; align-items:center; gap:14px; flex-wrap:wrap">
        <button class="btn btn--primary" type="submit" :disabled="!valid || submitting">
          <PhPaperPlaneTilt :size="16" aria-hidden="true" />
          {{ submitting ? "submitting…" : "Submit task" }}
        </button>
        <p v-if="submitError" class="form-error" role="alert">
          submission failed<template v-if="submitError.code"> ({{ submitError.code }})</template>:
          {{ submitError.message }}
        </p>
      </div>
    </form>
  </div>
</template>