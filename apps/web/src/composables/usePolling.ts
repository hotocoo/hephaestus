/**
 * Timer-based refresh for live views.
 *
 * Runs poll while their view is mounted; the interval comes from
 * runtime configuration so an operator can slow the control plane's
 * background chatter down without touching code.
 *
 * @module
 */

import { onScopeDispose } from "vue";

/** Call callback every seconds until the current scope disposes. */
export function usePolling(callback: () => void, seconds: number): void {
  if (!(seconds >= 1)) return; // configuration validated at boot; defense in depth
  const timer = setInterval(callback, seconds * 1000);
  onScopeDispose(() => clearInterval(timer));
}
