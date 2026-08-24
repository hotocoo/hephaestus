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
```

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
