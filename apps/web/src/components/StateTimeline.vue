<script setup lang="ts">
/**
 * A run's position on the workflow lifecycle spine.
 *
 * Every state renders in lifecycle order with its distance marker;
 * the server-declared current state is highlighted. Terminal failure
 * and cancellation keep the distance they reached - a dead run still
 * shows how far it got.
 */
import { computed } from "vue";
import { WORKFLOW_STATES, isTerminalState, stateProgress, stateTone } from "@/lib/workflow";

const props = defineProps<{
  state: string;
  /** Last transition time from the run, rendered next to the active step. */
  since?: string | null;
}>();

const progress = computed(() => stateProgress(props.state));
const terminal = computed(() => isTerminalState(props.state));
const tone = computed(() => stateTone(props.state));

function stepClass(index: number): Record<string, boolean> {
  return {
    "timeline__step--done": index < progress.value.reachedIndex,
    "timeline__step--current": index === progress.value.reachedIndex && !terminal.value,
    "timeline__step--bad": terminal.value && tone.value === "bad" && index === progress.value.reachedIndex,
  };
}
</script>

<template>
  <div>
    <div class="timeline" :aria-label="'workflow state: ' + state">
      <div
        v-for="(name, index) in WORKFLOW_STATES"
        :key="name"
        class="timeline__step"
        :class="stepClass(index)"
      >
        <span class="timeline__dot" aria-hidden="true"></span>
        <span class="timeline__name">{{ name }}</span>
        <span v-if="index === progress.reachedIndex" class="timeline__when">{{ since ?? "" }}</span>
      </div>
    </div>
    <div
      class="progressbar"
      role="img"
      :aria-label="'progress ' + progress.percent + '%'"
    >
      <div
        class="progressbar__fill"
        :class="{ 'progressbar__fill--bad': tone === 'bad' }"
        :style="{ width: progress.percent + '%' }"
      ></div>
    </div>
    <p v-if="!progress.known" class="cell-sub" style="margin-top:8px">
      unknown state "{{ state }}" - this UI predates the server; rendering verbatim.
    </p>
  </div>
</template>
