# ADR-001: Overall Architecture

## Status
Accepted

## Context
Hephaestus coordinates AI agents and deterministic verification for real
software engineering work. It must run on a laptop (single-node mode)
and scale horizontally in production, without forking two codebases.

## Decision
Modular monorepo with a Rust core engine and a TypeScript web tier:

* Rust crates implement every security-sensitive or high-throughput
  subsystem: workflow engine, scheduler, policy engine, tool runtime,
  sandbox, repository intelligence, artifact/evidence stores.
* TypeScript implements the dashboard (Vue 3) and developer SDK.
* The API layer (axum) is the only component the web tier talks to.
* Subsystems communicate through explicit interfaces (traits), never by
  reaching into each other's internals.

## Consequences
* One language for all enforcement points simplifies security review.
* Rust compile times are paid once per crate boundary; interfaces stay
  small to keep the graph shallow.
* The web tier holds no authority: it renders server state and forwards
  commands; all authorization happens server-side.
