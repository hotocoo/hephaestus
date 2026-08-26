<script setup lang="ts">
/**
 * Dashboard shell.
 *
 * Owns the two pieces of shared state - the control-plane client and
 * the organization catalog - plus the health indicator. Everything
 * below this component renders server state and forwards commands;
 * nothing here holds authority.
 */
import { computed } from "vue";
import { RouterLink, RouterView, useRoute } from "vue-router";
import { PhCaretRight, PhFlame } from "@phosphor-icons/vue";
import { provideControlPlane } from "@/api/client";
import type { ControlPlane } from "@/api/client";
import { useAsyncResource } from "@/composables/useAsyncResource";
import { provideCatalog } from "@/composables/useCatalog";
import { usePolling } from "@/composables/usePolling";
import { loadConfig } from "@/config";

const props = defineProps<{ client: ControlPlane }>();

provideControlPlane(props.client);
provideCatalog(props.client);

const config = loadConfig();
const health = useAsyncResource(() => props.client.getHealthz());
usePolling(health.reload, config.pollSeconds);

const healthClass = computed(() => {
  if (health.error.value !== null) return "health--down";
  return health.data.value !== null ? "health--ok" : "";
});

const route = useRoute();
const crumbs = computed<string>(() => {
  switch (route.name) {
    case "tasks":
      return "Tasks";
    case "task-detail":
      return "Tasks / Task";
    case "run-detail":
      return "Runs / Run";
    default:
      return "Overview";
  }
});
</script>

<template>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand">
        <span class="brand__mark">
          <PhFlame :size="22" weight="fill" aria-hidden="true" />
        </span>
        <span class="brand__name">
          Hephaestus
          <span class="brand__tag">control plane</span>
        </span>
      </div>
      <nav class="nav" aria-label="primary">
        <RouterLink to="/" exact-active-class="router-link-active">
          <PhCaretRight :size="14" aria-hidden="true" />
          <span>Overview</span>
        </RouterLink>
        <RouterLink to="/tasks">
          <PhCaretRight :size="14" aria-hidden="true" />
          <span>Tasks</span>
        </RouterLink>
      </nav>
      <div class="sidebar__foot">
        AI proposes;<br />deterministic systems verify.
      </div>
    </aside>

    <div class="shell__main">
      <header class="topbar">
        <span class="topbar__crumbs"><b>{{ crumbs }}</b></span>
        <span class="health" :class="healthClass" role="status" aria-label="api health">
          <span class="health__dot" aria-hidden="true"></span>
          {{ health.error.value !== null ? "api unreachable" : "api healthy" }}
        </span>
      </header>
      <main class="shell__content">
        <RouterView />
      </main>
    </div>
  </div>
</template>
