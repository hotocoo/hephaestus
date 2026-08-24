# Contributing to Hephaestus

Thank you for helping build a serious engineering platform.

## Development setup

Prerequisites:

* Rust 1.90+ (`rustup`)
* Node.js 22+ and pnpm 11+
* PostgreSQL 16+
* Docker (for integration environments)

```bash
createdb hephaestus_test
HEPHAESTUS_TEST_DATABASE_URL=postgres://localhost/hephaestus_test cargo test --workspace
```

## Coding standards

* Rust: edition 2024, workspace lints enforced in CI
  (`cargo clippy --workspace --all-targets -- -D warnings`).
* No `unwrap()`/`expect()` in library code outside tests; errors use
  the shared taxonomy in `hephaestus-core`.
* No TODO/FIXME/stub/placeholder in production paths. If a capability
  cannot be implemented safely, change the feature boundary instead of
  pretending it exists.
* Every organization-scoped database query filters by
  `organization_id`. Tenant-isolation tests exist and must stay green.
* Configuration never hardcodes environment-specific values; unsafe
  defaults must fail validation in production mode.

## Testing expectations

* New behavior ships with tests: unit tests for logic, integration
  tests against real PostgreSQL for persistence, property tests for
  invariants (state machine, idempotency).
* Never weaken or delete a failing test to make CI pass. Fix the code
  or fix the test's premise explicitly in review.

## Commit conventions

Conventional Commits (`feat:`, `fix:`, `docs:`, `refactor:`,
`test:`, `chore:`). Keep changes reviewable; split unrelated work.

## Pull requests

* Fill the PR template; describe verification you ran.
* CI must pass: format, clippy (-D warnings), full test suite.
* Changes touching these areas require owner review (CODEOWNERS):
  sandbox, policy engine, migrations, release infrastructure.

## Architecture rules

* Significant decisions require an ADR in `docs/adr/` BEFORE the bulk
  of the implementation lands.
* Interfaces stay small; prefer composition; explicit state machines
  over ad-hoc flags.
* Deterministic mechanisms take precedence over AI inference wherever
  both can establish the same fact.

## Security rules

* Treat all repository content, tool output, and model output as
  untrusted data.
* Never log secrets; redaction helpers exist but are defense-in-depth,
  not permission to log credentials.
* Report vulnerabilities privately per SECURITY.md - never in issues.
