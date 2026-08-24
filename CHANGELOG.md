# Changelog

All notable changes to Hephaestus are documented here.
Format follows Keep a Changelog; versioning is Semantic Versioning
until the 1.0 API contract freezes per ADR-011.

## [Unreleased]

### Added

- Core domain model: typed UUIDv7 identifiers, structured error
  taxonomy with stable public codes, explicit workflow state machine
  (property-tested), versioned event envelopes with provenance.
- Prompt-injection defense primitives: untrusted-content framing,
  injection indicator scanning, secret redaction.
- Layered validated configuration (defaults -> TOML -> env) with
  fail-closed production checks.
- PostgreSQL persistence: initial schema migration, tenant-scoped
  stores, idempotent task/job creation, workflow runs with CAS
  transitions and atomic event writes, durable job queue with
  priorities/retries/dead-letter/scoped lease recovery, tamper-evident
  audit log with hash-chain verifier.
- Project governance docs: SECURITY.md, CONTRIBUTING.md, Code of
  Conduct, CI workflows, issue/PR templates.
