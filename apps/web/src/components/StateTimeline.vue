<script setup lang="ts">
/**
 * A run's position on the workflow lifecycle spine.
 *
 * Every state renders in lifecycle order with its distance marker;
 * the server-declared current state is highlighted. Terminal failure
 * and cancellation keep the distance they reached - a dead run still
 * shows how far it got. The current step carries its absolute
 * transition time and a relative age, so a run parked in one state
 * for an unusual stretch is legible at a glance instead of looking
 * like healthy work in flight.
 */
import { computed } from "vue";
import {
  WORKFLOW_STATES,
  isStaleState,
  isTerminalState,
  stateProgress,
  stateTone,
} from "@/lib/workflow";
import { formatAge, formatDateTime, formatDateTimeCompact } from "@/lib/format";

const props = defineProps<{
  state: string;
  /** Last transition time from the run, rendered next to the active step. */
  since?: string | null;
}>();

const progress = computed(() => stateProgress(props.state));
const terminal = computed(() => isTerminalState(props.state));
const tone = computed(() => stateTone(props.state));

const sinceValid = computed(() => {
  if (props.since === null || props.since === undefined) return false;
  return !Number.isNaN(Date.parse(props.since));
});

/** Relative age of the current state, refreshed on each render cycle. */
const dwell = computed(() =>
  sinceValid.value ? formatAge(props.since as string) : null,
);

/**
 * Presentation-only stall hint: a still-moveable state whose last
 * transition is unusually old. The server stays the sole authority
 * on run health; this only makes the recorded time impossible to miss.
 */
const stale = computed(() =>
  sinceValid.value ? isStaleState(props.state, props.since as string) : false,
);

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
        <span
          v-if="index === progress.reachedIndex && sinceValid"
          class="timeline__when"
          :class="{ 'timeline__when--stale': stale }"
          :title="formatDateTime(since as string) + (stale ? ' - no transition since' : '')"
        >
          <span class="timeline__when-abs">{{ formatDateTimeCompact(since as string) }}</span>
          <span v-if="dwell" class="timeline__when-age">{{ dwell }}</span>
        </span>
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
    <p v-if="stale" class="timeline__stale-hint" role="status">
      in <b>{{ state }}</b> since {{ formatDateTime(since as string) }} ({{ dwell }}) - no
      transition recorded since; check that a worker serving the next queue is running.
    </p>
    <p v-if="!progress.known" class="cell-sub" style="margin-top:8px">
      unknown state "{{ state }}" - this UI predates the server; rendering verbatim.
    </p>
  </div>
</template>
