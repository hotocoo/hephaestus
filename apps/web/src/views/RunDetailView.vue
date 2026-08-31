<script setup lang="ts">
/**
 * One workflow run: position on the lifecycle spine, both human
 * gates, and the append-only event history. Reached directly (from a
 * receipt link) or from a task page.
 */
import { computed, ref } from "vue";
import { useRoute } from "vue-router";
import { PhCaretLeft, PhDownloadSimple, PhShieldCheck } from "@phosphor-icons/vue";
import type { Artifact, ArtifactVerification, Deployment, Gate } from "@hephaestus/sdk";
import { useControlPlane, notFoundToNull } from "@/api/client";
import { formatBytes } from "@/lib/format";
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

/**
 * Registered build artifacts (ADR-015). Runs that never built render
 * an empty panel; the list is the honest evidence of what shipped.
 */
const artifacts = useAsyncResource(
  (): Promise<Artifact[]> => client.listRunArtifacts(runId.value),
  { watch: [run.data] },
);

/** Per-artifact verification results, keyed by artifact id. */
const verifications = ref(new Map<string, ArtifactVerification>());
const verifying = ref(new Set<string>());
const downloading = ref(new Set<string>());

async function verifyOne(artifactId: string): Promise<void> {
  if (verifying.value.has(artifactId)) return;
  verifying.value.add(artifactId);
  try {
    const result = await client.verifyArtifact(runId.value, artifactId);
    const next = new Map(verifications.value);
    next.set(artifactId, result);
    verifications.value = next;
  } catch {
    // A failed verification is itself information; the row shows the
    // button again so the operator can retry without losing the list.
  } finally {
    const next = new Set(verifying.value);
    next.delete(artifactId);
    verifying.value = next;
  }
}

/**
 * Download through the typed client and save via a blob URL; the
 * bearer token never rides the URL (ADR-015).
 */
async function downloadOne(artifactId: string): Promise<void> {
  if (downloading.value.has(artifactId)) return;
  downloading.value.add(artifactId);
  try {
    const blob = await client.downloadArtifact(runId.value, artifactId);
    const artifact = (artifacts.data.value ?? []).find((a) => a.id === artifactId);
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = artifact?.path.split("/").pop() ?? "artifact";
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    URL.revokeObjectURL(url);
  } finally {
    const next = new Set(downloading.value);
    next.delete(artifactId);
    downloading.value = next;
  }
}

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

/** Render model for the artifacts panel; empty when nothing built. */
const artifactRows = computed(() => {
  const list = artifacts.data.value ?? [];
  return list.map((artifact) => ({
    id: artifact.id,
    path: artifact.path,
    size: formatBytes(artifact.size_bytes),
    digest: artifact.sha256.slice(0, 12),
    verification: verifications.value.get(artifact.id) ?? null,
    isVerifying: verifying.value.has(artifact.id),
    isDownloading: downloading.value.has(artifact.id),
  }));
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
        <section class="panel" aria-label="artifacts">
          <div class="panel__head"><h2>Artifacts</h2></div>
          <div class="panel__body">
            <ResourceState
              :loading="artifacts.loading.value"
              :error="artifacts.error.value"
              :empty="artifactRows.length === 0"
              empty-text="No artifacts registered yet"
            />
            <ul v-if="artifactRows.length > 0" class="artifact-list">
              <li v-for="row in artifactRows" :key="row.id" class="artifact-row">
                <div class="artifact-row__meta">
                  <code class="mono artifact-row__path">{{ row.path }}</code>
                  <span class="cell-sub">
                    {{ row.size }} · sha256:{{ row.digest }}
                  </span>
                </div>
                <div class="artifact-row__actions">
                  <span
                    v-if="row.verification !== null"
                    class="artifact-status"
                    :class="'artifact-status--' + row.verification.status"
                  >
                    {{ row.verification.status }}
                  </span>
                  <button
                    class="btn btn--ghost"
                    :disabled="row.isVerifying"
                    @click="verifyOne(row.id)"
                  >
                    <PhShieldCheck :size="13" aria-hidden="true" />
                    {{ row.isVerifying ? "checking" : "verify" }}
                  </button>
                  <button
                    class="btn btn--ghost"
                    :disabled="row.isDownloading"
                    @click="downloadOne(row.id)"
                  >
                    <PhDownloadSimple :size="13" aria-hidden="true" />
                    {{ row.isDownloading ? "saving" : "download" }}
                  </button>
                </div>
              </li>
            </ul>
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