<script setup lang="ts">
/**
 * Honest loading / failure / empty rendering for one resource.
 *
 * Failures show the API's stable public code when there is one; the
 * message comes from the server or the transport, never invented here.
 */
import type { ResourceError } from "@/composables/useAsyncResource";
import { PhWarningCircle } from "@phosphor-icons/vue";

defineProps<{
  loading: boolean;
  error: ResourceError | null;
  empty: boolean;
  /** Shown instead of the generic empty sentence. */
  emptyText?: string;
}>();
</script>

<template>
  <div v-if="loading" class="loading-row" role="status">
    <span class="spinner" aria-hidden="true"></span>
    <span>loading…</span>
  </div>
  <div v-else-if="error" class="notice notice--error" role="alert">
    <PhWarningCircle :size="16" aria-hidden="true" />
    <span>
      request failed<template v-if="error.code"> ({{ error.code }})</template>: {{ error.message }}
    </span>
  </div>
  <div v-else-if="empty" class="notice notice--empty">
    {{ emptyText ?? "nothing here yet" }}
  </div>
</template>
