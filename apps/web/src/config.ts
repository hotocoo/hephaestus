/**
 * Runtime configuration, read once at boot.
 *
 * The web tier holds no authority: it forwards credentials it was
 * deployed with to the API and nothing else. Configuration is
 * deliberately fail-closed - without a bearer token bound to one
 * organization the app renders its setup screen instead of firing
 * doomed requests, and there is no interactive way around that.
 *
 * @module
 */

/** Shape injected through index.html as window.__HEPHAESTUS_WEB_CONFIG__. */
export interface WebConfig {
  /**
   * Control-plane base URL without trailing slash. Empty means
   * same-origin: a reverse proxy or the dev server fronts the API.
   */
  baseUrl: string;
  /** Pre-provisioned API key; deployments inject their own. */
  token: string;
  /** Poll interval for live views, seconds. Defaults to 10. */
  pollSeconds: number;
}

declare global {
  var __HEPHAESTUS_WEB_CONFIG__: Partial<WebConfig> | undefined;
}

function readPositiveInt(value: unknown, fallback: number): number {
  if (typeof value === "number" && Number.isFinite(value) && value >= 1) {
    return Math.floor(value);
  }
  return fallback;
}

/** Load and validate configuration exactly once per page load. */
export function loadConfig(): WebConfig {
  const raw = globalThis.__HEPHAESTUS_WEB_CONFIG__ ?? {};
  const baseUrl = typeof raw.baseUrl === "string" ? raw.baseUrl.replace(/\/+$/, "") : "";
  return {
    baseUrl,
    token: typeof raw.token === "string" ? raw.token : "",
    pollSeconds: readPositiveInt(raw.pollSeconds, 10),
  };
}

/** Whether the dashboard may talk to the control plane at all. */
export function isConfigured(config: WebConfig): boolean {
  return config.token.length > 0;
}
