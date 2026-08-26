/**
 * Dashboard entry point.
 *
 * Configuration is read once and enforced before anything mounts:
 * without credentials the operator sees the setup screen and the app
 * fires no requests. With credentials the real typed client - the one
 * generated from the served OpenAPI document (ADR-011) - is provided
 * to the whole tree.
 *
 * @module
 */

import { createApp } from "vue";
import { HephaestusClient } from "@hephaestus/sdk";
import App from "./App.vue";
import SetupView from "./views/SetupView.vue";
import { createAppRouter } from "./router";
import { isConfigured, loadConfig } from "./config";
import "./app.css";

const config = loadConfig();

if (!isConfigured(config)) {
  createApp(SetupView).mount("#app");
} else {
  const client = new HephaestusClient({
    baseUrl: config.baseUrl,
    token: config.token,
  });
  const app = createApp(App, { client });
  app.use(createAppRouter());
  app.mount("#app");
}
