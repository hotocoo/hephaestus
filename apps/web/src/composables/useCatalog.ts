/**
 * Organization catalog (projects, repositories) as shared app state.
 *
 * Both catalogs are small and read-only for this tier; one fetch per
 * app session serves every view's id-to-name rendering. Provided once
 * in App.vue so tests can install doubles alongside the client double.
 *
 * @module
 */

import { computed, inject, provide, ref, shallowRef } from "vue";
import type { ComputedRef, InjectionKey, Ref } from "vue";
import type { Project, Repository } from "@hephaestus/sdk";
import type { ControlPlane } from "@/api/client";
import type { ResourceError } from "./useAsyncResource";

export interface CatalogStore {
  readonly projectName: Readonly<Ref<Map<string, string>>>;
  readonly repositoryName: Readonly<Ref<Map<string, string>>>;
  readonly projectCount: Readonly<ComputedRef<number>>;
  readonly repositoryCount: Readonly<ComputedRef<number>>;
  readonly error: Readonly<Ref<ResourceError | null>>;
  /** Reload both lists; safe to call repeatedly. */
  reload(): Promise<void>;
}

interface CatalogInternal extends CatalogStore {
  readonly ready: Ref<boolean>;
}

const CATALOG_KEY: InjectionKey<CatalogInternal> = Symbol("catalog");

/** Install the shared catalog for the whole tree; loads immediately. */
export function provideCatalog(client: ControlPlane): CatalogStore {
  const projects = shallowRef<Project[]>([]);
  const repositories = shallowRef<Repository[]>([]);
  const error = ref<ResourceError | null>(null);
  const ready = ref(false);

  const projectName = computed(
    () => new Map(projects.value.map((p) => [p.id, p.name] as const)),
  );
  const repositoryName = computed(
    () => new Map(repositories.value.map((r) => [r.id, r.display_name] as const)),
  );

  async function reload(): Promise<void> {
    try {
      const [projectRows, repositoryRows] = await Promise.all([
        client.listProjects({ limit: 200 }),
        client.listRepositories({ limit: 200 }),
      ]);
      projects.value = projectRows;
      repositories.value = repositoryRows;
      error.value = null;
    } catch (thrown) {
      // Callers render the store's error surface; a catalog failure
      // must not take the whole shell down.
      error.value = {
        status: null,
        code: "CATALOG_UNAVAILABLE",
        message: thrown instanceof Error ? thrown.message : String(thrown),
      };
    } finally {
      ready.value = true;
    }
  }

  const store: CatalogInternal = {
    projectName,
    repositoryName,
    projectCount: computed(() => projects.value.length),
    repositoryCount: computed(() => repositories.value.length),
    error,
    ready,
    reload,
  };

  void reload();
  provide(CATALOG_KEY, store);
  return store;
}

/** The shared catalog installed by App.vue. */
export function useCatalog(): CatalogStore {
  const store = inject(CATALOG_KEY);
  if (store === undefined) {
    throw new Error("no catalog provided - call provideCatalog() in the app shell");
  }
  return store;
}
