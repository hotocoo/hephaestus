import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// In dev the dashboard proxies every control-plane path to a locally
// running hephaestus-server, so the browser talks same-origin and the
// runtime config can stay empty of hosts.
export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:7300",
      "/healthz": "http://127.0.0.1:7300",
      "/readyz": "http://127.0.0.1:7300",
    },
  },
});
