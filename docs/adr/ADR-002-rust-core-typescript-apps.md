# ADR-002: Rust Core + TypeScript Applications

## Status
Accepted

## Context
The platform needs memory-safe systems programming for isolation,
parsing untrusted content, and durable state machines, plus a
productive UI stack for humans.

## Decision
Rust 2024 edition workspace under `crates/`; pnpm-managed TypeScript
under `apps/web` and `packages/`. Shared wire contracts are defined
once in Rust types and mirrored by zod schemas validated against the
live API by conformance tests.

## Consequences
* No ORM drift: SQL migrations are plain versioned .sql files applied by
  hephaestus-db; schema truth lives in the repository.
* The SDK is generated from the same OpenAPI document the API serves;
  conformance tests fail CI if drift appears.
