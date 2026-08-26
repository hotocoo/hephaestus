/**
 * Dashboard routes.
 *
 * Web-history routing with same-origin URLs; the dev server proxies
 * API paths away so the router never collides with them. Unknown
 * paths land on the overview rather than a dead end.
 *
 * @module
 */

import { createRouter, createWebHistory } from "vue-router";

export function createAppRouter() {
  return createRouter({
    history: createWebHistory(),
    routes: [
      { path: "/", name: "overview", component: () => import("./views/OverviewView.vue") },
      { path: "/tasks", name: "tasks", component: () => import("./views/TasksView.vue") },
      {
        path: "/tasks/:taskId",
        name: "task-detail",
        component: () => import("./views/TaskDetailView.vue"),
      },
      {
        path: "/runs/:runId",
        name: "run-detail",
        component: () => import("./views/RunDetailView.vue"),
      },
      { path: "/:pathMatch(.*)*", redirect: "/" },
    ],
  });
}
