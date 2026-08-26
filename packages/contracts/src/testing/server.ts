/**
 * Live-server test harness.
 *
 * Spawns the real hephaestus-server binary against the test
 * PostgreSQL database, seeds one tenant, and hands out everything a
 * conformance test needs to drive authenticated HTTP. This is the
 * same discipline the Rust integration tests follow: no mocks in
 * production paths, loud failure when prerequisites are missing.
 *
 * Prerequisites:
 * - HEPHAESTUS_TEST_DATABASE_URL set to a PostgreSQL database the
 *   server may migrate and seed.
 * - A built server binary: cargo build -p hephaestus-api --bin
 *   hephaestus-server, or HEPHAESTUS_SERVER_BIN pointing at one.
 *
 * @module
 */

import { randomUUID } from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import net from "node:net";
import pg from "pg";

export interface TenantSeed {
  organizationId: string;
  projectId: string;
  repositoryId: string;
}

export interface LiveServer {
  /** Base URL without trailing slash, e.g. http://127.0.0.1:41234 */
  readonly baseUrl: string;
  readonly token: string;
  readonly tenant: TenantSeed;
  /** Terminate the process and clean up all temporary state. */
  stop(): Promise<void>;
}

/** Walk up from this module to the directory containing Cargo.toml. */
function repoRoot(): string {
  let dir = path.dirname(fileURLToPath(import.meta.url));
  for (let i = 0; i < 10; i += 1) {
    if (existsSync(path.join(dir, "Cargo.toml"))) return dir;
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  throw new Error(
    "repository root not found from contracts/testing/server.ts",
  );
}

/** Resolve the server binary or fail with operator guidance. */
function resolveServerBinary(root: string): string {
  const override = process.env.HEPHAESTUS_SERVER_BIN;
  const candidate =
    override ?? path.join(root, "target", "debug", "hephaestus-server");
  if (!existsSync(candidate)) {
    throw new Error(
      "hephaestus-server binary not found at " +
        candidate +
        ". Build it first: cargo build -p hephaestus-api --bin hephaestus-server " +
        "(or point HEPHAESTUS_SERVER_BIN at an existing binary)",
    );
  }
  return candidate;
}

/** Require the shared test database URL, like every Rust test. */
function requireDatabaseUrl(): string {
  const url = process.env.HEPHAESTUS_TEST_DATABASE_URL;
  if (!url) {
    throw new Error(
      "HEPHAESTUS_TEST_DATABASE_URL must be set (e.g. postgres://localhost/hephaestus_test)",
    );
  }
  return url;
}

/** Grab an ephemeral loopback port by binding and releasing it. */
function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (address === null || typeof address === "string") {
        server.close(() => reject(new Error("no ephemeral port")));
        return;
      }
      const port = address.port;
      server.close(() => resolve(port));
    });
  });
}

/** Ring-buffer the child's stderr so failures explain themselves. */
class StderrTail {
  private readonly lines: string[] = [];

  push(chunk: Buffer | string): void {
    for (const line of chunk.toString().split("\n")) {
      if (line.trim().length === 0) continue;
      this.lines.push(line);
      if (this.lines.length > 40) this.lines.shift();
    }
  }

  render(): string {
    return this.lines.join("\n");
  }
}

/** Slug fragments satisfy the schema's slug CHECK constraints. */
function slug(prefix: string): string {
  return prefix + "-" + randomUUID().slice(0, 8);
}

/**
 * Start the server and seed one tenant. The binary applies its own
 * migrations before binding (ADR-009), so once the probe responds the
 * schema is current and seeding can proceed directly.
 */
export async function startLiveServer(): Promise<LiveServer> {
  const root = repoRoot();
  const binary = resolveServerBinary(root);
  const databaseUrl = requireDatabaseUrl();
  const port = await freePort();

  const tenant: TenantSeed = {
    organizationId: randomUUID(),
    projectId: randomUUID(),
    repositoryId: randomUUID(),
  };
  const token = "conformance-" + randomUUID().replaceAll("-", "").slice(0, 24);

  const configDir = await mkdtemp(path.join(tmpdir(), "hephaestus-contract-"));
  const configPath = path.join(configDir, 'config.toml');
  const toml = [
    'environment = "test"',
    '',
    '[server]',
    'bind_addr = "127.0.0.1"',
    'port = ' + String(port),
    '',
    '[database]',
    'url = ' + JSON.stringify(databaseUrl),
    '',
    '[auth]',
    'disabled = false',
    '',
    '[[auth.keys]]',
    'token = ' + JSON.stringify(token),
    'organization_id = ' + JSON.stringify(tenant.organizationId),
    'principal = "conformance"',
    '',
  ].join('\n');
  await writeFile(configPath, toml, 'utf8');

  const stderr = new StderrTail();
  const child: ChildProcess = spawn(binary, ['--config', configPath], {
    stdio: ["ignore", "ignore", "pipe"],
    env: { ...process.env, HEPHAESTUS_CONFIG: configPath },
  });
  child.stderr?.on("data", (chunk: Buffer | string) => stderr.push(chunk));

  const exitedEarly = new Promise<never>((_, reject) => {
    child.once("exit", (code, signal) => {
      reject(
        new Error(
          "hephaestus-server exited before serving (code=" +
            String(code) +
            " signal=" +
            String(signal) +
            ")\n" +
            stderr.render(),
        ),
      );
    });
  });

  const baseUrl = 'http://127.0.0.1:' + String(port);
  try {
    // The server migrates before binding; poll until then.
    const deadline = Date.now() + 60_000;
    for (;;) {
      if (Date.now() > deadline) {
        throw new Error(
          "hephaestus-server did not become healthy in time\n" +
            stderr.render(),
        );
      }
      const ready = await Promise.race([
        fetch(baseUrl + '/healthz')
          .then((r) => r.ok)
          .catch(() => false),
        exitedEarly,
      ]);
      if (ready) break;
      await new Promise((resolve) => setTimeout(resolve, 150));
    }

    const pool = new pg.Pool({ connectionString: databaseUrl, max: 2 });
    try {
      await pool.query(
        "INSERT INTO organizations (id, name, slug) VALUES ($1, $2, $3)",
        [tenant.organizationId, "Conformance T", slug("conformance-org")],
      );
      await pool.query(
        "INSERT INTO projects (id, organization_id, name, slug) VALUES ($1, $2, $3, $4)",
        [
          tenant.projectId,
          tenant.organizationId,
          "Conformance P",
          slug("conformance-proj"),
        ],
      );
      await pool.query(
        [
          "INSERT INTO repositories",
          "  (id, organization_id, project_id, remote_url, default_branch, display_name)",
          "VALUES ($1, $2, $3, $4, $5, $6)",
        ].join(' '),
        [
          tenant.repositoryId,
          tenant.organizationId,
          tenant.projectId,
          "https://example.invalid/conformance.git",
          "main",
          "Conformance Repository",
        ],
      );
    } finally {
      await pool.end();
    }
  } catch (error) {
    child.kill("SIGKILL");
    await rm(configDir, { recursive: true, force: true }).catch(() => {});
    throw error;
  }

  let stopped = false;
  return {
    baseUrl,
    token,
    tenant,
    async stop(): Promise<void> {
      if (stopped) return;
      stopped = true;
      child.kill("SIGTERM");
      const grace = new Promise<void>((resolve) => {
        child.once("exit", () => resolve());
        setTimeout(() => {
          if (child.exitCode === null && child.signalCode === null) {
            child.kill("SIGKILL");
          }
          resolve();
        }, 3_000);
      });
      await grace;
      await rm(configDir, { recursive: true, force: true }).catch(() => {});
    },
  };
}
