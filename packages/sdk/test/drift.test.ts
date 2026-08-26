/**
 * Drift check: regenerate the client types from the document a real
 * server serves and require byte equality with the committed file.
 * This is the ADR-002 guarantee - the SDK is generated from the same
 * OpenAPI document the API serves, and CI fails if drift appears.
 */
import { beforeAll, afterAll, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startLiveServer, type LiveServer } from "@hephaestus/contracts/testing";
import { typesFromUrl } from "../scripts/generate.mjs";

let server: LiveServer;

beforeAll(async () => {
  server = await startLiveServer();
});

afterAll(async () => {
  await server.stop();
});

it("committed openapi.d.ts equals a fresh generation", async () => {
  const fresh = await typesFromUrl(server.baseUrl + "/api/v1/openapi.json");
  const committedPath = path.join(
    path.dirname(fileURLToPath(import.meta.url)),
    "..",
    "src",
    "openapi.d.ts",
  );
  const committed = readFileSync(committedPath, "utf8");
  const normalize = (s: string) => s.replace(/\r\n/g, "\n").trimEnd() + "\n";
  try {
    expect(normalize(fresh)).toBe(normalize(committed));
  } catch (error) {
    throw new Error(
      "openapi.d.ts drifted from the served contract. " +
        "Run: pnpm --filter @hephaestus/sdk generate\n" +
        String(error),
    );
  }
});
