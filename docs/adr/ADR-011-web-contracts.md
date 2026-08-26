# ADR-011: Web Contract Surface - Served OpenAPI, Zod Mirrors, Typed SDK

## Status

Accepted

## Context

ADR-002 fixed the split: a Rust core owns every enforcement point,
a TypeScript tier renders state and forwards commands. The HTTP API
(ADR-009) and the worker runtime (ADR-010) made the control plane
runnable end to end; what the web tier must consume now exists. The
same ADR-002 also made two promises this phase has to keep:

* shared wire contracts are defined once in Rust types and mirrored
  by zod schemas validated against the live API by conformance tests;
* the SDK is generated from the same OpenAPI document the API
  serves, and conformance tests fail CI if drift appears.

Nothing consumed those promises yet, so nothing enforced them.
Meanwhile every new endpoint risks three silent divergences:
document vs. router, schema mirror vs. server bytes, client vs.
document.

## Decision

### One document, served, not stored

hephaestus-api builds its OpenAPI 3.1 document in Rust
(openapi::openapi_document) and serves it unauthenticated at
/api/v1/openapi.json. There is no committed spec file to fall out
of sync: a test renders the served document and compares it against
the builder byte for byte. The canonical operation inventory lives
next to the builder as OPERATIONS, and unit tests pin the document
to exactly that set - an undocumented route or a stale entry fails
loudly.

### Mirrors that parse real bytes

@hephaestus/contracts mirrors every wire type as zod schemas.
Response mirrors are strict: unknown fields fail parsing, so a
server that grows or renames fields fails conformance instead of
silently rendering stale data. Request mirrors stay open because
the server itself ignores unknown request fields. A live
conformance suite spawns the real hephaestus-server binary against
real PostgreSQL (the harness lives in contracts/testing), drives
intake through idempotent resubmission, and requires every
response - success and failure - to parse against the mirrors.

### A generated SDK with a drift tripwire

@hephaestus/sdk keeps one generated file, src/openapi.d.ts,
produced by openapi-typescript from the served document
(pnpm --filter @hephaestus/sdk generate). The typed client is a
thin fetch wrapper over those types: paths, bearer credentials and
idempotency headers only, no policy, no caching, no authority. Its
drift test regenerates the declarations from a live server and
requires byte equality with the committed file - CI fails whenever
the generation would change, which is exactly the ADR-002 sentence,
implemented.

### CI runs conformance, not just tsc

The web job now provisions PostgreSQL, builds the server binary,
and runs the suites against it. A web-tier green build therefore
means: types compile, lint passes, live responses match the zod
mirrors, and the generated SDK matches the served document.

## Consequences

* Drift fails CI from three directions simultaneously; none of the
  three layers can quietly become stale.
* The dashboard (next phase) inherits a verified client and needs
  no hand-written request plumbing.
* The web toolchain (Node 22+, pnpm 11) becomes a first-class part
  of the repository's test story, including its PostgreSQL service.
* The document is deliberately hand-maintained Rust: contract
  changes are reviewable diffs, not annotation side effects. The
  cost is paid once per change and enforced by tests forever after.
