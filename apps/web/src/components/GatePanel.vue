<script setup lang="ts">
/**
 * A human decision point (plan approval or merge confirmation).
 *
 * The panel is deliberately narrow: it shows the gate's recorded
 * state, collects exactly the fields the wire contract accepts, and
 * forwards them through the control plane. The approver is always the
 * authenticated principal server-side; nothing here can attribute a
 * decision to someone else.
 */
import { computed, ref } from "vue";
import type { Gate } from "@hephaestus/sdk";
import { PhCheckCircle, PhGitMerge, PhSealCheck } from "@phosphor-icons/vue";
import { useControlPlane } from "@/api/client";
import type { ControlPlane } from "@/api/client";
import { toResourceError } from "@/composables/useAsyncResource";
import type { ResourceError } from "@/composables/useAsyncResource";

const props = defineProps<{
  kind: "approval" | "merge";
  runId: string;
  /** Undecided gate when one exists; null renders the parked state. */
  gate: Gate | null;
}>();

const emit = defineEmits<{
  /** The run changed on the server; parents should reload their views. */
  decided: [];
}>();

const client: ControlPlane = useControlPlane();

const busy = ref(false);
const error = ref<ResourceError | null>(null);
const outcomeText = ref<string | null>(null);

const reason = ref("");
const externalRef = ref("");

/** Which decision the gate row already carries, if any. */
const decided = computed(() => props.gate?.decision ?? null);

function trimmed(value: string): string | undefined {
  const text = value.trim();
  return text.length > 0 ? text : undefined;
}

async function decideApproval(approved: boolean): Promise<void> {
  busy.value = true;
  error.value = null;
  outcomeText.value = null;
  try {
    const outcome = await client.decideApproval(props.runId, {
      approved,
      reason: trimmed(reason.value),
    });
    outcomeText.value =
      outcome.outcome === "approved"
        ? "approved - execution active" + (outcome.enqueued_step !== null ? ", first step enqueued" : "")
        : "rejected - the run returned to planning for revision";
    emit("decided");
  } catch (thrown) {
    error.value = toResourceError(thrown);
  } finally {
    busy.value = false;
  }
}

async function decideMerge(): Promise<void> {
  busy.value = true;
  error.value = null;
  outcomeText.value = null;
  try {
    const outcome = await client.decideMerge(props.runId, {
      reason: trimmed(reason.value),
      external_ref: trimmed(externalRef.value),
    });
    outcomeText.value =
      outcome.outcome === "merged"
        ? "merge recorded - delivery pipeline chained"
        : "already merged - replayed decision";
    emit("decided");
  } catch (thrown) {
    error.value = toResourceError(thrown);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="panel__body">
    <template v-if="gate === null">
      <p class="cell-sub">no {{ kind }} gate exists for this run yet</p>
    </template>
    <template v-else-if="decided !== null">
      <p style="display:flex; align-items:center; gap:8px; color: var(--ok)">
        <PhSealCheck :size="18" aria-hidden="true" />
        decided: <b>{{ decided }}</b>
      </p>
      <dl class="kv" style="margin-top:10px">
        <dt>Gate id</dt>
        <dd><code class="mono">{{ gate.approval_id }}</code></dd>
        <dt>Required role</dt>
        <dd>{{ gate.required_role }}</dd>
      </dl>
    </template>
    <template v-else>
      <div class="field" style="margin-bottom:12px">
        <label :for="kind + '-reason'">Reason (optional, persisted with the decision)</label>
        <input
          :id="kind + '-reason'"
          v-model="reason"
          class="input"
          type="text"
          :placeholder="kind === 'approval' ? 'why this plan is right' : 'why this was merged'"
          :disabled="busy"
        />
      </div>
      <div v-if="kind === 'merge'" class="field" style="margin-bottom:12px">
        <label for="merge-ref">External reference (commit sha / PR number)</label>
        <input
          id="merge-ref"
          v-model="externalRef"
          class="input"
          type="text"
          placeholder="e.g. PR #42"
          :disabled="busy"
        />
      </div>

      <div style="display:flex; gap:10px; flex-wrap:wrap">
        <template v-if="kind === 'approval'">
          <button class="btn btn--primary" :disabled="busy" @click="decideApproval(true)">
            <PhCheckCircle :size="16" aria-hidden="true" />
            {{ busy ? "sending…" : "Approve plan" }}
          </button>
          <button class="btn btn--danger" :disabled="busy" @click="decideApproval(false)">
            {{ busy ? "sending…" : "Reject plan" }}
          </button>
        </template>
        <template v-else>
          <button class="btn btn--primary" :disabled="busy" @click="decideMerge()">
            <PhGitMerge :size="16" aria-hidden="true" />
            {{ busy ? "recording…" : "Record merge" }}
          </button>
        </template>
      </div>

      <dl class="kv" style="margin-top:14px">
        <dt>Gate id</dt>
        <dd><code class="mono">{{ gate.approval_id }}</code></dd>
        <dt>Required role</dt>
        <dd>{{ gate.required_role }}</dd>
      </dl>
    </template>

    <p v-if="error" class="notice notice--error" role="alert" style="margin-top:12px">
      decision failed<template v-if="error.code"> ({{ error.code }})</template>: {{ error.message }}
    </p>
    <p v-else-if="outcomeText" class="form-ok" style="margin-top:12px">{{ outcomeText }}</p>
  </div>
</template>
