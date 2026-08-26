import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    // The live-server harness spawns a real binary; give it room.
    testTimeout: 120_000,
    hookTimeout: 120_000,
  },
});
