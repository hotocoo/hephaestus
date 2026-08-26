# Hephaestus

**An AI-native software engineering control plane.**

Hephaestus takes a software-engineering task and coordinates repository
understanding, planning, implementation, testing, verification, review,
artifact generation, deployment verification, observability, and
long-term maintenance — while keeping deterministic engineering systems
authoritative wherever deterministic verification is possible.

> **AI proposes and executes engineering work; deterministic systems verify it.**

Named for the Greek god of the forge.

## Status

Hephaestus is under active development. This repository follows a
phase plan ([docs/adr](docs/adr)); every phase lands only when its
features are **implemented, tested, and documented**. We do not ship
placeholders, mock data in production paths, or simulated integrations.

Currently working end-to-end:

* Typed domain model with an explicit workflow state machine
  (property-tested transitions; illegal states unrepresentable)
* Versioned event model with provenance classification for
  prompt-injection defense
* PostgreSQL persistence layer: versioned migrations, tenant-scoped
  stores, idempotent writes enforced by unique indexes
* Durable job queue: priority claims (`FOR UPDATE SKIP LOCKED`),
  exponential-backoff retries, dead-lettering, scoped expired-lease
  recovery
* Tamper-evident audit log (SHA-256 hash chaining with verifier)
* Layered validated configuration (defaults -> TOML file ->
  environment) that fails closed, including production-only checks
* Secret redaction and untrusted-content framing primitives
* Centralized tool runtime: deny-by-default capabilities, workspace
  containment, command allowlists, sandboxed shell, audited invocation
* Layered deterministic verification executed through the tool runtime
* Agent runtime: role manifests with policy-enforced separation of
  duties (reviewers read-only by invariant), OpenAI-compatible model
  providers with bounded retries, and a governed session loop where
  model output can select tools but never widen permissions
* Planning pipeline: repository snapshot + deterministic inventory
  analysis, requirement extraction and plan generation driven by
  governed planner sessions (strict JSON documents, parsed
  deterministically, retried within queue budgets when malformed),
  transactional requirements/plan persistence, and a human approval
  gate that parks the run at `awaiting_approval`
* Execution pipeline: idempotent approval decisions that bootstrap
  executions over snapshot-isolated plan steps, governed implementer
  sessions with strict JSON outcome documents, deterministic
  verification through the governed tool runtime with append-only
  evidence, bounded fix/review loops owned by the execution layer
  (ADR-007), and a read-only automated review gate that parks
  finished work at `awaiting_merge`
* Delivery pipeline (ADR-008): externally made merge decisions
  recorded on a durable approval gate with full crash-replay
  recovery, deterministic builds of the frozen change set through the
  governed tool runtime with SHA-256 artifact evidence, runs whose
  plans demand no deployment completing via `skip_deployment`, and
  demanded-but-impossible deployments failing loudly instead of being
  simulated
* HTTP API layer (ADR-009): an axum-based control-plane surface in
  `hephaestus-api` served by the `hephaestus-server` binary - task
  intake with idempotency keys, tenant-scoped reads over tasks, runs,
  plans, gates, events and the project/repository catalog, and human
  plan-approval / merge decisions routed through the same services the
  workers use. Requests authenticate via pre-provisioned bearer API
  keys bound to one organization each (fail-closed configuration,
  production demands at least one key), errors map onto the core
  taxonomy with stable public codes, and every endpoint scopes its
  store access by the authenticated principal's tenant
* Worker runtime (ADR-010): a `hephaestus-worker` binary that
  assembles the full pipeline registry from layered configuration and
  runs the durable job loop with graceful drain - analysis, planning,
  governed implementation and repair, deterministic verification,
  automated review, artifact builds. Model credentials live only in
  workers that serve model-backed queues; queue sets are validated
  fail-closed at load, and the unservable `deployment` queue is
  refused outright until a real executor exists
* Web contract surface (ADR-011): the API serves its own OpenAPI 3.1
  document at `/api/v1/openapi.json`; the TypeScript workspace mirrors
  it twice and proves both mirrors against the live server -
  `@hephaestus/contracts` (strict zod schemas over real responses) and
  `@hephaestus/sdk` (a typed client whose declarations are regenerated
  from the served document in CI, byte-compared to catch drift)

## Architecture (in progress)

```
                 +---------------------+
                 |       Web UI        |
                 +----------+----------+
                            |
                 +----------v----------+
                 |      API layer      |
                 +----------+----------+
                            |
     +----------------------+----------------------+
     |                      |                      |
     v                      v                      v
 Workflow engine      Repository intel       Policy engine
     |                      |                      |
     v                      v                      v
 Scheduler/queue      Code intelligence      Authorization
     |                      |                      |
     +----------+-----------+----------+-----------+
                |                      |
                v                      v
          Agent runtime           Verification
                |                      |
                v                      v
          Model providers         Evidence graph
                |                      |
                +----------+-----------+
                           v
                     Artifact system
                           |
                      CI/CD / deploy
```

Design decisions are recorded as [ADRs](docs/adr/ADR-001-architecture.md).

## Development quick start

Prerequisites: Rust 1.90+, Node 22+, pnpm 11+, PostgreSQL 16+.

```bash
git clone <repository-url> && cd hephaestus
createdb hephaestus_test

# Run the full Rust test suite (unit + property + database integration)
HEPHAESTUS_TEST_DATABASE_URL=postgres://localhost/hephaestus_test cargo test --workspace

# Run the control plane: one API server (owns migrations) plus workers.
cargo run -p hephaestus-api --bin hephaestus-server &
HEPHAESTUS_DATABASE_URL=postgres://localhost/hephaestus_dev \
HEPHAESTUS_STORAGE_ROOT=.hephaestus/data \
cargo run -p hephaestus-worker --bin hephaestus-worker
```

Workers serve only deterministic queues by default; serving planning,
implementation or review additionally requires `[model]` provider
settings (see [`.env.example`](.env.example)).

### Web tier

The TypeScript packages live under `packages/` (pnpm 11, Node 22+).
Their tests are conformance suites: they spawn the real server binary,
so build it first and point them at a test database.

```bash
cargo build -p hephaestus-api --bin hephaestus-server
HEPHAESTUS_TEST_DATABASE_URL=postgres://localhost/hephaestus_test \
  pnpm install && pnpm -r typecheck && pnpm -r lint && pnpm -r test

# Regenerate the SDK's types from the served OpenAPI document:
HEPHAESTUS_TEST_DATABASE_URL=postgres://localhost/hephaestus_test \
  pnpm --filter @hephaestus/sdk generate

# Run the dashboard against a local control plane (Vite proxies /api):
HEPHAESTUS_TEST_DATABASE_URL=postgres://localhost/hephaestus_test \
  pnpm --filter @hephaestus/web test
pnpm --filter @hephaestus/web dev

# The dashboard reads credentials at runtime from window.__HEPHAESTUS_WEB_CONFIG__
# (see apps/web/index.html); without a token it renders its setup screen.
```

A drift test fails CI whenever the committed `openapi.d.ts` differs
from what the serving API would generate (ADR-002, ADR-011).

Configuration precedence: built-in defaults -> TOML file -> environment
(`HEPHAESTUS_` prefix). See [`.env.example`](.env.example).
Invalid configuration refuses to start the process; production mode
additionally rejects unsafe settings (auth disabled, sandbox network
egress enabled, localhost database).

## Security

See [SECURITY.md](SECURITY.md) for reporting policy. Core security
posture:

* Untrusted content (repositories, issues, tool output, model output)
  is data, never instructions; provenance tagging plus runtime
  enforcement outside the model.
* Agents receive declared capabilities only, checked by the runtime;
  default deny.
* Secrets are never logged or persisted in plaintext; redaction
  defense-in-depth is built into shared primitives.

## License

Apache-2.0. See [LICENSE](LICENSE).