/**
 * Regenerate src/openapi.d.ts from the OpenAPI document a real
 * hephaestus-server serves.
 *
 * Usage: pnpm --filter @hephaestus/sdk generate
 * Requires the same prerequisites as the conformance suite:
 * HEPHAESTUS_TEST_DATABASE_URL plus a built server binary (or
 * HEPHAESTUS_SERVER_BIN). The committed openapi.d.ts is checked
 * against a fresh generation by test/drift.test.ts, so CI fails
 * whenever this script's output would change.
 */
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import net from "node:net";
import openapiTS, { astToString } from "openapi-typescript";

const here = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(here, '..');
const repoRoot = path.resolve(packageRoot, '..', '..');

/** Generate declaration contents from a served document. */
export async function typesFromUrl(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error('fetching document failed: HTTP ' + response.status);
  }
  const document = await response.json();
  const ast = await openapiTS(document);
  return astToString(ast);
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      if (!address || typeof address === 'string') {
        server.close(() => reject(new Error('no ephemeral port')));
        return;
      }
      const port = address.port;
      server.close(() => resolve(port));
    });
  });
}

function resolveBinary() {
  const override = process.env.HEPHAESTUS_SERVER_BIN;
  const candidate = override ?? path.join(repoRoot, 'target', 'debug', 'hephaestus-server');
  if (!existsSync(candidate)) {
    throw new Error(
      'hephaestus-server binary not found at ' + candidate +
        '. Build it first: cargo build -p hephaestus-api --bin hephaestus-server'
    );
  }
  return candidate;
}

async function withLiveServer(callback) {
  const databaseUrl = process.env.HEPHAESTUS_TEST_DATABASE_URL;
  if (!databaseUrl) {
    throw new Error('HEPHAESTUS_TEST_DATABASE_URL must be set');
  }
  const binary = resolveBinary();
  const port = await freePort();
  const configDir = await mkdtemp(path.join(tmpdir(), 'hephaestus-sdk-gen-'));
  const configPath = path.join(configDir, 'config.toml');
  await writeFile(configPath, [
    'environment = "test\"',
    '',
    '[server]',
    'bind_addr = "127.0.0.1\"',
    'port = ' + String(port),
    '',
    '[database]',
    'url = ' + JSON.stringify(databaseUrl),
    '',
  ].join('\n'), 'utf8');

  const child = spawn(binary, ['--config', configPath], { stdio: 'ignore' });
  const baseUrl = 'http://127.0.0.1:' + String(port);
  try {
    const deadline = Date.now() + 60_000;
    let healthy = false;
    while (Date.now() < deadline) {
      healthy = await fetch(baseUrl + '/healthz').then((r) => r.ok).catch(() => false);
      if (healthy) break;
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
    if (!healthy) throw new Error('server did not become healthy');
    return await callback(baseUrl);
  } finally {
    child.kill('SIGTERM');
    setTimeout(() => child.kill('SIGKILL'), 3_000).unref?.();
    await rm(configDir, { recursive: true, force: true }).catch(() => {});
  }
}

const invokedDirectly =
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(process.argv[1]).href;

if (invokedDirectly) {
  const output = await withLiveServer(async (baseUrl) =>
    typesFromUrl(baseUrl + '/api/v1/openapi.json'),
  );
  const target = path.join(packageRoot, 'src', 'openapi.d.ts');
  await writeFile(target, output, 'utf8');
  console.log('generated', path.relative(repoRoot, target),
    '(' + String(output.split('\n').length) + ' lines)');
}
