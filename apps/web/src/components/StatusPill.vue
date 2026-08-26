<script setup lang="ts">
/**
 * A classified value rendered as a tone-coded pill.
 *
 * Tones come from lib/workflow so every view paints the same state
 * vocabulary; unknown values fall back to the neutral pill.
 */
import { computed } from "vue";
import { stateTone } from "@/lib/workflow";

const props = defineProps<{
  value: string;
  /** Fixed tone override for non-workflow classifications (priority/risk). */
  tone?: "idle" | "busy" | "ok" | "bad" | "warn" | "info";
}>();

const cssClass = computed(() => {
  if (props.tone !== undefined) return "pill--" + props.tone;
  return "pill--" + stateTone(props.value);
});

const label = computed(() => props.value.replaceAll("_", " "));
</script>

<template>
  <span class="pill" :class="cssClass">{{ label }}</span>
</template>
