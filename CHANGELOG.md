# Changelog

All notable changes to Hephaestus are documented here.
Format follows Keep a Changelog; versioning is Semantic Versioning
until the 1.0 API contract freezes (its own ADR, to come).

## [Unreleased]

### Added

- Web dashboard (ADR-012): a Vue 3 app under apps/web rendering
  server state and forwarding human decisions - overview, task list
  and idempotent intake form, plan rendering with per-step
  verification hooks, workflow-state timeline, approval/merge gate
  panels, and provenance-tagged event history rendered strictly as
  data. It mounts only with deployed credentials (fail-closed setup
  screen otherwise), polls live views on a configured interval, and is
  covered by component tests against an in-memory client double plus a
  live-server suite driving real HTTP. The API grows one operation for
  it - GET /api/v1/tasks/{task_id}/run - mirrored through the
  contracts inventory and the regenerated SDK types like every other.
- Web contract surface (ADR-011): the API serves its OpenAPI 3.1
  document at /api/v1/openapi.json, built in Rust and pinned by tests
  to the exact operation inventory; the TypeScript workspace
  (pnpm) lands with @hephaestus/contracts - strict zod mirrors of
  every wire type validated against a live hephaestus-server process -
  and @hephaestus/sdk - a typed client generated from the served
  document whose committed declarations are checked byte-for-byte by
  a drift test. CI's web job now provisions PostgreSQL, builds the
  server binary, and runs these conformance suites.
- Core domain model: typed UUIDv7 identifiers, structured error
  taxonomy with stable public codes, explicit workflow state machine
  (property-tested), versioned event envelopes with provenance.