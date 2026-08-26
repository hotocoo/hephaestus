import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    // The drift check spawns a real server; give it room.
    testTimeout: 120_000,
    hookTimeout: 120_000,
  },
});
