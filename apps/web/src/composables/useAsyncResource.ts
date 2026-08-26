/**
 * One asynchronously loaded piece of server state.
 *
 * The dashboard renders three honest states per resource - loading,
 * failed (with the API's stable code) and data - and never fabricates
 * content for the first two. Reload is always explicit in the UI;
 * polling views simply call it on a timer.
 *
 * @module
 */

import { ref, shallowRef, watch, onScopeDispose } from "vue";
import type { Ref, WatchSource } from "vue";
import { ApiError } from "@hephaestus/sdk";

export interface AsyncResource<T> {
  /** Loaded value; null until the first success. */
  readonly data: Readonly<Ref<T | null>>;
  /** Human-renderable failure; stable public code when the API supplied one. */
  readonly error: Readonly<Ref<ResourceError | null>>;
  readonly loading: Readonly<Ref<boolean>>;
  /** Fetch again; safe to call concurrently, last completion wins. */
  reload(): Promise<void>;
}

/** A failure rendered honestly: what happened, at which HTTP status. */
export interface ResourceError {
  status: number | null;
  code: string | null;
  message: string;
}

export function toResourceError(thrown: unknown): ResourceError {
  if (thrown instanceof ApiError) {
    return { status: thrown.status, code: thrown.code ?? null, message: thrown.message };
  }
  return {
    status: null,
    code: null,
    message: thrown instanceof Error ? thrown.message : String(thrown),
  };
}

/**
 * Load a resource immediately (and whenever a watched source changes).
 * The loader receives no arguments; capture reactive values yourself.
 */
export function useAsyncResource<T>(
  loader: () => Promise<T>,
  options: { watch?: WatchSource | WatchSource[] } = {},
): AsyncResource<T> {
  const data = shallowRef<T | null>(null);
  const error = ref<ResourceError | null>(null);
  const loading = ref(false);
  let generation = 0;

  async function reload(): Promise<void> {
    const current = ++generation;
    loading.value = true;
    try {
      const value = await loader();
      if (current !== generation) return; // superseded by a newer load
      data.value = value;
      error.value = null;
    } catch (thrown) {
      if (current !== generation) return;
      data.value = null;
      error.value = toResourceError(thrown);
    } finally {
      if (current === generation) loading.value = false;
    }
  }

  if (options.watch === undefined) {
    void reload();
  } else {
    watch(options.watch, () => void reload(), { immediate: true });
  }

  // Polling timers registered through this composable stop with it.
  onScopeDispose(() => {
    generation += 1;
  });

  return { data, error, loading, reload };
}
