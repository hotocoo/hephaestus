# Changelog

All notable changes to Hephaestus are documented here.
Format follows Keep a Changelog; versioning is Semantic Versioning
until the 1.0 API contract freezes (its own ADR, to come).

## [Unreleased]

### Fixed

- Dashboard: a run parked in one non-terminal state for an unusually
  long stretch now renders an unmistakable stall hint - the workflow
  timeline's current step carries its compact transition time and
  relative age, and past a 30-minute dwell an amber notice states the
  exact since-time and suggests checking for a worker serving the next
  queue. Previously a task stuck in `analyzing` for two days looked
  exactly like healthy work in flight. The server stays the sole
  authority on run health; the hint only makes the recorded time
  impossible to miss (pinned by unit tests for the dwell heuristic).
- Dashboard: the run detail view was orphaned - nothing in the
  interface linked to `/runs/:runId`, so it was reachable only by
  typing the URL. The task detail page's workflow panel now links to
  the run that owns it (component test asserts the link).
- Dashboard: table rows on the overview and task list were clickable
  only by mouse (`@click` on the `<tr>`); the title is now a real
  link, so keyboard and assistive-tech users can open a task.
- Dashboard: horizontal overflow on phone-width viewports. The
  panel grid floored every column at 320px and the timeline's
  timestamp column could not shrink, so at 375px the whole page
  scrolled sideways; panels now stack full-width below 480px and the
  timestamp shrinks then wraps instead of forcing width (verified
  against the live deployment: no page exceeds the viewport).
- Dashboard: per-task run lookups swallowed every error as "no run";
  only a 404 means the run is not bootstrapped yet, and any other
  failure now surfaces instead of masquerading as absence.
- Dashboard: gate decisions gave no feedback while in flight beyond
  disabled buttons; the buttons now read "sending…"/"recording…".
- Dashboard: the overview's Tasks stat could read as a total; it is
  labeled as covering the most recent page.

### Added

- Queue routing: requirement-extraction jobs were chained onto the
  `analysis` queue instead of `planning`. Model-capable workers
  (the only ones holding an extraction handler) poll `planning` and
  never saw them, while analysis-only workers claimed them with no
  handler and retried forever - a run stalled in `analyzing`
  indefinitely. Found during live end-to-end verification of ADR-014;
  pinned by a regression test that serves exactly one queue and asserts
  where the chain lands.
- SDK default transport: storing bare `fetch` on the client detached
  it from its global receiver, so every dashboard request failed with
  "Illegal invocation" in real browsers while Node-based tests passed.
  The transport now wraps `globalThis.fetch`; found by driving the
  live deployment with a headless browser and pinned by a regression
  test that constructs the client without a fetch override.

### Added

- Production serving and telemetry (ADR-014): a shared
  `hephaestus-telemetry` path installs structured logging everywhere
  and, when `telemetry.otlp_endpoint` is configured, exports spans
  over OTLP/HTTP protobuf under the configured service name - proven
  by an integration test that ships a span to an in-process collector.
  Both binaries now drain on SIGTERM exactly as on Ctrl-C, so process
  managers get graceful restarts. An optional `[web]` configuration
  section serves the built dashboard from hephaestus-server itself:
  hashed assets cache immutably, unknown non-API paths fall back to
  the SPA entry whose bootstrap placeholder is replaced at startup
  with injected runtime credentials (a bundle without the placeholder
  fails startup loudly), and every /api response keeps its exact JSON
  contract. A bounded `server.request_timeout_secs` (default 30,
  range 1..=600) applies a request timeout across the router.

- Deployment pipeline (ADR-013): configured deployment targets
  (`[[deployment.targets]]` in TOML - named argv command plus mandatory
  post-deployment verification hooks and a bounded timeout), executed
  through the governed sandboxed shell under a new operator role whose
  allowlist derives strictly from the target's own commands (Deploy
  capability reserved to operators; secrets, network egress and source
  writes forbidden for them like everyone else). Plans select targets
  by exact name in `strategy.deployment`; unmatched names fail the run
  before any command runs. The build stage bootstraps a durable
  deployments row (one live row per run via partial unique index),
  advances building -> deploying and chains one job that drives deploy
  then verify as separate idempotent steps; failures finish the row as
  failed with a bounded reason and fail the run terminally, with typed
  DeploymentOutcome events appended in the same transaction. Worker
  validation admits the `deployment` queue exactly when targets are
  configured; job payload schema bumps to v3. New read endpoint GET
  /api/v1/runs/{run_id}/deployment mirrored through the OpenAPI
  inventory, zod contracts, regenerated SDK types and a run-detail
  dashboard panel.
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