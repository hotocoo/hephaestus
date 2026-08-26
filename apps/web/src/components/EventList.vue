<script setup lang="ts">
/**
 * Append-only event history for a run.
 *
 * Provenance is always visible (injection defense starts with making
 * origin obvious). Payloads render as bounded monospace text through
 * lib/format's renderer - data, never markup, never instructions.
 * Well-known transition payloads enrich the headline; anything else
 * falls back to raw rendering without failing.
 */
import { computed } from "vue";
import type { Event } from "@hephaestus/sdk";
import type { AsyncResource } from "@/composables/useAsyncResource";
import { formatAge, formatDateTime, renderPayload } from "@/lib/format";
import { workflowTransition } from "@/lib/events";
import type { WorkflowTransitionView } from "@/lib/events";
import ResourceState from "./ResourceState.vue";

const props = defineProps<{
  resource: AsyncResource<Event[]>;
}>();

interface EventView {
  id: string;
  occurredAt: string;
  age: string;
  provenance: string;
  aggregate: string;
  payloadText: string;
  transition: WorkflowTransitionView | null;
}

/** Newest first, as the API returns them. */
const views = computed<EventView[]>(() =>
  (props.resource.data.value ?? []).map((event) => ({
    id: event.id,
    occurredAt: formatDateTime(event.occurred_at),
    age: formatAge(event.occurred_at),
    provenance: event.provenance,
    aggregate: event.aggregate,
    payloadText: renderPayload(event.payload),
    transition: workflowTransition(event.payload),
  })),
);
</script>

<template>
  <ResourceState
    :loading="resource.loading.value"
    :error="resource.error.value"
    :empty="views.length === 0"
    empty-text="no events recorded yet"
  />
  <div v-if="!resource.loading.value && !resource.error.value" class="eventlist">
    <div v-for="event in views" :key="event.id" class="event">
      <span class="event__when" :title="event.occurredAt">{{ event.age }}</span>
      <div class="event__main">
        <div class="event__meta">
          <span class="pill pill--outline">{{ event.provenance }}</span>
          <span v-if="event.transition" class="event__type">
            {{ event.transition.from ?? "?" }} → {{ event.transition.to ?? "?" }}<template v-if="event.transition.trigger"> ({{ event.transition.trigger }})</template>
          </span>
          <span v-else class="event__type">{{ event.aggregate }}</span>
        </div>
        <pre class="event__payload">{{ event.payloadText }}</pre>
      </div>
    </div>
  </div>
</template>
